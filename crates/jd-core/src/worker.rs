//! The vault worker: ONE background thread owns all writes (spec §3).
//! Commands arrive on a channel, execute serially, results post back.
//! The UI drains events once per frame; `wake` requests a repaint.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::SystemTime;

use crate::doc::NoteDoc;
use crate::error::CoreError;
use crate::frontmatter::FrontmatterDoc;
use crate::id::{IdGen, NoteId};
use crate::index::{Index, SharedIndex};
use crate::note::{NewNote, NoteMeta, Status};
use crate::time::Timestamp;
use crate::vault::Vault;
use crate::vault::io::{atomic_save, filename_for};
use crate::vault::recovery::{clear_buffer, journal_buffer};
use crate::vault::scan::{parse_note_file, scan};
use crate::vault::trash::purge_older_than;
use crate::vault::watcher::{VaultWatcher, WatchEvent};

/// Which sub-folder to write a newly-created note into.
pub enum Dest {
    Inbox,
    Notes,
}

/// Commands the vault worker accepts.
pub enum VaultCommand {
    Create {
        seed: NewNote,
        dest: Dest,
    },
    /// Full body text; the worker manages the frontmatter.
    SaveBody {
        id: NoteId,
        content: String,
    },
    ReadBody {
        id: NoteId,
    },
    JournalBuffer {
        id: NoteId,
        content: String,
    },
    PurgeTrash {
        older_than_days: Option<u32>,
    },
    RescanAll,
    Shutdown,
}

/// Events emitted by the vault worker.
#[derive(Debug)]
pub enum VaultEvent {
    Created {
        meta: NoteMeta,
    },
    Saved {
        id: NoteId,
    },
    Body {
        id: NoteId,
        content: String,
    },
    External {
        changed: Vec<NoteId>,
        removed: Vec<NoteId>,
    },
    Conflict {
        id: NoteId,
        conflict_copy: PathBuf,
    },
    ScanProgress {
        done: usize,
        total: usize,
    },
    ScanComplete {
        quarantined_count: usize,
    },
    Error {
        context: String,
        message: String,
    },
}

/// The public handle returned by `start`.
pub struct VaultHandle {
    pub commands: mpsc::Sender<VaultCommand>,
    pub events: mpsc::Receiver<VaultEvent>,
    pub index: SharedIndex,
}

enum WorkerMsg {
    Cmd(VaultCommand),
    Watch(WatchEvent),
}

/// (len, mtime) of files we wrote — a matching Changed event is our echo.
type WriteLedger = HashMap<PathBuf, (u64, SystemTime)>;

fn stat(abs: &Path) -> Option<(u64, SystemTime)> {
    let m = abs.metadata().ok()?;
    Some((m.len(), m.modified().ok()?))
}

pub fn start(vault: Vault, wake: Box<dyn Fn() + Send + Sync>) -> Result<VaultHandle, CoreError> {
    let (cmd_tx, cmd_rx) = mpsc::channel::<VaultCommand>();
    let (msg_tx, msg_rx) = mpsc::channel::<WorkerMsg>();
    let (event_tx, event_rx) = mpsc::channel::<VaultEvent>();
    let index: SharedIndex = std::sync::Arc::new(std::sync::RwLock::new(Index::new()));

    // forwarder: public command channel → internal msg channel (decision #3)
    let fwd = msg_tx.clone();
    std::thread::Builder::new()
        .name("jd-fwd".into())
        .spawn(move || {
            while let Ok(cmd) = cmd_rx.recv() {
                let stop = matches!(cmd, VaultCommand::Shutdown);
                if fwd.send(WorkerMsg::Cmd(cmd)).is_err() || stop {
                    return;
                }
            }
        })
        .map_err(|e| {
            CoreError::Io(crate::error::IoError {
                path: "<threads>".into(),
                op: "spawn",
                source: e,
            })
        })?;

    // watcher → internal msg channel
    let (watch_tx, watch_rx) = mpsc::channel::<WatchEvent>();
    let watcher = VaultWatcher::start(&vault, watch_tx)?;
    let wfwd = msg_tx;
    std::thread::Builder::new()
        .name("jd-watch-fwd".into())
        .spawn(move || {
            while let Ok(ev) = watch_rx.recv() {
                if wfwd.send(WorkerMsg::Watch(ev)).is_err() {
                    return;
                }
            }
        })
        .map_err(|e| {
            CoreError::Io(crate::error::IoError {
                path: "<threads>".into(),
                op: "spawn",
                source: e,
            })
        })?;

    let worker_index = index.clone();
    std::thread::Builder::new()
        .name("jd-worker".into())
        .spawn(move || {
            let _watcher = watcher; // owned by the worker; dropped on exit
            let mut id_gen = IdGen::new();
            let mut ledger: WriteLedger = HashMap::new();
            let wake = std::sync::Arc::new(wake);
            let emit = {
                let wake = wake.clone();
                move |ev: VaultEvent| {
                    let _ = event_tx.send(ev);
                    wake();
                }
            };

            run_initial_scan(&vault, &worker_index, &emit);

            while let Ok(msg) = msg_rx.recv() {
                match msg {
                    WorkerMsg::Cmd(VaultCommand::Shutdown) => return,
                    WorkerMsg::Cmd(cmd) => {
                        handle_command(&vault, &worker_index, &mut id_gen, &mut ledger, &emit, cmd)
                    }
                    WorkerMsg::Watch(ev) => {
                        handle_watch(&vault, &worker_index, &mut ledger, &emit, ev)
                    }
                }
            }
        })
        .map_err(|e| {
            CoreError::Io(crate::error::IoError {
                path: "<threads>".into(),
                op: "spawn",
                source: e,
            })
        })?;

    Ok(VaultHandle {
        commands: cmd_tx,
        events: event_rx,
        index,
    })
}

