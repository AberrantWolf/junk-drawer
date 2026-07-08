//! The vault worker: ONE background thread owns all writes (spec §3).
//! Commands arrive on a channel, execute serially, results post back.
//! The UI drains events once per frame; `wake` requests a repaint.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, mpsc};
use std::time::SystemTime;

use crate::command::{Dest, OpResult, OpSource, VaultOp, label_display, op_label};
use crate::doc::{NoteDoc, extract_links, extract_title};
use crate::error::CoreError;
use crate::frontmatter::FrontmatterDoc;
use crate::id::{IdGen, NoteId};
use crate::index::{Index, SharedIndex};
use crate::note::Status;
use crate::time::Timestamp;
use crate::vault::Vault;
use crate::vault::io::{atomic_save, filename_for};
use crate::vault::recovery::{clear_buffer, journal_buffer};
use crate::vault::scan::{QuarantinedFile, parse_note_file, scan};
use crate::vault::trash::{purge_older_than, restore, trash_note};
use crate::vault::watcher::{VaultWatcher, WatchEvent};

/// Commands the vault worker accepts.
pub enum VaultCommand {
    Op { op: VaultOp, source: OpSource },
    ReadBody { id: NoteId },
    JournalBuffer { id: NoteId, content: String },
    PurgeTrash { older_than_days: Option<u32> },
    RescanAll,
    Shutdown,
}

