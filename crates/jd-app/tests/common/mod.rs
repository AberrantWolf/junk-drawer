//! Shared test helpers. `tempfile` is a rejected dependency (Appendix B);
//! this is the ~25-line in-house version.
#![allow(dead_code)] // not every test file uses every helper

use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};

pub struct TempDir(pub PathBuf);

impl TempDir {
    pub fn new() -> TempDir {
        static N: AtomicU32 = AtomicU32::new(0);
        let p = std::env::temp_dir().join(format!(
            "jd-app-test-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&p).unwrap();
        TempDir(p)
    }

    pub fn path(&self) -> &std::path::Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Step the harness until `pred` returns true or `max_frames` elapse.
/// Panics with `what` on exhaustion. Use this instead of sleeping: worker
/// events arrive between frames, and `wake` requests repaints.
pub fn pump<S>(
    harness: &mut egui_kittest::Harness<'_, S>,
    pred: &mut dyn FnMut(&S) -> bool,
    max_frames: usize,
    what: &str,
) {
    for _ in 0..max_frames {
        if pred(harness.state()) {
            return;
        }
        harness.step();
        std::thread::sleep(std::time::Duration::from_millis(5)); // yield to worker thread only
    }
    panic!("pump: gave up waiting for {what}");
}

/// A minimal vault on disk: inbox/, notes/ (Vault::open creates structure).
pub fn temp_vault() -> TempDir {
    TempDir::new()
}

/// Construct a `NewNote` seed for use in `VaultOp::Create`.
pub fn new_note(title: &str, body: &str) -> jd_core::note::NewNote {
    jd_core::note::NewNote {
        body: format!("# {title}\n{body}"),
        status: jd_core::note::Status::Permanent,
        kind: jd_core::note::Kind::Note,
        source: None,
        tags: Vec::new(),
    }
}