/// Run the initial scan, emitting ScanProgress every 64 files and ScanComplete.
/// Populates the index under a single write lock.
fn run_initial_scan(vault: &Vault, index: &SharedIndex, emit: &impl Fn(VaultEvent)) {
    // scan's progress callback fires from worker threads (must be Sync).
    // We collect events and emit after: emit progress once at the end.
    let outcome = match scan(vault, &|_done, _total| {
        // Progress is emitted after the scan completes since emit is !Sync
    }) {
        Ok(o) => o,
        Err(e) => {
            emit(VaultEvent::Error {
                context: "initial scan".into(),
                message: e.to_string(),
            });
            return;
        }
    };

    // Emit scan progress (one shot at end — emit is not Sync so we can't call
    // it from inside scan's parallel worker threads)
    let total = outcome.metas.len() + outcome.quarantined.len();
    if total > 0 {
        emit(VaultEvent::ScanProgress { done: total, total });
    }

    // Upsert all metas under a single write lock
    {
        let mut ix = index.write().unwrap();
        for (meta, body) in &outcome.metas {
            ix.upsert(meta.clone(), body);
        }
    }

    emit(VaultEvent::ScanComplete {
        quarantined_count: outcome.quarantined.len(),
    });
}

/// Handle a single VaultCommand (not Shutdown).
fn handle_command(
    vault: &Vault,
    index: &SharedIndex,
    id_gen: &mut IdGen,
    ledger: &mut WriteLedger,
    emit: &impl Fn(VaultEvent),
    cmd: VaultCommand,
) {
    match cmd {
        VaultCommand::Create { seed, dest } => {
            let id = NoteId::generate(id_gen);
            let now = Timestamp::now();

            // Determine directory
            let dir_name = match dest {
                Dest::Inbox => "inbox",
                Dest::Notes => "notes",
            };
            let dir_abs = vault.abs(Path::new(dir_name));

            // Extract title from body: first `# ` heading, else first non-empty line
            let title = extract_note_title(&seed.body);

            // Determine filename
            let abs_path = filename_for(&title, id, &dir_abs);
            let rel_path = vault.rel(&abs_path).unwrap_or_else(|| abs_path.clone());

            // Build frontmatter
            let mut fm = FrontmatterDoc::synthesize(id, now, seed.status);
            fm.set_kind(seed.kind);
            if let Some(src) = &seed.source {
                fm.set_source(Some(src.as_str()));
            }
            if !seed.tags.is_empty() {
                fm.set_tags(&seed.tags);
            }

            // Serialize and save
            let doc = NoteDoc {
                fm,
                body: seed.body,
            };
            let content = doc.serialize();

            if let Err(e) = atomic_save(&abs_path, &content) {
                emit(VaultEvent::Error {
                    context: "create note".into(),
                    message: e.to_string(),
                });
                return;
            }

            // Record in ledger
            if let Some(s) = stat(&abs_path) {
                ledger.insert(rel_path.clone(), s);
            }

            // Parse back and upsert
            match parse_note_file(vault, &rel_path) {
                Ok((meta, body)) => {
                    index.write().unwrap().upsert(meta.clone(), &body);
                    emit(VaultEvent::Created { meta });
                }
                Err(e) => {
                    emit(VaultEvent::Error {
                        context: "create note index".into(),
                        message: e,
                    });
                }
            }
        }

        VaultCommand::SaveBody { id, content } => {
            // Look up rel path from index
            let rel_path = match index.read().unwrap().get(id) {
                Some(m) => m.rel_path.clone(),
                None => {
                    emit(VaultEvent::Error {
                        context: "save body".into(),
                        message: format!("note {id} not found in index"),
                    });
                    return;
                }
            };
            let abs_path = vault.abs(&rel_path);

            // Conflict check (decision #5): if ledger HAS an entry AND current stat ≠ ledger
            if let Some(&ledger_stat) = ledger.get(&rel_path) {
                let current_stat = stat(&abs_path);
                if current_stat != Some(ledger_stat) {
                    // Conflict: write our content to a conflict copy
                    let conflict_path = conflict_copy_path(&abs_path);
                    let conflict_rel = vault
                        .rel(&conflict_path)
                        .unwrap_or_else(|| conflict_path.clone());

                    // Build our content with synthesized frontmatter for the conflict copy.
                    // Generate a new id for the conflict copy so it shows up as a distinct note.
                    let conflict_id = NoteId::generate(id_gen);
                    let now = Timestamp::now();
                    let status = index
                        .read()
                        .unwrap()
                        .get(id)
                        .map(|m| m.status)
                        .unwrap_or(Status::Fleeting);
                    let mut conflict_fm = FrontmatterDoc::synthesize(conflict_id, now, status);
                    conflict_fm.set_modified(now);
                    let conflict_doc = NoteDoc {
                        fm: conflict_fm,
                        body: content.clone(),
                    };
                    let conflict_content = conflict_doc.serialize();

                    if let Err(e) = atomic_save(&conflict_path, &conflict_content) {
                        emit(VaultEvent::Error {
                            context: "save conflict copy".into(),
                            message: e.to_string(),
                        });
                        return;
                    }

                    // Index the conflict copy
                    if let Ok((meta, body)) = parse_note_file(vault, &conflict_rel) {
                        index.write().unwrap().upsert(meta, &body);
                    }

                    emit(VaultEvent::Conflict {
                        id,
                        conflict_copy: conflict_rel,
                    });
                    return;
                }
            }

            // Happy path: read existing file (synthesize if missing)
            let existing_content = std::fs::read_to_string(&abs_path).ok();
            let mut doc = match &existing_content {
                Some(s) => NoteDoc::parse(s),
                None => {
                    // File missing: synthesize frontmatter with current id
                    let now = Timestamp::now();
                    let status = index
                        .read()
                        .unwrap()
                        .get(id)
                        .map(|m| m.status)
                        .unwrap_or(Status::Fleeting);
                    let fm = FrontmatterDoc::synthesize(id, now, status);
                    NoteDoc {
                        fm,
                        body: String::new(),
                    }
                }
            };

            // If frontmatter is empty, synthesize with the note's current id (decision #1)
            if doc.fm.id().is_none() && doc.fm.serialize().is_empty() {
                let now = Timestamp::now();
                let status = index
                    .read()
                    .unwrap()
                    .get(id)
                    .map(|m| m.status)
                    .unwrap_or(Status::Fleeting);
                doc.fm = FrontmatterDoc::synthesize(id, now, status);
            }

            // Replace body, update modified
            doc.body = content;
            doc.fm.set_modified(Timestamp::now());
            let new_content = doc.serialize();

            if let Err(e) = atomic_save(&abs_path, &new_content) {
                emit(VaultEvent::Error {
                    context: "save body".into(),
                    message: e.to_string(),
                });
                return;
            }

            // Update ledger
            if let Some(s) = stat(&abs_path) {
                ledger.insert(rel_path.clone(), s);
            }

            // Re-parse and upsert
            match parse_note_file(vault, &rel_path) {
                Ok((meta, body)) => {
                    index.write().unwrap().upsert(meta, &body);
                }
                Err(e) => {
                    emit(VaultEvent::Error {
                        context: "save body index".into(),
                        message: e,
                    });
                }
            }

            // Clear recovery buffer
            clear_buffer(vault, id);

            emit(VaultEvent::Saved { id });
        }

        VaultCommand::ReadBody { id } => {
            let rel_path = match index.read().unwrap().get(id) {
                Some(m) => m.rel_path.clone(),
                None => {
                    emit(VaultEvent::Error {
                        context: "read body".into(),
                        message: format!("note {id} not found in index"),
                    });
                    return;
                }
            };
            let abs_path = vault.abs(&rel_path);
            match std::fs::read_to_string(&abs_path) {
                Ok(s) => {
                    let doc = NoteDoc::parse(&s);
                    emit(VaultEvent::Body {
                        id,
                        content: doc.body,
                    });
                }
                Err(e) => {
                    emit(VaultEvent::Error {
                        context: "read body".into(),
                        message: format!("couldn't read '{}': {e}", rel_path.display()),
                    });
                }
            }
        }

        VaultCommand::JournalBuffer { id, content } => {
            if let Err(e) = journal_buffer(vault, id, &content) {
                emit(VaultEvent::Error {
                    context: "journal buffer".into(),
                    message: e.to_string(),
                });
            }
            // No event on success per the spec
        }

        VaultCommand::PurgeTrash { older_than_days } => {
            if let Err(e) = purge_older_than(vault, older_than_days) {
                emit(VaultEvent::Error {
                    context: "purge trash".into(),
                    message: e.to_string(),
                });
            }
            // No event on success per the spec
        }

        VaultCommand::RescanAll => {
            // Clear the index
            *index.write().unwrap() = Index::new();
            run_initial_scan(vault, index, emit);
        }

        VaultCommand::Shutdown => {
            // Handled in the outer loop; shouldn't reach here
        }
    }
}