/// Events emitted by the vault worker.
#[derive(Debug, Clone)]
pub enum VaultEvent {
    OpDone {
        result: OpResult,
        source: OpSource,
    },
    OpFailed {
        label: String,
        message: String,
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
        /// Files the scan could not read (WP4: the list itself rides the
        /// event; a count derives via `.len()`).
        quarantined: Vec<QuarantinedFile>,
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
            let event_tx_for_scan = Mutex::new(event_tx.clone());
            let emit = {
                let wake = wake.clone();
                move |ev: VaultEvent| {
                    let _ = event_tx.send(ev);
                    wake();
                }
            };

            run_initial_scan(&vault, &worker_index, &event_tx_for_scan, &*wake, &emit);

            while let Ok(msg) = msg_rx.recv() {
                match msg {
                    WorkerMsg::Cmd(VaultCommand::Shutdown) => return,
                    WorkerMsg::Cmd(cmd) => handle_command(
                        &vault,
                        &worker_index,
                        &mut id_gen,
                        &mut ledger,
                        &event_tx_for_scan,
                        &*wake,
                        &emit,
                        cmd,
                    ),
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
///
/// `event_tx` is wrapped in a Mutex so the progress callback (called from
/// parallel scan threads that require Sync) can send through it.  `wake` is
/// called after each send so the UI repaints promptly.  `emit` handles the
/// post-scan ScanComplete event (and any scan error) on the worker thread.
fn run_initial_scan(
    vault: &Vault,
    index: &SharedIndex,
    event_tx: &Mutex<mpsc::Sender<VaultEvent>>,
    wake: &(dyn Fn() + Sync),
    emit: &impl Fn(VaultEvent),
) {
    let outcome = match scan(vault, &|done, total| {
        // Throttle: emit when done % 64 == 0 or at the very end.
        if done % 64 == 0 || done == total {
            let _ = event_tx
                .lock()
                .unwrap()
                .send(VaultEvent::ScanProgress { done, total });
            wake();
        }
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

    // Upsert all metas under a single write lock
    {
        let mut ix = index.write().unwrap();
        for (meta, body) in &outcome.metas {
            ix.upsert(meta.clone(), body);
        }
        // One O(total terms) pass so similar() serves from the norm cache.
        // Per-upsert refresh would blow the incremental reindex budget.
        ix.refresh_similarity_cache();
    }

    emit(VaultEvent::ScanComplete {
        quarantined: outcome.quarantined,
    });
}

/// Execute a single VaultOp, returning an OpResult on success or an error
/// tuple (label, message) on failure.  The `emit` callback is used only for Conflict events
/// (SaveBody conflict path succeeds with divergence: OpFailed is NOT emitted).
/// For Batch, the error carries the failing member's label.
fn execute_op(
    vault: &Vault,
    index: &SharedIndex,
    id_gen: &mut IdGen,
    ledger: &mut WriteLedger,
    emit: &impl Fn(VaultEvent),
    op: VaultOp,
) -> Result<OpResult, (String, String)> {
    match op {
        VaultOp::Create { seed, dest } => {
            let id = NoteId::generate(id_gen);
            let now = Timestamp::now();

            let dir_name = match dest {
                Dest::Inbox => "inbox",
                Dest::Notes => "notes",
            };
            let dir_abs = vault.abs(Path::new(dir_name));

            let title = extract_note_title(&seed.body);
            let abs_path = filename_for(&title, id, &dir_abs);
            let rel_path = vault.rel(&abs_path).unwrap_or_else(|| abs_path.clone());

            let mut fm = FrontmatterDoc::synthesize(id, now, seed.status);
            fm.set_kind(seed.kind);
            if let Some(src) = &seed.source {
                fm.set_source(Some(src.as_str()));
            }
            if !seed.tags.is_empty() {
                fm.set_tags(&seed.tags);
            }

            let doc = NoteDoc {
                fm,
                body: seed.body.clone(),
            };
            let content = doc.serialize();

            let display = label_display(&title);
            atomic_save(&abs_path, &content)
                .map_err(|e| (op_label("Create", false, &display), format!("create: {e}")))?;

            if let Some(s) = stat(&abs_path) {
                ledger.insert(rel_path.clone(), s);
            }

            match parse_note_file(vault, &rel_path) {
                Ok((meta, body)) => {
                    let is_fleeting = meta.status == Status::Fleeting;
                    index.write().unwrap().upsert(meta, &body);
                    Ok(OpResult {
                        inverse: Some(VaultOp::Delete { id }),
                        label: op_label("Create", is_fleeting, &display),
                        created: vec![id],
                    })
                }
                Err(e) => Err((
                    op_label("Create", false, &display),
                    format!("create index: {e}"),
                )),
            }
        }

        VaultOp::SaveBody { id, content } => {
            let meta = index.read().unwrap().get(id).cloned().ok_or_else(|| {
                let label = "Edit note".to_string();
                (label, format!("note {id} not found in index"))
            })?;
            let rel_path = meta.rel_path.clone();
            let abs_path = vault.abs(&rel_path);
            let is_fleeting = meta.status == Status::Fleeting;
            let display_title = meta
                .title
                .clone()
                .unwrap_or_else(|| meta.first_line.clone());

            // Read old body for the inverse
            let old_body = match std::fs::read_to_string(&abs_path) {
                Ok(s) => NoteDoc::parse(&s).body,
                Err(_) => String::new(),
            };

            let label = op_label("Edit", is_fleeting, &label_display(&display_title));

            // Conflict check (decision #5): if ledger HAS an entry AND current stat ≠ ledger
            if let Some(&ledger_stat) = ledger.get(&rel_path) {
                let current_stat = stat(&abs_path);
                if current_stat != Some(ledger_stat) {
                    let conflict_id = NoteId::generate(id_gen);
                    let conflict_path = conflict_copy_path(&abs_path, conflict_id);
                    let conflict_rel = vault
                        .rel(&conflict_path)
                        .unwrap_or_else(|| conflict_path.clone());
                    let now = Timestamp::now();
                    let status = meta.status;
                    let mut conflict_fm = FrontmatterDoc::synthesize(conflict_id, now, status);
                    conflict_fm.set_modified(now);
                    let conflict_doc = NoteDoc {
                        fm: conflict_fm,
                        body: content.clone(),
                    };
                    let conflict_content = conflict_doc.serialize();

                    if let Err(e) = atomic_save(&conflict_path, &conflict_content) {
                        return Err((label, format!("save conflict copy: {e}")));
                    }

                    if let Ok((cmeta, cbody)) = parse_note_file(vault, &conflict_rel) {
                        index.write().unwrap().upsert(cmeta, &cbody);
                    }

                    emit(VaultEvent::Conflict {
                        id,
                        conflict_copy: conflict_rel,
                    });

                    // Op "succeeds with divergence" — return Ok with old-body inverse
                    return Ok(OpResult {
                        inverse: Some(VaultOp::SaveBody {
                            id,
                            content: old_body,
                        }),
                        label,
                        created: vec![],
                    });
                }
            }

            // Happy path: read existing file (synthesize if missing)
            let existing_content = std::fs::read_to_string(&abs_path).ok();
            let mut doc = match &existing_content {
                Some(s) => NoteDoc::parse(s),
                None => {
                    let now = Timestamp::now();
                    let status = meta.status;
                    let fm = FrontmatterDoc::synthesize(id, now, status);
                    NoteDoc {
                        fm,
                        body: String::new(),
                    }
                }
            };

            if doc.fm.id().is_none() {
                if doc.fm.serialize().is_empty() {
                    let now = Timestamp::now();
                    let status = meta.status;
                    doc.fm = FrontmatterDoc::synthesize(id, now, status);
                } else {
                    doc.fm.set_id(id);
                }
            }

            doc.body = content;
            doc.fm.set_modified(Timestamp::now());
            let new_content = doc.serialize();

            atomic_save(&abs_path, &new_content)
                .map_err(|e| (label.clone(), format!("save body: {e}")))?;

            if let Some(s) = stat(&abs_path) {
                ledger.insert(rel_path.clone(), s);
            }

            match parse_note_file(vault, &rel_path) {
                Ok((m, b)) => {
                    index.write().unwrap().upsert(m, &b);
                }
                Err(e) => {
                    return Err((label, format!("save body index: {e}")));
                }
            }

            clear_buffer(vault, id);

            Ok(OpResult {
                inverse: Some(VaultOp::SaveBody {
                    id,
                    content: old_body,
                }),
                label,
                created: vec![],
            })
        }

        VaultOp::Toss { id } => {
            let meta = index.read().unwrap().get(id).cloned().ok_or_else(|| {
                let label = "Toss note".to_string();
                (label, format!("note {id} not found in index"))
            })?;
            let is_fleeting = meta.status == Status::Fleeting;
            let display = meta
                .title
                .clone()
                .unwrap_or_else(|| meta.first_line.clone());
            let rel_path = meta.rel_path.clone();
            let label = op_label("Toss", is_fleeting, &label_display(&display));

            trash_note(vault, &meta).map_err(|e| (label.clone(), e.to_string()))?;
            ledger.remove(&rel_path);
            index.write().unwrap().remove(id);

            Ok(OpResult {
                inverse: Some(VaultOp::Restore { id }),
                label,
                created: vec![],
            })
        }

        VaultOp::Delete { id } => {
            let meta = index.read().unwrap().get(id).cloned().ok_or_else(|| {
                let label = "Delete note".to_string();
                (label, format!("note {id} not found in index"))
            })?;
            let is_fleeting = meta.status == Status::Fleeting;
            let display = meta
                .title
                .clone()
                .unwrap_or_else(|| meta.first_line.clone());
            let rel_path = meta.rel_path.clone();
            let label = op_label("Delete", is_fleeting, &label_display(&display));

            trash_note(vault, &meta).map_err(|e| (label.clone(), e.to_string()))?;
            ledger.remove(&rel_path);
            index.write().unwrap().remove(id);

            Ok(OpResult {
                inverse: Some(VaultOp::Restore { id }),
                label,
                created: vec![],
            })
        }

        VaultOp::Restore { id } => {
            let new_rel = restore(vault, id).map_err(|e| {
                let label = "Restore note".to_string();
                (label, e.to_string())
            })?;

            match parse_note_file(vault, &new_rel) {
                Ok((meta, body)) => {
                    let is_fleeting = meta.status == Status::Fleeting;
                    let display = meta
                        .title
                        .clone()
                        .unwrap_or_else(|| meta.first_line.clone());
                    index.write().unwrap().upsert(meta, &body);
                    // Update ledger for the restored file
                    let abs = vault.abs(&new_rel);
                    if let Some(s) = stat(&abs) {
                        ledger.insert(new_rel, s);
                    }
                    Ok(OpResult {
                        inverse: Some(VaultOp::Toss { id }),
                        label: op_label("Restore", is_fleeting, &label_display(&display)),
                        created: vec![],
                    })
                }
                Err(e) => Err(("Restore note".to_string(), format!("restore index: {e}"))),
            }
        }

        VaultOp::Promote { id } => {
            let meta = index.read().unwrap().get(id).cloned().ok_or_else(|| {
                let label = "Promote note".to_string();
                (label, format!("note {id} not found in index"))
            })?;
            let old_rel = meta.rel_path.clone();
            let old_abs = vault.abs(&old_rel);
            let display = meta
                .title
                .clone()
                .unwrap_or_else(|| meta.first_line.clone());

            let content = std::fs::read_to_string(&old_abs)
                .map_err(|e| ("Promote note".to_string(), format!("promote read: {e}")))?;
            let mut doc = NoteDoc::parse(&content);
            doc.fm.set_status(Status::Permanent);
            doc.fm.set_modified(Timestamp::now());
            let new_content = doc.serialize();

            let notes_dir = vault.abs(Path::new("notes"));
            let new_abs = filename_for(&display, id, &notes_dir);
            let new_rel = vault.rel(&new_abs).unwrap_or_else(|| new_abs.clone());

            atomic_save(&new_abs, &new_content)
                .map_err(|e| ("Promote note".to_string(), format!("promote write: {e}")))?;
            std::fs::remove_file(&old_abs).map_err(|e| {
                (
                    "Promote note".to_string(),
                    format!("promote remove old: {e}"),
                )
            })?;

            ledger.remove(&old_rel);
            if let Some(s) = stat(&new_abs) {
                ledger.insert(new_rel.clone(), s);
            }

            match parse_note_file(vault, &new_rel) {
                Ok((m, b)) => {
                    // Collapse into single write-lock scope
                    {
                        let mut ix = index.write().unwrap();
                        ix.remove(id);
                        ix.upsert(m, &b);
                    }
                    Ok(OpResult {
                        inverse: Some(VaultOp::Demote { id }),
                        label: op_label("Promote", true, &label_display(&display)),
                        created: vec![],
                    })
                }
                Err(e) => Err(("Promote note".to_string(), format!("promote index: {e}"))),
            }
        }

        VaultOp::Demote { id } => {
            let meta = index.read().unwrap().get(id).cloned().ok_or_else(|| {
                let label = "Demote note".to_string();
                (label, format!("note {id} not found in index"))
            })?;
            let old_rel = meta.rel_path.clone();
            let old_abs = vault.abs(&old_rel);
            let display = meta
                .title
                .clone()
                .unwrap_or_else(|| meta.first_line.clone());

            let content = std::fs::read_to_string(&old_abs)
                .map_err(|e| ("Demote note".to_string(), format!("demote read: {e}")))?;
            let mut doc = NoteDoc::parse(&content);
            doc.fm.set_status(Status::Fleeting);
            doc.fm.set_modified(Timestamp::now());
            let new_content = doc.serialize();

            let inbox_dir = vault.abs(Path::new("inbox"));
            let new_abs = filename_for(&display, id, &inbox_dir);
            let new_rel = vault.rel(&new_abs).unwrap_or_else(|| new_abs.clone());

            atomic_save(&new_abs, &new_content)
                .map_err(|e| ("Demote note".to_string(), format!("demote write: {e}")))?;
            std::fs::remove_file(&old_abs)
                .map_err(|e| ("Demote note".to_string(), format!("demote remove old: {e}")))?;

            ledger.remove(&old_rel);
            if let Some(s) = stat(&new_abs) {
                ledger.insert(new_rel.clone(), s);
            }

            match parse_note_file(vault, &new_rel) {
                Ok((m, b)) => {
                    // Collapse into single write-lock scope
                    {
                        let mut ix = index.write().unwrap();
                        ix.remove(id);
                        ix.upsert(m, &b);
                    }
                    Ok(OpResult {
                        inverse: Some(VaultOp::Promote { id }),
                        label: op_label("Demote", false, &label_display(&display)),
                        created: vec![],
                    })
                }
                Err(e) => Err(("Demote note".to_string(), format!("demote index: {e}"))),
            }
        }

        VaultOp::SetKind { id, kind } => {
            let meta = index.read().unwrap().get(id).cloned().ok_or_else(|| {
                let label = "Edit note".to_string();
                (label, format!("note {id} not found in index"))
            })?;
            let old_kind = meta.kind;
            let is_fleeting = meta.status == Status::Fleeting;
            let display = meta
                .title
                .clone()
                .unwrap_or_else(|| meta.first_line.clone());
            let rel_path = meta.rel_path.clone();
            let abs_path = vault.abs(&rel_path);

            let label = op_label("Edit", is_fleeting, &label_display(&display));
            let content = std::fs::read_to_string(&abs_path)
                .map_err(|e| (label.clone(), format!("set_kind read: {e}")))?;
            let mut doc = NoteDoc::parse(&content);
            doc.fm.set_kind(kind);
            doc.fm.set_modified(Timestamp::now());
            let new_content = doc.serialize();

            atomic_save(&abs_path, &new_content)
                .map_err(|e| (label.clone(), format!("set_kind write: {e}")))?;

            if let Some(s) = stat(&abs_path) {
                ledger.insert(rel_path.clone(), s);
            }

            match parse_note_file(vault, &rel_path) {
                Ok((m, b)) => {
                    index.write().unwrap().upsert(m, &b);
                    Ok(OpResult {
                        inverse: Some(VaultOp::SetKind { id, kind: old_kind }),
                        label,
                        created: vec![],
                    })
                }
                Err(e) => Err((label, format!("set_kind index: {e}"))),
            }
        }

        VaultOp::SetSource { id, source } => {
            let meta = index.read().unwrap().get(id).cloned().ok_or_else(|| {
                let label = "Edit note".to_string();
                (label, format!("note {id} not found in index"))
            })?;
            let old_source = meta.source.clone();
            let is_fleeting = meta.status == Status::Fleeting;
            let display = meta
                .title
                .clone()
                .unwrap_or_else(|| meta.first_line.clone());
            let rel_path = meta.rel_path.clone();
            let abs_path = vault.abs(&rel_path);

            let label = op_label("Edit", is_fleeting, &label_display(&display));
            let content = std::fs::read_to_string(&abs_path)
                .map_err(|e| (label.clone(), format!("set_source read: {e}")))?;
            let mut doc = NoteDoc::parse(&content);
            doc.fm.set_source(source.as_deref());
            doc.fm.set_modified(Timestamp::now());
            let new_content = doc.serialize();

            atomic_save(&abs_path, &new_content)
                .map_err(|e| (label.clone(), format!("set_source write: {e}")))?;

            if let Some(s) = stat(&abs_path) {
                ledger.insert(rel_path.clone(), s);
            }

            match parse_note_file(vault, &rel_path) {
                Ok((m, b)) => {
                    index.write().unwrap().upsert(m, &b);
                    Ok(OpResult {
                        inverse: Some(VaultOp::SetSource {
                            id,
                            source: old_source,
                        }),
                        label,
                        created: vec![],
                    })
                }
                Err(e) => Err((label, format!("set_source index: {e}"))),
            }
        }

        VaultOp::SetTags { id, tags } => {
            let meta = index.read().unwrap().get(id).cloned().ok_or_else(|| {
                let label = "Edit note".to_string();
                (label, format!("note {id} not found in index"))
            })?;
            let old_tags: Vec<_> = meta.tags.iter().cloned().collect();
            let is_fleeting = meta.status == Status::Fleeting;
            let display = meta
                .title
                .clone()
                .unwrap_or_else(|| meta.first_line.clone());
            let rel_path = meta.rel_path.clone();
            let abs_path = vault.abs(&rel_path);

            let label = op_label("Edit", is_fleeting, &label_display(&display));
            let content = std::fs::read_to_string(&abs_path)
                .map_err(|e| (label.clone(), format!("set_tags read: {e}")))?;
            let mut doc = NoteDoc::parse(&content);
            doc.fm.set_tags(&tags);
            doc.fm.set_modified(Timestamp::now());
            let new_content = doc.serialize();

            atomic_save(&abs_path, &new_content)
                .map_err(|e| (label.clone(), format!("set_tags write: {e}")))?;

            if let Some(s) = stat(&abs_path) {
                ledger.insert(rel_path.clone(), s);
            }

            match parse_note_file(vault, &rel_path) {
                Ok((m, b)) => {
                    index.write().unwrap().upsert(m, &b);
                    Ok(OpResult {
                        inverse: Some(VaultOp::SetTags { id, tags: old_tags }),
                        label,
                        created: vec![],
                    })
                }
                Err(e) => Err((label, format!("set_tags index: {e}"))),
            }
        }

        VaultOp::RenameTitle { id, new_title } => {
            let meta = index.read().unwrap().get(id).cloned().ok_or_else(|| {
                (
                    "Rename card".to_string(),
                    format!("note {id} not found in index"),
                )
            })?;

            let old_title = meta.title.clone().ok_or_else(|| {
                let display = label_display(&meta.first_line);
                (
                    op_label("Rename", meta.status == Status::Fleeting, &display),
                    "cannot rename an untitled note".to_string(),
                )
            })?;

            let is_fleeting = meta.status == Status::Fleeting;
            let display = label_display(&old_title);
            let label = op_label("Rename", is_fleeting, &display);

            let old_rel = meta.rel_path.clone();
            let old_abs = vault.abs(&old_rel);
            let old_dir = old_abs
                .parent()
                .unwrap_or_else(|| Path::new("."))
                .to_owned();

            // Read the file and rewrite the heading line
            let content = std::fs::read_to_string(&old_abs)
                .map_err(|e| (label.clone(), format!("rename read: {e}")))?;
            let doc = NoteDoc::parse(&content);
            let body = doc.body.clone();

            let new_body = if let Some((_title_text, span)) = extract_title(&body) {
                // Replace just the title text within the heading line
                let mut b = body.clone();
                b.replace_range(span, &new_title);
                b
            } else {
                // No heading — should have been caught above, but handle gracefully
                return Err((label, "no heading found to rename".to_string()));
            };

            // Determine new file path
            let new_abs = filename_for(&new_title, id, &old_dir);
            let new_rel = vault.rel(&new_abs).unwrap_or_else(|| new_abs.clone());

            // Write new content to new path — reuse the fm from the first parse (no second parse)
            let mut new_doc = NoteDoc {
                fm: doc.fm.clone(),
                body: new_body,
            };
            new_doc.fm.set_modified(Timestamp::now());
            let new_content = new_doc.serialize();

            atomic_save(&new_abs, &new_content)
                .map_err(|e| (label.clone(), format!("rename write: {e}")))?;

            // Remove old file if path changed
            if old_abs != new_abs {
                std::fs::remove_file(&old_abs)
                    .map_err(|e| (label.clone(), format!("rename remove old: {e}")))?;
            }

            // Ledger: remove old, add new
            ledger.remove(&old_rel);
            if let Some(s) = stat(&new_abs) {
                ledger.insert(new_rel.clone(), s);
            }

            // Capture referrers BEFORE modifying the index (backlinks will be cleared by remove)
            let referrers = index.read().unwrap().backlinks(id);
            let old_title_lower = old_title.to_lowercase();

            // Re-upsert self in index
            match parse_note_file(vault, &new_rel) {
                Ok((new_meta, new_body_parsed)) => {
                    let mut ix = index.write().unwrap();
                    ix.remove(id);
                    ix.upsert(new_meta, &new_body_parsed);
                }
                Err(e) => return Err((label, format!("rename index self: {e}"))),
            }

            // Track already-rewritten referrers for rollback on mid-loop failure:
            // (ref_abs, ref_rel, original_full_content)
            let mut rewritten_referrers: Vec<(std::path::PathBuf, std::path::PathBuf, String)> =
                Vec::new();

            for ref_id in referrers {
                let ref_meta = match index.read().unwrap().get(ref_id).cloned() {
                    Some(m) => m,
                    None => continue,
                };
                let ref_rel = ref_meta.rel_path.clone();
                let ref_abs = vault.abs(&ref_rel);

                let ref_content = match std::fs::read_to_string(&ref_abs) {
                    Ok(s) => s,
                    Err(_) => continue,
                };
                let ref_doc = NoteDoc::parse(&ref_content);
                let ref_body = ref_doc.body.clone();

                // Rewrite [[old_title]] → [[new_title]] and [[old_title|x]] → [[new_title|x]]
                // We do this by rebuilding the body, replacing link spans from right to left
                // to preserve byte offsets.
                let links = extract_links(&ref_body);
                let mut new_ref_body = ref_body.clone();
                // Process links in reverse order to preserve byte offsets
                let mut matching_links: Vec<_> = links
                    .iter()
                    .filter(|l| l.target.to_lowercase() == old_title_lower)
                    .collect();
                matching_links.sort_by_key(|l| std::cmp::Reverse(l.span.start));

                for link in matching_links {
                    let replacement = match &link.display {
                        Some(d) => format!("[[{}|{}]]", new_title, d),
                        None => format!("[[{}]]", new_title),
                    };
                    new_ref_body.replace_range(link.span.clone(), &replacement);
                }

                if new_ref_body == ref_body {
                    continue; // nothing changed
                }

                let mut new_ref_doc = NoteDoc::parse(&ref_content);
                new_ref_doc.body = new_ref_body;
                new_ref_doc.fm.set_modified(Timestamp::now());
                let new_ref_content = new_ref_doc.serialize();

                if let Err(e) = atomic_save(&ref_abs, &new_ref_content) {
                    // Mid-loop failure: roll back already-rewritten referrers and
                    // the self-rename, surfacing any rollback failures as Error events.
                    let mut rollback_failed: Vec<String> = Vec::new();

                    // Roll back already-written referrers (most recent first)
                    for (rb_abs, rb_rel, orig_content) in rewritten_referrers.into_iter().rev() {
                        if let Err(rb_e) = atomic_save(&rb_abs, &orig_content) {
                            rollback_failed
                                .push(format!("referrer '{}': {rb_e}", rb_rel.display()));
                        } else {
                            ledger.remove(&rb_rel);
                            if let Ok((rm, rb)) = parse_note_file(vault, &rb_rel) {
                                index.write().unwrap().upsert(rm, &rb);
                            }
                        }
                    }

                    // Roll back self-rename: restore original content to old_abs,
                    // remove new_abs if path changed.
                    if let Err(rb_e) = atomic_save(&old_abs, &content) {
                        rollback_failed.push(format!("self '{}': {rb_e}", old_rel.display()));
                    } else {
                        if old_abs != new_abs {
                            let _ = std::fs::remove_file(&new_abs);
                        }
                        ledger.remove(&new_rel);
                        if let Some(s) = stat(&old_abs) {
                            ledger.insert(old_rel.clone(), s);
                        }
                        if let Ok((sm, sb)) = parse_note_file(vault, &old_rel) {
                            let mut ix = index.write().unwrap();
                            ix.remove(id);
                            ix.upsert(sm, &sb);
                        }
                    }

                    if !rollback_failed.is_empty() {
                        emit(VaultEvent::Error {
                            context: "batch rollback".into(),
                            message: format!(
                                "rename rollback failed for: {}",
                                rollback_failed.join("; ")
                            ),
                        });
                    }

                    return Err((label, format!("rename rewrite referrer: {e}")));
                }
                // Save succeeded: record original content for potential future rollback
                rewritten_referrers.push((ref_abs.clone(), ref_rel.clone(), ref_content));
                if let Some(s) = stat(&ref_abs) {
                    ledger.insert(ref_rel.clone(), s);
                }
                if let Ok((rm, rb)) = parse_note_file(vault, &ref_rel) {
                    index.write().unwrap().upsert(rm, &rb);
                }
            }

            Ok(OpResult {
                inverse: Some(VaultOp::RenameTitle {
                    id,
                    new_title: old_title,
                }),
                label,
                created: vec![],
            })
        }

        VaultOp::Split { id, at_byte } => {
            let meta = index.read().unwrap().get(id).cloned().ok_or_else(|| {
                (
                    "Split note".to_string(),
                    format!("note {id} not found in index"),
                )
            })?;

            let is_fleeting = meta.status == Status::Fleeting;
            let display = label_display(meta.title.as_deref().unwrap_or(&meta.first_line));
            let label = op_label("Split", is_fleeting, &display);

            let rel = meta.rel_path.clone();
            let abs = vault.abs(&rel);

            let content = std::fs::read_to_string(&abs)
                .map_err(|e| (label.clone(), format!("split read: {e}")))?;
            let doc = NoteDoc::parse(&content);
            let old_body = doc.body.clone();

            // Validate at_byte
            if at_byte > old_body.len() {
                return Err((
                    label,
                    format!(
                        "split at_byte {at_byte} out of range (body len {})",
                        old_body.len()
                    ),
                ));
            }
            if !old_body.is_char_boundary(at_byte) {
                return Err((
                    label,
                    format!("split at_byte {at_byte} is not on a char boundary"),
                ));
            }
            let tail = &old_body[at_byte..];
            if tail.trim().is_empty() {
                return Err((label, "split tail is empty after trim".to_string()));
            }

            // Determine new note type and display ref
            let tail_trimmed = tail.trim_start();
            let (new_status, new_dest_dir, link_text) = if tail_trimmed.starts_with("# ") {
                // Extract title from heading
                let title = extract_title(tail_trimmed)
                    .map(|(t, _)| t)
                    .unwrap_or_else(|| "Untitled".to_string());
                let dir = vault.abs(Path::new("notes"));
                (Status::Permanent, dir, title)
            } else {
                // Use first non-empty line as link text
                let first = tail_trimmed
                    .lines()
                    .find(|l| !l.trim().is_empty())
                    .unwrap_or("Untitled")
                    .trim()
                    .to_string();
                let dir = vault.abs(Path::new("inbox"));
                (Status::Fleeting, dir, first)
            };

            // Generate new note id and path
            let new_id = NoteId::generate(id_gen);
            let now = Timestamp::now();
            let new_abs = filename_for(&link_text, new_id, &new_dest_dir);
            let new_rel = vault.rel(&new_abs).unwrap_or_else(|| new_abs.clone());

            // Build new note content
            let mut new_fm = FrontmatterDoc::synthesize(new_id, now, new_status);
            new_fm.set_modified(now);
            let new_doc = NoteDoc {
                fm: new_fm,
                body: tail.to_string(),
            };
            let new_content = new_doc.serialize();

            atomic_save(&new_abs, &new_content)
                .map_err(|e| (label.clone(), format!("split write new: {e}")))?;
            if let Some(s) = stat(&new_abs) {
                ledger.insert(new_rel.clone(), s);
            }

            // Index the new note
            match parse_note_file(vault, &new_rel) {
                Ok((nm, nb)) => {
                    index.write().unwrap().upsert(nm, &nb);
                }
                Err(e) => return Err((label, format!("split index new: {e}"))),
            }

            // Rewrite host body: body[..at_byte] + [[link_text]] + \n
            let new_host_body = format!("{}[[{}]]\n", &old_body[..at_byte], link_text);

            let mut new_host_doc = NoteDoc::parse(&content);
            new_host_doc.body = new_host_body;
            new_host_doc.fm.set_modified(now);
            let new_host_content = new_host_doc.serialize();

            atomic_save(&abs, &new_host_content)
                .map_err(|e| (label.clone(), format!("split write host: {e}")))?;
            if let Some(s) = stat(&abs) {
                ledger.insert(rel.clone(), s);
            }

            // Re-upsert host in index
            match parse_note_file(vault, &rel) {
                Ok((hm, hb)) => {
                    index.write().unwrap().upsert(hm, &hb);
                }
                Err(e) => return Err((label, format!("split index host: {e}"))),
            }

            Ok(OpResult {
                inverse: Some(VaultOp::Batch(vec![
                    VaultOp::SaveBody {
                        id,
                        content: old_body,
                    },
                    VaultOp::Delete { id: new_id },
                ])),
                label,
                created: vec![new_id],
            })
        }

        VaultOp::Batch(ops) => {
            // Each completed member: (forward label, Option<inverse op>)
            let mut done: Vec<(String, Option<VaultOp>)> = Vec::new();
            let mut all_created: Vec<NoteId> = Vec::new();
            let mut first_label: Option<String> = None;

            for op in ops {
                match execute_op(vault, index, id_gen, ledger, emit, op) {
                    Ok(result) => {
                        if first_label.is_none() {
                            first_label = Some(result.label.clone());
                        }
                        done.push((result.label, result.inverse));
                        all_created.extend(result.created);
                    }
                    Err((label, msg)) => {
                        // Rollback in reverse order; collect any rollback failures.
                        // A rollback failure means the vault may be in mixed state —
                        // surface each as VaultEvent::Error so the user can see it.
                        let mut rollback_failed: Vec<String> = Vec::new();
                        for (fwd_label, inv_op) in done.into_iter().rev() {
                            if let Some(inv) = inv_op
                                && let Err((rb_label, rb_msg)) =
                                    execute_op(vault, index, id_gen, ledger, emit, inv)
                            {
                                rollback_failed
                                    .push(format!("'{fwd_label}' → {rb_label}: {rb_msg}"));
                            }
                        }
                        if !rollback_failed.is_empty() {
                            emit(VaultEvent::Error {
                                context: "batch rollback".into(),
                                message: format!(
                                    "rollback failed for: {}",
                                    rollback_failed.join("; ")
                                ),
                            });
                        }
                        // Return the failing member's label with the error
                        return Err((label, msg));
                    }
                }
            }

            let done_inverses: Vec<VaultOp> =
                done.into_iter().rev().filter_map(|(_, inv)| inv).collect();
            Ok(OpResult {
                inverse: Some(VaultOp::Batch(done_inverses)),
                label: first_label.unwrap_or_else(|| "Batch".into()),
                created: all_created,
            })
        }
    }
}

/// Handle a single VaultCommand (not Shutdown).
#[allow(clippy::too_many_arguments)]
fn handle_command(
    vault: &Vault,
    index: &SharedIndex,
    id_gen: &mut IdGen,
    ledger: &mut WriteLedger,
    event_tx: &Mutex<mpsc::Sender<VaultEvent>>,
    wake: &(dyn Fn() + Sync),
    emit: &impl Fn(VaultEvent),
    cmd: VaultCommand,
) {
    match cmd {
        VaultCommand::Op { op, source } => {
            match execute_op(vault, index, id_gen, ledger, emit, op) {
                Ok(result) => emit(VaultEvent::OpDone { result, source }),
                Err((label, message)) => emit(VaultEvent::OpFailed { label, message }),
            }
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
            run_initial_scan(vault, index, event_tx, wake, emit);
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
                    // Look up the previous meta for this path in the index.
                    let prev_meta = index
                        .read()
                        .unwrap()
                        .iter_meta()
                        .find(|m| m.rel_path == rel)
                        .cloned();

                    let emit_id;
                    if let Some(prev) = prev_meta {
                        if meta.id != prev.id {
                            // The file lost its frontmatter id (e.g. external overwrite).
                            // Preserve identity: reuse the existing id, remove the old entry
                            // and re-insert under that id — one lock, one call.
                            meta.id = prev.id;
                        }
                        // WP1d handoff: preserve created timestamp
                        meta.created = prev.created;
                        emit_id = prev.id;
                        index.write().unwrap().replace_at_path(prev.id, meta, &body);
                    } else {
                        emit_id = meta.id;
                        index.write().unwrap().upsert(meta, &body);
                    }
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

/// Generate the conflict copy path via `filename_for` so that two conflicts in
/// the same minute get distinct names (spec §2: never silently clobber either side).
///
/// The conflict TITLE is `"<stem> (conflict YYYY-MM-DD HHMM)"`.  The first
/// conflict in a given minute resolves to the plain `<title>.md`; a second one
/// (or any collision) gets the `<title> (<short-id>).md` suffix from
/// `filename_for`.
fn conflict_copy_path(abs_path: &Path, conflict_id: NoteId) -> PathBuf {
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
    let conflict_title = format!("{stem} (conflict {conflict_tag})");
    filename_for(&conflict_title, conflict_id, dir)
}
