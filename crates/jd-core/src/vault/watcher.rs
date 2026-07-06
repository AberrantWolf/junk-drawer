//! notify wrapper: raw FS events → 200 ms debounce → coalesced,
//! existence-based WatchEvents (decision #2). The `.md`-under-inbox/notes
//! filter lives HERE so consumers never see machine-state noise.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use notify::{RecursiveMode, Watcher};

use crate::error::WatchError;
use crate::vault::Vault;
use crate::vault::io::is_our_tempfile;

const DEBOUNCE: Duration = Duration::from_millis(200);

#[derive(Clone, Debug, PartialEq)]
pub enum WatchEvent {
    Changed(PathBuf),
    Removed(PathBuf),
    /// Best-effort (decision #2): consumers must also handle a rename arriving
    /// as Removed(from) + Changed(to).
    Renamed {
        from: PathBuf,
        to: PathBuf,
    },
}

pub struct VaultWatcher {
    // keep the notify watcher alive; drop = stop
    _watcher: notify::RecommendedWatcher,
}

/// True if this rel path is a note we care about.
fn is_note_path(rel: &Path) -> bool {
    let under_note_dirs = rel.starts_with("inbox") || rel.starts_with("notes");
    let name = rel
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    under_note_dirs
        && rel.extension().is_some_and(|e| e == "md")
        && !name.starts_with('.')
        && !is_our_tempfile(&name)
}

impl VaultWatcher {
    pub fn start(vault: &Vault, tx: mpsc::Sender<WatchEvent>) -> Result<VaultWatcher, WatchError> {
        let (raw_tx, raw_rx) = mpsc::channel::<PathBuf>();
        // Canonicalize so strip_prefix works even when the OS returns
        // symlink-resolved paths (e.g. macOS FSEvents: /tmp → /private/tmp).
        let root = vault
            .root()
            .canonicalize()
            .unwrap_or_else(|_| vault.root().to_owned());

        let mut watcher =
            notify::recommended_watcher(move |res: Result<notify::Event, notify::Error>| {
                if let Ok(event) = res {
                    for path in event.paths {
                        let _ = raw_tx.send(path);
                    }
                }
            })
            .map_err(|e| WatchError::Init(e.to_string()))?;
        watcher
            .watch(&root, RecursiveMode::Recursive)
            .map_err(|e| WatchError::Init(e.to_string()))?;

        // Debouncer thread: collect touched paths; after DEBOUNCE of quiet,
        // flush each as Changed/Removed by existence.
        std::thread::Builder::new()
            .name("jd-debounce".into())
            .spawn(move || {
                let mut pending: HashMap<PathBuf, Instant> = HashMap::new();
                loop {
                    let timeout = if pending.is_empty() {
                        Duration::from_secs(3600)
                    } else {
                        Duration::from_millis(50)
                    };
                    match raw_rx.recv_timeout(timeout) {
                        Ok(abs) => {
                            pending.insert(abs, Instant::now());
                        }
                        Err(mpsc::RecvTimeoutError::Timeout) => {}
                        Err(mpsc::RecvTimeoutError::Disconnected) => return,
                    }
                    let now = Instant::now();
                    let ready: Vec<PathBuf> = pending
                        .iter()
                        .filter(|(_, t)| now.duration_since(**t) >= DEBOUNCE)
                        .map(|(p, _)| p.clone())
                        .collect();
                    for abs in ready {
                        pending.remove(&abs);
                        let Some(rel) = abs.strip_prefix(&root).ok().map(Path::to_owned) else {
                            continue;
                        };
                        if !is_note_path(&rel) {
                            continue;
                        }
                        let event = if abs.exists() {
                            WatchEvent::Changed(rel)
                        } else {
                            WatchEvent::Removed(rel)
                        };
                        if tx.send(event).is_err() {
                            return; // consumer gone
                        }
                    }
                }
            })
            .map_err(|e| WatchError::Init(e.to_string()))?;

        Ok(VaultWatcher { _watcher: watcher })
    }
}