/// Handle a watch event from the file system watcher.
fn handle_watch(
    vault: &Vault,
    index: &SharedIndex,
    ledger: &mut WriteLedger,
    emit: &impl Fn(VaultEvent),
    ev: WatchEvent,
) {
    match ev {
        WatchEvent::Changed(rel) => {
            let abs = vault.abs(&rel);
            // Echo suppression (decision #4): if the ledger entry matches current stat, drop
            if let Some(&ledger_stat) = ledger.get(&rel)
                && stat(&abs) == Some(ledger_stat)
            {
                return; // our own write, suppress
            }

            // External change: re-parse and upsert
            match parse_note_file(vault, &rel) {
                Ok((mut meta, body)) => {
                    // Look up the previous id for this path in the index.
                    let prev_id = index
                        .read()
                        .unwrap()
                        .iter_meta()
                        .find_map(|m| if m.rel_path == rel { Some(m.id) } else { None });

                    let emit_id;
                    if let Some(prev) = prev_id {
                        if meta.id != prev {
                            // The file lost its frontmatter id (e.g. external overwrite).
                            // Preserve identity: reuse the existing id, remove the old entry
                            // and re-insert under that id.
                            index.write().unwrap().remove(prev);
                            meta.id = prev;
                        }
                        emit_id = prev;
                    } else {
                        emit_id = meta.id;
                    }

                    index.write().unwrap().upsert(meta, &body);
                    emit(VaultEvent::External {
                        changed: vec![emit_id],
                        removed: vec![],
                    });
                }
                Err(e) => {
                    emit(VaultEvent::Error {
                        context: "watch changed".into(),
                        message: e,
                    });
                }
            }
        }

        WatchEvent::Removed(rel) => {
            // Find the note in the index by rel_path
            let id = index
                .read()
                .unwrap()
                .iter_meta()
                .find_map(|m| if m.rel_path == rel { Some(m.id) } else { None });
            if let Some(id) = id {
                index.write().unwrap().remove(id);
                ledger.remove(&rel);
                emit(VaultEvent::External {
                    changed: vec![],
                    removed: vec![id],
                });
            }
        }

        WatchEvent::Renamed { from, to } => {
            // Treat as Removed(from) + Changed(to)
            handle_watch(vault, index, ledger, emit, WatchEvent::Removed(from));
            handle_watch(vault, index, ledger, emit, WatchEvent::Changed(to));
        }
    }
}

/// Extract a display title from the body: first `# ` heading, else first non-empty line.
fn extract_note_title(body: &str) -> String {
    for line in body.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("# ") {
            let title = rest.trim();
            if !title.is_empty() {
                return title.to_owned();
            }
        }
    }
    // Fallback: first non-empty line, stripped of leading #
    for line in body.lines() {
        let trimmed = line.trim().trim_start_matches('#').trim();
        if !trimmed.is_empty() {
            return trimmed.to_owned();
        }
    }
    "Untitled".to_owned()
}

/// Generate the conflict copy path: `<stem> (conflict YYYY-MM-DD HHMM).md`
fn conflict_copy_path(abs_path: &Path) -> PathBuf {
    let now = Timestamp::now();
    // Format as YYYY-MM-DD HHMM from the rfc3339 string
    let rfc = now.to_rfc3339();
    // "2026-07-06T10:22:00Z" → "2026-07-06 1022"
    let date_part = &rfc[..10]; // "2026-07-06"
    let time_part = rfc[11..16].replace(':', ""); // "10:22" → "1022"
    let conflict_tag = format!("{date_part} {time_part}");

    let dir = abs_path.parent().unwrap_or_else(|| Path::new("."));
    let stem = abs_path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("Untitled");
    dir.join(format!("{stem} (conflict {conflict_tag}).md"))
}
