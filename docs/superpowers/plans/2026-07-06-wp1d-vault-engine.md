# WP1d — Vault Engine Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Implement `jd-core`'s vault engine — atomic I/O, parallel scan, file watcher, trash/recovery, and the vault-worker thread — per architecture doc §2.10/§2.12 and spec §2–§3, with the editor-zoo integration tests and CI performance budgets from spec §13.

**Architecture:** `crates/jd-core/src/vault/{mod, io, scan, watcher, trash, recovery}.rs` + `error.rs` + `worker.rs`. The vault on disk is the only persistent truth; the worker (one thread) owns all writes and applies index updates; the watcher (notify + our debouncer) feeds external changes to the worker; saves are atomic (temp + fsync + rename). First real dependency: `notify` (approved, spec Appendix B).

**Tech Stack:** Rust stable, std + `notify`. Branch: `feat/vault-engine` (single stream — everything here is coupled through the worker).

## Decisions Pinned by This Plan (beyond the architecture doc — flag disagreements early)

1. **Synthetic IDs for id-less files (foreign/Obsidian notes):** scan assigns a deterministic `NoteId` derived from a 128-bit FNV-1a hash of the rel path — stable across rescans, changes on rename (acceptable: rename of an id-less file IS an identity change). The ID becomes persistent when the worker first rewrites the file's frontmatter. **WP1e handoff:** ops that rewrite frontmatter persist the current index id via `synthesize`.
2. **Watcher semantics are existence-based:** after the 200 ms debounce, each coalesced path flushes as `Changed` (file exists) or `Removed` (doesn't). Rename-swap saves therefore normalize to `Changed(target)` + `Removed(tmp-source)`-suppressed-by-filter. `WatchEvent::Renamed` stays in the enum (arch §2.10) but v1 emission is best-effort; consumers must handle rename as Removed+Changed.
3. **Worker channel topology (std has no `select`):** one internal `mpsc<WorkerMsg>`; a 5-line forwarder thread repackages the public `Sender<VaultCommand>`, and the debouncer sends watch events directly. Threads: worker, notify's own, debouncer, forwarder.
4. **Echo suppression** (self-write vs external edit) via a worker-owned write-ledger: rel path → (len, mtime) recorded after each of our writes; a watch `Changed` matching the ledger is dropped.
5. **Conflict rule (spec §2):** before writing, compare the file's current (len, mtime) to the ledger; mismatch = external edit → write ours to `<stem> (conflict <YYYY-MM-DD HHMM>).md`, leave theirs in place, emit `Conflict`.
6. **Perf budgets run in release only:** `#[cfg_attr(debug_assertions, ignore)]` + a dedicated CI step `cargo test -p jd-core --release --test perf`. Debug runs skip them (they'd fail meaninglessly unoptimized).
7. **Tempdir helper written in-house** (`tests/common/mod.rs`, ~25 lines, Drop-cleanup) — the `tempfile` crate stays out of the tree.

## Global Constraints

- **Dependencies: `notify` only** (added in Task 5; spec Appendix B approves it). Nothing else. `tempfile`, `crossbeam`, `rayon` are all rejected — parallelism is `std::thread::scope`.
- Every commit leaves `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace` green.
- Public signatures match architecture doc §2.10/§2.12 (as refined by the decisions above; the doc's `VaultCommand::Op{VaultOp}` form lands in WP1e — WP1d ships the narrower command set below).
- **The load-bearing invariant (spec §13): the atomic-save torture test — a simulated kill between temp-write and rename must leave the original byte-intact.** Never weaken it.
- Watcher tests use generous deadlines (poll up to 3 s) and tolerant coalescing bounds — platform FS event latency varies; a test that's flaky on macOS/CI is a bug in the test.
- TDD throughout; RED evidence is a deliverable per task (integration-heavy tasks: RED = the new test failing against a stub or missing symbol).

---

### Task 1: `error.rs` + `vault/mod.rs` — errors and Vault::open

**Files:**
- Create: `crates/jd-core/src/error.rs`, `crates/jd-core/src/vault/mod.rs`
- Modify: `crates/jd-core/src/lib.rs` (add `pub mod error;` and `pub mod vault;`)

**Interfaces:**
- Produces: `IoError { path, op, source }`, `VaultError`, `WatchError`, `CoreError` (wrapping those + `FmError`/`TimeError`/`IdError` — the WP1a follow-up), all with `Display` rendering human sentences (spec §3 error posture). `Vault::open(root) -> Result<Vault, VaultError>` creating `inbox/`, `notes/`, `.junkdrawer/{trash,recovery,session}/`; `root()`, `abs(rel)`, `rel(abs) -> Option<PathBuf>`.

- [ ] **Step 1: Write the failing tests** (in `vault/mod.rs`'s test module; the tempdir helper is inline here and gets extracted to `tests/common` in Task 2)

```rust
#[cfg(test)]
mod tests {
    use super::*;

    struct TempDir(std::path::PathBuf);
    impl TempDir {
        fn new() -> TempDir {
            use std::sync::atomic::{AtomicU32, Ordering};
            static N: AtomicU32 = AtomicU32::new(0);
            let p = std::env::temp_dir().join(format!(
                "jd-vault-test-{}-{}",
                std::process::id(),
                N.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir_all(&p).unwrap();
            TempDir(p)
        }
    }
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn open_creates_the_layout() {
        let t = TempDir::new();
        let v = Vault::open(&t.0).unwrap();
        for sub in ["inbox", "notes", ".junkdrawer/trash", ".junkdrawer/recovery", ".junkdrawer/session"] {
            assert!(t.0.join(sub).is_dir(), "{sub} missing");
        }
        assert_eq!(v.root(), t.0.as_path());
    }

    #[test]
    fn open_is_idempotent_and_preserves_content() {
        let t = TempDir::new();
        Vault::open(&t.0).unwrap();
        std::fs::write(t.0.join("notes/existing.md"), "# Keep me\n").unwrap();
        Vault::open(&t.0).unwrap();
        assert_eq!(std::fs::read_to_string(t.0.join("notes/existing.md")).unwrap(), "# Keep me\n");
    }

    #[test]
    fn open_rejects_a_file_path() {
        let t = TempDir::new();
        let f = t.0.join("a-file");
        std::fs::write(&f, "x").unwrap();
        assert!(matches!(Vault::open(&f), Err(VaultError::NotADirectory(_))));
    }

    #[test]
    fn abs_and_rel_are_inverses() {
        let t = TempDir::new();
        let v = Vault::open(&t.0).unwrap();
        let rel = std::path::Path::new("notes/x.md");
        let abs = v.abs(rel);
        assert!(abs.starts_with(v.root()));
        assert_eq!(v.rel(&abs).unwrap(), rel);
        assert_eq!(v.rel(std::path::Path::new("/elsewhere/x.md")), None);
    }

    #[test]
    fn errors_render_human_sentences() {
        let e = crate::error::IoError {
            path: "notes/x.md".into(),
            op: "save",
            source: std::io::Error::new(std::io::ErrorKind::PermissionDenied, "denied"),
        };
        let msg = e.to_string();
        assert!(msg.contains("save") && msg.contains("notes/x.md"), "unhelpful: {msg}");
    }
}
```

- [ ] **Step 2: Run to verify failure** — `cargo test -p jd-core vault` → compile error.

- [ ] **Step 3: Implement**

`crates/jd-core/src/error.rs`:

```rust
//! Typed errors rendering human sentences with context (spec §3 error
//! posture). No `anyhow`, no `Box<dyn Error>` in public APIs.

use std::fmt;
use std::path::PathBuf;

use crate::frontmatter::FmError;
use crate::id::IdError;
use crate::time::TimeError;

/// A filesystem operation that failed, with enough context for the UI to
/// render "Couldn't save 'x' — permission denied. [Retry]".
#[derive(Debug)]
pub struct IoError {
    pub path: PathBuf,
    /// Verb phrase: "save", "read", "move to trash", …
    pub op: &'static str,
    pub source: std::io::Error,
}

impl fmt::Display for IoError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "couldn't {} '{}': {}", self.op, self.path.display(), self.source)
    }
}

impl IoError {
    pub(crate) fn wrap(op: &'static str, path: &std::path::Path) -> impl FnOnce(std::io::Error) -> IoError + '_ {
        move |source| IoError { path: path.to_owned(), op, source }
    }
}

#[derive(Debug)]
pub enum VaultError {
    NotADirectory(PathBuf),
    Io(IoError),
}

impl fmt::Display for VaultError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            VaultError::NotADirectory(p) => write!(f, "'{}' isn't a folder", p.display()),
            VaultError::Io(e) => e.fmt(f),
        }
    }
}

#[derive(Debug)]
pub enum WatchError {
    Init(String),
}

impl fmt::Display for WatchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            WatchError::Init(s) => write!(f, "couldn't watch the vault for changes: {s}"),
        }
    }
}

#[derive(Debug)]
pub enum CoreError {
    Io(IoError),
    Vault(VaultError),
    Watch(WatchError),
    Parse(FmError),
    Time(TimeError),
    Id(IdError),
}

impl fmt::Display for CoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CoreError::Io(e) => e.fmt(f),
            CoreError::Vault(e) => e.fmt(f),
            CoreError::Watch(e) => e.fmt(f),
            CoreError::Parse(e) => write!(f, "couldn't read the note's header: {e:?}"),
            CoreError::Time(e) => write!(f, "couldn't read a timestamp: {e:?}"),
            CoreError::Id(e) => write!(f, "couldn't read a note id: {e:?}"),
        }
    }
}

impl From<IoError> for CoreError { fn from(e: IoError) -> Self { CoreError::Io(e) } }
impl From<VaultError> for CoreError { fn from(e: VaultError) -> Self { CoreError::Vault(e) } }
impl From<WatchError> for CoreError { fn from(e: WatchError) -> Self { CoreError::Watch(e) } }
```

`crates/jd-core/src/vault/mod.rs`:

```rust
//! The vault: one folder the user picks; `inbox/` + `notes/` are workflow
//! state made visible; `.junkdrawer/` is disposable machine state (spec §2).

// submodules land one per task: io (T2), scan (T3), trash+recovery (T4), watcher (T5)

use std::path::{Path, PathBuf};

use crate::error::{IoError, VaultError};

pub struct Vault {
    root: PathBuf,
}

impl Vault {
    /// Creates the vault layout as needed; never touches existing notes.
    pub fn open(root: &Path) -> Result<Vault, VaultError> {
        if root.exists() && !root.is_dir() {
            return Err(VaultError::NotADirectory(root.to_owned()));
        }
        for sub in ["inbox", "notes", ".junkdrawer/trash", ".junkdrawer/recovery", ".junkdrawer/session"] {
            let dir = root.join(sub);
            std::fs::create_dir_all(&dir).map_err(IoError::wrap("create folder", &dir)).map_err(VaultError::Io)?;
        }
        Ok(Vault { root: root.to_owned() })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn abs(&self, rel: &Path) -> PathBuf {
        self.root.join(rel)
    }

    /// Inverse of `abs`: None if the path isn't under this vault.
    pub fn rel(&self, abs: &Path) -> Option<PathBuf> {
        abs.strip_prefix(&self.root).ok().map(Path::to_owned)
    }
}
```

(The `vault/` module list grows one `pub mod` line per task — this task's commit declares none.)

- [ ] **Step 4: Run to verify pass** — `cargo test -p jd-core vault` → 5 passed; full gate.
- [ ] **Step 5: Commit** — `feat(core): vault layout and typed errors`

---

### Task 2: `vault/io.rs` — atomic save, filenames, torture test

**Files:**
- Create: `crates/jd-core/src/vault/io.rs`, `crates/jd-core/tests/common/mod.rs`, `crates/jd-core/tests/vault_io.rs`
- Modify: `crates/jd-core/src/vault/mod.rs` (add `pub mod io;`)

**Interfaces:**
- Produces: `atomic_save(abs, content) -> Result<(), IoError>`; `pub(crate) atomic_save_with(abs, content, checkpoint)` (failpoint injection — `checkpoint: &dyn Fn(&str) -> std::io::Result<()>` called with `"written"` after temp+fsync and `"renamed"` after rename); `sanitize_filename(title) -> String`; `filename_for(title, id, dir) -> PathBuf` (collision → ` (XXXXXXXX)` short-id suffix); `is_our_tempfile(name) -> bool` (the watcher filter uses it in Task 5). `tests/common/mod.rs` exports the `TempDir` helper (moved from Task 1's inline copy — leave the inline one in `vault/mod.rs` tests; unit tests can't import integration-test helpers).

`atomic_save` steps (spec §3): temp file `.{name}.jd-tmp` in the SAME directory → write → `sync_all` → checkpoint("written") → rename over target → on unix, fsync the directory → checkpoint("renamed"). Any failure leaves the original untouched; stale temp files are overwritten by the next save.

- [ ] **Step 1: Write the failing tests**

`crates/jd-core/tests/common/mod.rs`:

```rust
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
            "jd-it-{}-{}",
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
```

`crates/jd-core/tests/vault_io.rs`:

```rust
//! Atomic-save torture (spec §13): a kill between temp-write and rename must
//! leave the original byte-intact. Plus filename sanitization/collision.

mod common;

use common::TempDir;
use jd_core::id::NoteId;
use jd_core::vault::io::{atomic_save, filename_for, sanitize_filename};

#[test]
fn atomic_save_writes_and_replaces() {
    let t = TempDir::new();
    let f = t.path().join("note.md");
    atomic_save(&f, "first").unwrap();
    assert_eq!(std::fs::read_to_string(&f).unwrap(), "first");
    atomic_save(&f, "second").unwrap();
    assert_eq!(std::fs::read_to_string(&f).unwrap(), "second");
    // no temp litter after success
    let leftovers: Vec<_> = std::fs::read_dir(t.path()).unwrap()
        .filter(|e| e.as_ref().unwrap().file_name().to_string_lossy().contains("jd-tmp"))
        .collect();
    assert!(leftovers.is_empty());
}

#[test]
fn torture_kill_before_rename_leaves_original_intact() {
    let t = TempDir::new();
    let f = t.path().join("note.md");
    atomic_save(&f, "precious original").unwrap();

    // simulate a crash after the temp file is written+synced but before rename
    let killed = jd_core::vault::io::atomic_save_with(&f, "half-baked", &|phase| {
        if phase == "written" {
            Err(std::io::Error::other("simulated kill"))
        } else {
            Ok(())
        }
    });
    assert!(killed.is_err());
    assert_eq!(
        std::fs::read_to_string(&f).unwrap(),
        "precious original",
        "the original must survive a crash mid-save"
    );

    // and the next save recovers cleanly, overwriting any stale temp
    atomic_save(&f, "fresh").unwrap();
    assert_eq!(std::fs::read_to_string(&f).unwrap(), "fresh");
}

#[test]
fn sanitize_strips_forbidden_and_caps_length() {
    assert_eq!(sanitize_filename("Egui: immediate/mode <tradeoffs>?"), "Egui immediatemode tradeoffs");
    assert_eq!(sanitize_filename("  trailing dots... "), "trailing dots");
    assert_eq!(sanitize_filename(""), "Untitled");
    assert_eq!(sanitize_filename("///"), "Untitled");
    let long = "x".repeat(500);
    assert!(sanitize_filename(&long).len() <= 120);
    // multibyte-safe cap
    let long_multi = "é".repeat(300);
    let s = sanitize_filename(&long_multi);
    assert!(s.len() <= 120 && s.chars().all(|c| c == 'é'));
}

#[test]
fn filename_for_suffixes_on_collision() {
    let t = TempDir::new();
    let id = NoteId::parse("01J8ZQ4KF3T9M2X7C5VBNAE8RD").unwrap();
    let first = filename_for("My Note", id, t.path());
    assert_eq!(first.file_name().unwrap().to_str().unwrap(), "My Note.md");
    std::fs::write(&first, "occupied").unwrap();
    let second = filename_for("My Note", id, t.path());
    assert_eq!(second.file_name().unwrap().to_str().unwrap(), "My Note (01J8ZQ4K).md");
}
```

- [ ] **Step 2: RED** — `cargo test -p jd-core --test vault_io` → compile error.

- [ ] **Step 3: Implement** `crates/jd-core/src/vault/io.rs`:

```rust
//! Atomic writes and filename rules (spec §2, §3). Saves are temp + fsync +
//! rename; a crash at any point leaves the original intact (torture-tested).

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use crate::error::IoError;
use crate::id::NoteId;

const MAX_FILENAME_BYTES: usize = 120;
const FORBIDDEN: &[char] = &['/', '\\', ':', '*', '?', '"', '<', '>', '|'];

pub fn atomic_save(abs_path: &Path, content: &str) -> Result<(), IoError> {
    atomic_save_with(abs_path, content, &|_| Ok(()))
}

/// Failpoint-injectable core: `checkpoint("written")` fires after the temp
/// file is written and synced; `checkpoint("renamed")` after the rename.
/// Tests inject failures to simulate crashes between phases.
pub fn atomic_save_with(
    abs_path: &Path,
    content: &str,
    checkpoint: &dyn Fn(&str) -> std::io::Result<()>,
) -> Result<(), IoError> {
    let dir = abs_path.parent().unwrap_or_else(|| Path::new("."));
    let name = abs_path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let tmp = dir.join(format!(".{name}.jd-tmp"));
    let wrap = |op| IoError::wrap(op, abs_path);

    let mut f = fs::File::create(&tmp).map_err(wrap("save"))?;
    f.write_all(content.as_bytes()).map_err(wrap("save"))?;
    f.sync_all().map_err(wrap("save"))?;
    drop(f);
    checkpoint("written").map_err(wrap("save"))?;

    fs::rename(&tmp, abs_path).map_err(wrap("save"))?;
    #[cfg(unix)]
    {
        if let Ok(d) = fs::File::open(dir) {
            let _ = d.sync_all(); // best-effort directory durability
        }
    }
    checkpoint("renamed").map_err(wrap("save"))?;
    Ok(())
}

/// True for our own temp files — the watcher must ignore them.
pub fn is_our_tempfile(file_name: &str) -> bool {
    file_name.starts_with('.') && file_name.ends_with(".jd-tmp")
}

/// Strip path-hostile characters and control chars, trim dots/spaces,
/// cap at a char boundary. Empty results become "Untitled".
pub fn sanitize_filename(title: &str) -> String {
    let mut s: String = title
        .chars()
        .filter(|c| !FORBIDDEN.contains(c) && !c.is_control())
        .collect();
    s = s.trim().trim_end_matches(['.', ' ']).trim().to_owned();
    if s.len() > MAX_FILENAME_BYTES {
        let mut cut = MAX_FILENAME_BYTES;
        while !s.is_char_boundary(cut) {
            cut -= 1;
        }
        s.truncate(cut);
    }
    if s.is_empty() {
        "Untitled".to_owned()
    } else {
        s
    }
}

/// "Title.md", or "Title (01J8ZQ4K).md" when a different file already holds
/// the name (spec §2 collision rule).
pub fn filename_for(title: &str, id: NoteId, dir: &Path) -> PathBuf {
    let base = sanitize_filename(title);
    let plain = dir.join(format!("{base}.md"));
    if !plain.exists() {
        return plain;
    }
    dir.join(format!("{base} ({}).md", id.short()))
}
```

Add `pub mod io;` to `vault/mod.rs`.

- [ ] **Step 4: GREEN** — `cargo test -p jd-core --test vault_io` → 4 passed; full gate.
- [ ] **Step 5: Commit** — `feat(core): atomic saves with torture test and filename rules`

---

### Task 3: `vault/scan.rs` — parallel scan + quarantine + synthetic IDs

**Files:**
- Create: `crates/jd-core/src/vault/scan.rs`, `crates/jd-core/tests/vault_scan.rs`
- Modify: `crates/jd-core/src/vault/mod.rs` (add `pub mod scan;`)

**Interfaces:**
- Produces: `ScanOutcome { metas: Vec<(NoteMeta, String)>, quarantined: Vec<QuarantinedFile> }`, `QuarantinedFile { rel_path, error }`, `scan(vault, progress: &(dyn Fn(usize, usize) + Sync)) -> Result<ScanOutcome, VaultError>`, `pub(crate) fn note_files(vault) -> Result<Vec<PathBuf>, VaultError>` (rel paths of `.md` files under `inbox/` + `notes/`, recursive, skipping dot-files and our temp files), `pub fn synthetic_id(rel: &Path) -> NoteId` (FNV-1a 128-bit path hash — decision #1), `pub(crate) fn parse_note_file(vault, rel) -> Result<(NoteMeta, String), String>` (single-file parse shared with the worker's incremental path).

Semantics: every `.md` file parses (NoteDoc::parse is infallible) — quarantine happens only for unreadable files (I/O error, invalid UTF-8). ID = frontmatter `id:` if present+valid, else `synthetic_id(rel)`. `fs_modified` from file mtime. Progress callback: `(done, total)` from an `AtomicUsize`, called per file. Parallelism: `std::thread::scope` with `std::thread::available_parallelism()` chunks.

- [ ] **Step 1: Write the failing tests**

```rust
//! Parallel startup scan (spec §3): every note parses or quarantines;
//! the scan itself never fails on file content.

mod common;

use common::TempDir;
use jd_core::vault::scan::{scan, synthetic_id};
use jd_core::vault::Vault;

fn vault_with(notes: &[(&str, &str)]) -> (TempDir, Vault) {
    let t = TempDir::new();
    let v = Vault::open(t.path()).unwrap();
    for (rel, content) in notes {
        let p = t.path().join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, content).unwrap();
    }
    (t, v)
}

#[test]
fn scans_both_dirs_and_extracts_meta() {
    let (_t, v) = vault_with(&[
        ("inbox/scrap.md", "a stray thought\n"),
        ("notes/Card.md", "---\nid: 01J8ZQ4KF3T9M2X7C5VBNAE8RD\nstatus: permanent\n---\n# Card\nBody [[Link]].\n"),
    ]);
    let out = scan(&v, &|_, _| {}).unwrap();
    assert_eq!(out.metas.len(), 2);
    assert!(out.quarantined.is_empty());
    let card = out.metas.iter().find(|(m, _)| m.title.as_deref() == Some("Card")).unwrap();
    assert_eq!(card.0.id.to_string(), "01J8ZQ4KF3T9M2X7C5VBNAE8RD");
    assert_eq!(card.0.links_out.len(), 1);
    let scrap = out.metas.iter().find(|(m, _)| m.title.is_none()).unwrap();
    assert_eq!(scrap.0.status, jd_core::note::Status::Fleeting); // inbox path default
    assert_eq!(scrap.0.id, synthetic_id(std::path::Path::new("inbox/scrap.md")));
}

#[test]
fn synthetic_ids_are_stable_and_distinct() {
    let a = synthetic_id(std::path::Path::new("inbox/a.md"));
    assert_eq!(a, synthetic_id(std::path::Path::new("inbox/a.md")));
    assert_ne!(a, synthetic_id(std::path::Path::new("inbox/b.md")));
}

#[test]
fn unreadable_files_quarantine_without_failing_the_scan() {
    let (_t, v) = vault_with(&[("notes/good.md", "fine\n")]);
    // invalid UTF-8 file
    std::fs::write(v.root().join("notes/bad.md"), [0xFF, 0xFE, 0x00, 0x01]).unwrap();
    let out = scan(&v, &|_, _| {}).unwrap();
    assert_eq!(out.metas.len(), 1);
    assert_eq!(out.quarantined.len(), 1);
    assert_eq!(out.quarantined[0].rel_path, std::path::Path::new("notes/bad.md"));
}

#[test]
fn skips_hidden_temp_and_non_md() {
    let (_t, v) = vault_with(&[
        ("notes/real.md", "x\n"),
        ("notes/.hidden.md", "x\n"),
        ("notes/.real.md.jd-tmp", "x\n"),
        ("notes/readme.txt", "x\n"),
    ]);
    let out = scan(&v, &|_, _| {}).unwrap();
    assert_eq!(out.metas.len(), 1);
}

#[test]
fn progress_reaches_total() {
    let (_t, v) = vault_with(&[("notes/a.md", "a\n"), ("notes/b.md", "b\n"), ("inbox/c.md", "c\n")]);
    use std::sync::atomic::{AtomicUsize, Ordering};
    let max_done = AtomicUsize::new(0);
    let total_seen = AtomicUsize::new(0);
    scan(&v, &|done, total| {
        max_done.fetch_max(done, Ordering::Relaxed);
        total_seen.store(total, Ordering::Relaxed);
    })
    .unwrap();
    assert_eq!(max_done.load(Ordering::Relaxed), 3);
    assert_eq!(total_seen.load(Ordering::Relaxed), 3);
}

#[test]
fn subdirectories_are_scanned() {
    let (_t, v) = vault_with(&[("notes/sub/deep.md", "# Deep\n")]);
    let out = scan(&v, &|_, _| {}).unwrap();
    assert_eq!(out.metas.len(), 1);
    assert_eq!(out.metas[0].0.rel_path, std::path::Path::new("notes/sub/deep.md"));
}
```

- [ ] **Step 2: RED** — compile error.

- [ ] **Step 3: Implement**

```rust
//! Parallel startup scan (spec §3): the index is rebuilt from disk truth.
//! A file that fails to READ is quarantined; file CONTENT never fails
//! (NoteDoc::parse is infallible).

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

use crate::doc::NoteDoc;
use crate::error::{IoError, VaultError};
use crate::id::NoteId;
use crate::note::NoteMeta;
use crate::time::Timestamp;
use crate::vault::io::is_our_tempfile;
use crate::vault::Vault;

pub struct QuarantinedFile {
    pub rel_path: PathBuf,
    pub error: String,
}

pub struct ScanOutcome {
    /// Meta + body per note; the caller feeds these to Index::upsert and
    /// drops the bodies (they are never retained — spec §3).
    pub metas: Vec<(NoteMeta, String)>,
    pub quarantined: Vec<QuarantinedFile>,
}

/// Deterministic ID for files without a frontmatter id (decision #1):
/// 128-bit FNV-1a over the rel path, two offset bases. Stable across
/// rescans; becomes persistent when the worker first rewrites frontmatter.
pub fn synthetic_id(rel: &Path) -> NoteId {
    fn fnv64(bytes: &[u8], mut hash: u64) -> u64 {
        for &b in bytes {
            hash ^= b as u64;
            hash = hash.wrapping_mul(0x0000_0100_0000_01B3);
        }
        hash
    }
    let s = rel.to_string_lossy();
    let a = fnv64(s.as_bytes(), 0xcbf2_9ce4_8422_2325);
    let b = fnv64(s.as_bytes(), 0x9e37_79b9_7f4a_7c15);
    let mut bytes = [0u8; 16];
    bytes[..8].copy_from_slice(&a.to_be_bytes());
    bytes[8..].copy_from_slice(&b.to_be_bytes());
    NoteId(bytes)
}

/// Rel paths of every note file under inbox/ and notes/, recursive,
/// skipping dot-files, our temp files, and non-.md files.
pub(crate) fn note_files(vault: &Vault) -> Result<Vec<PathBuf>, VaultError> {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) -> std::io::Result<()> {
        for entry in std::fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') || is_our_tempfile(&name) {
                continue;
            }
            if path.is_dir() {
                walk(&path, out)?;
            } else if path.extension().is_some_and(|e| e == "md") {
                out.push(path);
            }
        }
        Ok(())
    }
    let mut abs = Vec::new();
    for top in ["inbox", "notes"] {
        let dir = vault.abs(Path::new(top));
        walk(&dir, &mut abs)
            .map_err(IoError::wrap("scan", &dir))
            .map_err(VaultError::Io)?;
    }
    Ok(abs.into_iter().filter_map(|p| vault.rel(&p)).collect())
}

/// Read + parse one note. Err(reason) means unreadable → quarantine.
pub(crate) fn parse_note_file(vault: &Vault, rel: &Path) -> Result<(NoteMeta, String), String> {
    let abs = vault.abs(rel);
    let src = std::fs::read_to_string(&abs).map_err(|e| e.to_string())?;
    let fs_modified = abs
        .metadata()
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| Timestamp(d.as_millis() as i64))
        .unwrap_or_else(Timestamp::now);
    let doc = NoteDoc::parse(&src);
    let id = doc.fm.id().unwrap_or_else(|| synthetic_id(rel));
    let meta = doc.to_meta(id, rel, fs_modified);
    Ok((meta, doc.body))
}

pub fn scan(
    vault: &Vault,
    progress: &(dyn Fn(usize, usize) + Sync),
) -> Result<ScanOutcome, VaultError> {
    let files = note_files(vault)?;
    let total = files.len();
    let done = AtomicUsize::new(0);
    let metas = Mutex::new(Vec::with_capacity(total));
    let quarantined = Mutex::new(Vec::new());

    let workers = std::thread::available_parallelism().map_or(4, |n| n.get());
    let chunk = files.len().div_ceil(workers).max(1);
    std::thread::scope(|s| {
        for slice in files.chunks(chunk) {
            s.spawn(|| {
                for rel in slice {
                    match parse_note_file(vault, rel) {
                        Ok(pair) => metas.lock().unwrap().push(pair),
                        Err(error) => quarantined
                            .lock()
                            .unwrap()
                            .push(QuarantinedFile { rel_path: rel.clone(), error }),
                    }
                    progress(done.fetch_add(1, Ordering::Relaxed) + 1, total);
                }
            });
        }
    });

    Ok(ScanOutcome {
        metas: metas.into_inner().unwrap(),
        quarantined: quarantined.into_inner().unwrap(),
    })
}
```

Note: `NoteId(bytes)` requires the tuple field to be `pub` — it is (`NoteId(pub [u8; 16])`). Add `pub mod scan;` to `vault/mod.rs`.

- [ ] **Step 4: GREEN** — 6 passed; full gate.
- [ ] **Step 5: Commit** — `feat(core): parallel vault scan with quarantine and synthetic ids`

---

### Task 4: `vault/trash.rs` + `vault/recovery.rs`

**Files:**
- Create: `crates/jd-core/src/vault/trash.rs`, `crates/jd-core/src/vault/recovery.rs`, `crates/jd-core/tests/vault_trash.rs`
- Modify: `crates/jd-core/src/vault/mod.rs` (add both mods)

**Interfaces (arch §2.10):**
- trash: `TrashEntry { id, title_or_first_line, deleted: Timestamp }`, `trash_note(vault, meta) -> Result<(), IoError>` (moves the note file to `.junkdrawer/trash/<ULID>.md` + writes `<ULID>.meta` — 3 lines: original rel path, deleted-at RFC3339, display title), `list_trash(vault) -> Vec<TrashEntry>` (newest first), `restore(vault, id) -> Result<PathBuf, IoError>` (back to the original dir, re-collision-checked via `filename_for`), `purge_older_than(vault, days: Option<u32>) -> Result<usize, IoError>` (None = manual-only → 0 purged).
- recovery: `journal_buffer(vault, id, content)` (atomic_save to `.junkdrawer/recovery/<ULID>.md`), `clear_buffer(vault, id)`, `pending_recoveries(vault) -> Vec<(NoteId, String)>`.

The `.meta` format is 3 plain lines (path / timestamp / title) — hand-parsed, disposable-by-design like everything in `.junkdrawer/`.

- [ ] **Step 1: Write the failing tests**

```rust
//! Trash lifecycle (spec §7): tossed notes are recoverable per retention;
//! recovery journal survives the autosave debounce window (spec §3).

mod common;

use common::TempDir;
use jd_core::note::{Kind, NoteMeta, Status};
use jd_core::time::Timestamp;
use jd_core::vault::recovery::{clear_buffer, journal_buffer, pending_recoveries};
use jd_core::vault::trash::{list_trash, purge_older_than, restore, trash_note};
use jd_core::vault::Vault;

fn meta_for(v: &Vault, rel: &str, body: &str) -> NoteMeta {
    std::fs::write(v.root().join(rel), body).unwrap();
    let id = jd_core::vault::scan::synthetic_id(std::path::Path::new(rel));
    NoteMeta {
        id,
        rel_path: rel.into(),
        title: None,
        first_line: body.lines().next().unwrap_or("").to_owned(),
        status: Status::Fleeting,
        kind: Kind::Note,
        source: None,
        created: Timestamp(0),
        modified: Timestamp(0),
        tags: Default::default(),
        links_out: vec![],
        word_count: 1,
    }
}

#[test]
fn trash_restore_round_trip() {
    let t = TempDir::new();
    let v = Vault::open(t.path()).unwrap();
    let meta = meta_for(&v, "inbox/tossme.md", "a doomed thought\n");
    trash_note(&v, &meta).unwrap();
    assert!(!t.path().join("inbox/tossme.md").exists());

    let listed = list_trash(&v);
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].id, meta.id);
    assert_eq!(listed[0].title_or_first_line, "a doomed thought");

    let back = restore(&v, meta.id).unwrap();
    assert_eq!(back, std::path::Path::new("inbox/tossme.md"));
    assert_eq!(std::fs::read_to_string(t.path().join("inbox/tossme.md")).unwrap(), "a doomed thought\n");
    assert!(list_trash(&v).is_empty());
}

#[test]
fn restore_recollides_when_the_name_is_retaken() {
    let t = TempDir::new();
    let v = Vault::open(t.path()).unwrap();
    let meta = meta_for(&v, "notes/Taken.md", "original\n");
    trash_note(&v, &meta).unwrap();
    std::fs::write(t.path().join("notes/Taken.md"), "usurper\n").unwrap();
    let back = restore(&v, meta.id).unwrap();
    assert_ne!(back, std::path::Path::new("notes/Taken.md"));
    assert_eq!(std::fs::read_to_string(t.path().join("notes/Taken.md")).unwrap(), "usurper\n");
    assert_eq!(std::fs::read_to_string(v.root().join(&back)).unwrap(), "original\n");
}

#[test]
fn purge_respects_retention_and_manual_mode() {
    let t = TempDir::new();
    let v = Vault::open(t.path()).unwrap();
    let meta = meta_for(&v, "inbox/old.md", "old scrap\n");
    trash_note(&v, &meta).unwrap();

    // manual-only never purges
    assert_eq!(purge_older_than(&v, None).unwrap(), 0);
    assert_eq!(list_trash(&v).len(), 1);
    // 0-day retention purges everything deleted before "now"
    assert_eq!(purge_older_than(&v, Some(0)).unwrap(), 1);
    assert!(list_trash(&v).is_empty());
}

#[test]
fn recovery_journal_round_trip() {
    let t = TempDir::new();
    let v = Vault::open(t.path()).unwrap();
    let id = jd_core::vault::scan::synthetic_id(std::path::Path::new("inbox/x.md"));
    journal_buffer(&v, id, "unsaved keystrokes").unwrap();
    let pending = pending_recoveries(&v);
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0], (id, "unsaved keystrokes".to_owned()));
    clear_buffer(&v, id);
    assert!(pending_recoveries(&v).is_empty());
}
```

- [ ] **Step 2: RED** — compile error.

- [ ] **Step 3: Implement** both modules:

```rust
// trash.rs
//! .junkdrawer/trash/: <ULID>.md (the note bytes) + <ULID>.meta (3 lines:
//! original rel path / deleted-at RFC3339 / display line). Disposable state.

use std::path::{Path, PathBuf};

use crate::error::IoError;
use crate::id::NoteId;
use crate::note::NoteMeta;
use crate::time::Timestamp;
use crate::vault::io::filename_for;
use crate::vault::Vault;

pub struct TrashEntry {
    pub id: NoteId,
    pub title_or_first_line: String,
    pub deleted: Timestamp,
}

fn trash_dir(vault: &Vault) -> PathBuf {
    vault.abs(Path::new(".junkdrawer/trash"))
}

pub fn trash_note(vault: &Vault, meta: &NoteMeta) -> Result<(), IoError> {
    let dir = trash_dir(vault);
    let src = vault.abs(&meta.rel_path);
    let dst = dir.join(format!("{}.md", meta.id));
    std::fs::rename(&src, &dst).map_err(IoError::wrap("move to trash", &src))?;
    let display = meta.title.clone().unwrap_or_else(|| meta.first_line.clone());
    let sidecar = format!(
        "{}\n{}\n{}\n",
        meta.rel_path.display(),
        Timestamp::now().to_rfc3339(),
        display
    );
    let side_path = dir.join(format!("{}.meta", meta.id));
    std::fs::write(&side_path, sidecar).map_err(IoError::wrap("record trash entry", &side_path))
}

pub fn list_trash(vault: &Vault) -> Vec<TrashEntry> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(trash_dir(vault)) else { return out };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().is_some_and(|e| e == "meta") {
            let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else { continue };
            let Ok(id) = NoteId::parse(stem) else { continue };
            let Ok(side) = std::fs::read_to_string(&path) else { continue };
            let mut lines = side.lines();
            let _orig = lines.next();
            let deleted = lines
                .next()
                .and_then(|l| Timestamp::parse_rfc3339(l).ok())
                .unwrap_or(Timestamp(0));
            let display = lines.next().unwrap_or("").to_owned();
            out.push(TrashEntry { id, title_or_first_line: display, deleted });
        }
    }
    out.sort_by_key(|e| std::cmp::Reverse((e.deleted, e.id)));
    out
}

pub fn restore(vault: &Vault, id: NoteId) -> Result<PathBuf, IoError> {
    let dir = trash_dir(vault);
    let side_path = dir.join(format!("{id}.meta"));
    let side = std::fs::read_to_string(&side_path).map_err(IoError::wrap("read trash entry", &side_path))?;
    let orig_rel = PathBuf::from(side.lines().next().unwrap_or_default());
    let orig_dir = orig_rel.parent().unwrap_or_else(|| Path::new("notes"));
    let stem = orig_rel.file_stem().and_then(|s| s.to_str()).unwrap_or("Untitled");
    let dst_abs = filename_for(stem, id, &vault.abs(orig_dir));
    let src = dir.join(format!("{id}.md"));
    std::fs::rename(&src, &dst_abs).map_err(IoError::wrap("restore from trash", &src))?;
    let _ = std::fs::remove_file(&side_path);
    Ok(vault.rel(&dst_abs).unwrap_or(orig_rel))
}

/// None = manual only (never purge). Returns how many notes were purged.
pub fn purge_older_than(vault: &Vault, days: Option<u32>) -> Result<usize, IoError> {
    let Some(days) = days else { return Ok(0) };
    let cutoff = Timestamp(Timestamp::now().0 - i64::from(days) * 86_400_000);
    let mut purged = 0;
    for entry in list_trash(vault) {
        if entry.deleted <= cutoff {
            let dir = trash_dir(vault);
            let _ = std::fs::remove_file(dir.join(format!("{}.md", entry.id)));
            let _ = std::fs::remove_file(dir.join(format!("{}.meta", entry.id)));
            purged += 1;
        }
    }
    Ok(purged)
}
```

```rust
// recovery.rs
//! .junkdrawer/recovery/: journaled unsaved buffers so a crash loses nothing,
//! including the autosave debounce window (spec §3).

use std::path::{Path, PathBuf};

use crate::error::IoError;
use crate::id::NoteId;
use crate::vault::io::atomic_save;
use crate::vault::Vault;

fn buffer_path(vault: &Vault, id: NoteId) -> PathBuf {
    vault.abs(Path::new(".junkdrawer/recovery")).join(format!("{id}.md"))
}

pub fn journal_buffer(vault: &Vault, id: NoteId, content: &str) -> Result<(), IoError> {
    atomic_save(&buffer_path(vault, id), content)
}

pub fn clear_buffer(vault: &Vault, id: NoteId) {
    let _ = std::fs::remove_file(buffer_path(vault, id));
}

/// Checked at startup: buffers that outlived their session.
pub fn pending_recoveries(vault: &Vault) -> Vec<(NoteId, String)> {
    let dir = vault.abs(Path::new(".junkdrawer/recovery"));
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else { return out };
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else { continue };
        let Ok(id) = NoteId::parse(stem) else { continue };
        if let Ok(content) = std::fs::read_to_string(&path) {
            out.push((id, content));
        }
    }
    out.sort_by_key(|(id, _)| *id);
    out
}
```

- [ ] **Step 4: GREEN** — 4 passed; full gate.
- [ ] **Step 5: Commit** — `feat(core): trash lifecycle and recovery journal`

---

### Task 5: `vault/watcher.rs` — notify + debounce + editor zoo

**Files:**
- Create: `crates/jd-core/src/vault/watcher.rs`, `crates/jd-core/tests/vault_watcher.rs`
- Modify: `crates/jd-core/Cargo.toml` (add `notify` — `cargo add notify -p jd-core`), `crates/jd-core/src/vault/mod.rs` (add `pub mod watcher;`)

**Interfaces (arch §2.10 + decision #2):**
- `WatchEvent { Changed(PathBuf), Removed(PathBuf), Renamed { from, to } }` (rel paths).
- `VaultWatcher::start(vault, tx: mpsc::Sender<WatchEvent>) -> Result<VaultWatcher, WatchError>`; dropping the `VaultWatcher` stops watching.
- Debounce ~200 ms; coalesce bursts per path; filter to `.md` under `inbox/`/`notes/` only (never `.junkdrawer/`, dot-files, our temp files); existence-based flush (`Changed` if the path exists, `Removed` if not).

**notify API note:** the brief's code targets notify's closure-watcher API (`notify::recommended_watcher(|res: Result<Event, Error>| …)` + `.watch(root, RecursiveMode::Recursive)`), stable across notify 5–8. If the resolved version differs, adapt mechanically — the pinned contract is the `WatchEvent` semantics the tests enforce, not notify's types.

- [ ] **Step 1: Write the failing tests**

```rust
//! Watcher contract (spec §2 "external edits are legal", §13 editor zoo).
//! Platform FS latency varies wildly: tests poll with generous deadlines and
//! assert semantics, not exact event counts.

mod common;

use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use common::TempDir;
use jd_core::vault::watcher::{VaultWatcher, WatchEvent};
use jd_core::vault::Vault;

/// Collect events until `pred` is satisfied or 3 s pass. Returns all seen.
fn wait_for(rx: &mpsc::Receiver<WatchEvent>, pred: impl Fn(&[WatchEvent]) -> bool) -> Vec<WatchEvent> {
    let deadline = Instant::now() + Duration::from_secs(3);
    let mut seen = Vec::new();
    while Instant::now() < deadline {
        if let Ok(ev) = rx.recv_timeout(Duration::from_millis(50)) {
            seen.push(ev);
        }
        if pred(&seen) {
            break;
        }
    }
    seen
}

fn changed_paths(evs: &[WatchEvent]) -> Vec<&Path> {
    evs.iter()
        .filter_map(|e| match e {
            WatchEvent::Changed(p) => Some(p.as_path()),
            _ => None,
        })
        .collect()
}

fn setup() -> (TempDir, Vault, VaultWatcher, mpsc::Receiver<WatchEvent>) {
    let t = TempDir::new();
    let v = Vault::open(t.path()).unwrap();
    let (tx, rx) = mpsc::channel();
    let w = VaultWatcher::start(&v, tx).unwrap();
    // let the OS watcher settle before mutating
    std::thread::sleep(Duration::from_millis(250));
    (t, v, w, rx)
}

#[test]
fn direct_write_is_changed() {
    let (t, _v, _w, rx) = setup();
    std::fs::write(t.path().join("notes/new.md"), "# New\n").unwrap();
    let evs = wait_for(&rx, |s| !changed_paths(s).is_empty());
    assert!(changed_paths(&evs).contains(&Path::new("notes/new.md")), "{evs:?}");
}

#[test]
fn rename_swap_save_normalizes_to_changed() {
    // vim-style: write a sidecar, rename over the target
    let (t, _v, _w, rx) = setup();
    let target = t.path().join("notes/note.md");
    std::fs::write(&target, "v1\n").unwrap();
    let _ = wait_for(&rx, |s| !changed_paths(s).is_empty());

    let sidecar = t.path().join("notes/note.md.swp-like");
    std::fs::write(&sidecar, "v2\n").unwrap();
    std::fs::rename(&sidecar, &target).unwrap();
    let evs = wait_for(&rx, |s| changed_paths(s).contains(&Path::new("notes/note.md")));
    assert!(changed_paths(&evs).contains(&Path::new("notes/note.md")), "{evs:?}");
    // the sidecar itself must not surface (non-.md name)
    assert!(!evs.iter().any(|e| matches!(e, WatchEvent::Changed(p) | WatchEvent::Removed(p) if p.to_string_lossy().contains("swp"))));
}

#[test]
fn truncate_rewrite_is_changed_and_delete_is_removed() {
    let (t, _v, _w, rx) = setup();
    let target = t.path().join("inbox/scrap.md");
    std::fs::write(&target, "first\n").unwrap();
    let _ = wait_for(&rx, |s| !changed_paths(s).is_empty());

    std::fs::write(&target, "rewritten\n").unwrap(); // truncate+rewrite
    let evs = wait_for(&rx, |s| changed_paths(s).contains(&Path::new("inbox/scrap.md")));
    assert!(changed_paths(&evs).contains(&Path::new("inbox/scrap.md")));

    std::fs::remove_file(&target).unwrap();
    let evs = wait_for(&rx, |s| {
        s.iter().any(|e| matches!(e, WatchEvent::Removed(p) if p == Path::new("inbox/scrap.md")))
    });
    assert!(evs.iter().any(|e| matches!(e, WatchEvent::Removed(p) if p == Path::new("inbox/scrap.md"))), "{evs:?}");
}

#[test]
fn create_then_rename_lands_on_the_final_name() {
    let (t, _v, _w, rx) = setup();
    let tmp_name = t.path().join("notes/draft.md");
    std::fs::write(&tmp_name, "content\n").unwrap();
    std::fs::rename(&tmp_name, t.path().join("notes/Final.md")).unwrap();
    let evs = wait_for(&rx, |s| changed_paths(s).contains(&Path::new("notes/Final.md")));
    assert!(changed_paths(&evs).contains(&Path::new("notes/Final.md")), "{evs:?}");
}

#[test]
fn junkdrawer_and_tempfiles_are_invisible() {
    let (t, _v, _w, rx) = setup();
    std::fs::write(t.path().join(".junkdrawer/session/state.jd"), "x").unwrap();
    std::fs::write(t.path().join("notes/.real.md.jd-tmp"), "x").unwrap();
    std::fs::write(t.path().join("notes/visible.md"), "x").unwrap();
    let evs = wait_for(&rx, |s| !changed_paths(s).is_empty());
    for e in &evs {
        let p = match e {
            WatchEvent::Changed(p) | WatchEvent::Removed(p) => p,
            WatchEvent::Renamed { to, .. } => to,
        };
        assert_eq!(p, Path::new("notes/visible.md"), "leaked event: {e:?}");
    }
}

#[test]
fn burst_coalesces_after_quiet() {
    let (t, _v, _w, rx) = setup();
    let target = t.path().join("notes/busy.md");
    for i in 0..10 {
        std::fs::write(&target, format!("rev {i}\n")).unwrap();
        std::thread::sleep(Duration::from_millis(10));
    }
    // wait out the debounce, then drain
    std::thread::sleep(Duration::from_millis(600));
    let evs = wait_for(&rx, |s| !s.is_empty());
    let for_busy = changed_paths(&evs).iter().filter(|p| **p == Path::new("notes/busy.md")).count();
    assert!(for_busy >= 1, "burst produced nothing: {evs:?}");
    assert!(for_busy <= 3, "debounce isn't coalescing (got {for_busy}): {evs:?}");
}
```

- [ ] **Step 2: RED** — `cargo add notify -p jd-core` first, then compile error on the missing module.

- [ ] **Step 3: Implement**

```rust
//! notify wrapper: raw FS events → 200 ms debounce → coalesced,
//! existence-based WatchEvents (decision #2). The `.md`-under-inbox/notes
//! filter lives HERE so consumers never see machine-state noise.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use notify::{RecursiveMode, Watcher};

use crate::error::WatchError;
use crate::vault::io::is_our_tempfile;
use crate::vault::Vault;

const DEBOUNCE: Duration = Duration::from_millis(200);

#[derive(Clone, Debug, PartialEq)]
pub enum WatchEvent {
    Changed(PathBuf),
    Removed(PathBuf),
    /// Best-effort (decision #2): consumers must also handle a rename arriving
    /// as Removed(from) + Changed(to).
    Renamed { from: PathBuf, to: PathBuf },
}

pub struct VaultWatcher {
    // keep the notify watcher alive; drop = stop
    _watcher: notify::RecommendedWatcher,
}

/// True if this rel path is a note we care about.
fn is_note_path(rel: &Path) -> bool {
    let under_note_dirs = rel.starts_with("inbox") || rel.starts_with("notes");
    let name = rel.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    under_note_dirs
        && rel.extension().is_some_and(|e| e == "md")
        && !name.starts_with('.')
        && !is_our_tempfile(&name)
}

impl VaultWatcher {
    pub fn start(vault: &Vault, tx: mpsc::Sender<WatchEvent>) -> Result<VaultWatcher, WatchError> {
        let (raw_tx, raw_rx) = mpsc::channel::<PathBuf>();
        let root = vault.root().to_owned();

        let mut watcher = notify::recommended_watcher(move |res: Result<notify::Event, notify::Error>| {
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
                    let timeout = if pending.is_empty() { Duration::from_secs(3600) } else { Duration::from_millis(50) };
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
                        .filter(|(_, &t)| now.duration_since(t) >= DEBOUNCE)
                        .map(|(p, _)| p.clone())
                        .collect();
                    for abs in ready {
                        pending.remove(&abs);
                        let Some(rel) = abs.strip_prefix(&root).ok().map(Path::to_owned) else { continue };
                        if !is_note_path(&rel) {
                            continue;
                        }
                        let event = if abs.exists() { WatchEvent::Changed(rel) } else { WatchEvent::Removed(rel) };
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
```

- [ ] **Step 4: GREEN** — `cargo test -p jd-core --test vault_watcher` → 6 passed (run it twice locally to shake out timing flakes; a flake means the TEST needs a looser bound or longer deadline, per Global Constraints). Full gate.
- [ ] **Step 5: Commit** — `feat(core): vault watcher with debounced existence-based events`

---

### Task 6: `worker.rs` — the vault worker thread

**Files:**
- Create: `crates/jd-core/src/worker.rs`, `crates/jd-core/tests/vault_worker.rs`
- Modify: `crates/jd-core/src/lib.rs` (add `pub mod worker;`)

**Interfaces (arch §2.12, WP1d subset — WP1e refactors commands into `Op{VaultOp}`):**

```rust
pub enum Dest { Inbox, Notes }
pub enum VaultCommand {
    Create { seed: NewNote, dest: Dest },
    SaveBody { id: NoteId, content: String },   // full body text; frontmatter managed by the worker
    ReadBody { id: NoteId },
    JournalBuffer { id: NoteId, content: String },
    PurgeTrash { older_than_days: Option<u32> },
    RescanAll,
    Shutdown,
}
pub enum VaultEvent {
    Created { meta: NoteMeta },
    Saved { id: NoteId },
    Body { id: NoteId, content: String },
    External { changed: Vec<NoteId>, removed: Vec<NoteId> },
    Conflict { id: NoteId, conflict_copy: PathBuf },
    ScanProgress { done: usize, total: usize },
    ScanComplete { quarantined_count: usize },
    Error { context: String, message: String },
}
pub struct VaultHandle {
    pub commands: mpsc::Sender<VaultCommand>,
    pub events: mpsc::Receiver<VaultEvent>,
    pub index: SharedIndex,
}
pub fn start(vault: Vault, wake: Box<dyn Fn() + Send + Sync>) -> Result<VaultHandle, CoreError>;
```

Behavior contract:
- `start` spawns: forwarder (public commands → internal `WorkerMsg`), watcher+debouncer (Task 5, feeding internal channel), and the worker thread, which FIRST runs the initial scan (emitting `ScanProgress` per ~64 files and `ScanComplete`), populating the index, then loops.
- `Create`: id from worker-owned `IdGen`; `filename_for` from the seed's first line (or "Untitled"); synthesize frontmatter (status per dest, kind/source/tags applied via setters); `atomic_save`; ledger-record; index upsert; emit `Created`.
- `SaveBody`: look up rel path via index; **conflict check (decision #5)** — if the file's (len, mtime) differs from the ledger entry AND the ledger has one, divert to a conflict copy and emit `Conflict`; else parse existing file, replace body, `set_modified(now)` (synthesizing frontmatter with the note's current id if the file has none — decision #1 handoff), atomic_save, ledger-record, index upsert, `clear_buffer`, emit `Saved`.
- `ReadBody`: read + parse, emit `Body` (spec §3: bodies load on demand; the UI never touches the FS).
- Watch events: `Changed` → ledger-match ⇒ drop (echo, decision #4); else re-parse + upsert + emit `External`. `Removed`/`Renamed` → index remove(s)/re-add + `External`.
- `wake` is called after EVERY event posted (the egui repaint hook).
- `Shutdown` (and the command sender dropping) ends the worker cleanly.

- [ ] **Step 1: Write the failing tests**

```rust
//! Worker contract: serialized writes, echo suppression, conflict copies,
//! event flow. Uses real files + the real watcher.

mod common;

use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use common::TempDir;
use jd_core::note::{Kind, NewNote, Status};
use jd_core::vault::Vault;
use jd_core::worker::{start, Dest, VaultCommand, VaultEvent, VaultHandle};

fn boot(t: &TempDir) -> (VaultHandle, Arc<Mutex<u32>>) {
    let v = Vault::open(t.path()).unwrap();
    let wakes = Arc::new(Mutex::new(0u32));
    let w = wakes.clone();
    let h = start(v, Box::new(move || *w.lock().unwrap() += 1)).unwrap();
    (h, wakes)
}

fn drain_until<T>(h: &VaultHandle, mut pick: impl FnMut(&VaultEvent) -> Option<T>) -> T {
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if let Ok(ev) = h.events.recv_timeout(Duration::from_millis(50)) {
            if let Some(t) = pick(&ev) {
                return t;
            }
        }
    }
    panic!("expected event never arrived");
}

fn scrap(body: &str) -> NewNote {
    NewNote {
        body: body.to_owned(),
        status: Status::Fleeting,
        kind: Kind::Note,
        source: None,
        tags: vec![],
    }
}

#[test]
fn boot_scans_existing_notes_into_the_index() {
    let t = TempDir::new();
    {
        let v = Vault::open(t.path()).unwrap();
        std::fs::write(v.root().join("notes/Pre.md"), "# Pre\nexisting\n").unwrap();
    }
    let (h, wakes) = boot(&t);
    drain_until(&h, |e| matches!(e, VaultEvent::ScanComplete { .. }).then_some(()));
    assert_eq!(h.index.read().unwrap().count(), 1);
    assert!(*wakes.lock().unwrap() >= 1, "wake must fire on events");
}

#[test]
fn create_writes_a_file_and_indexes_it() {
    let t = TempDir::new();
    let (h, _) = boot(&t);
    drain_until(&h, |e| matches!(e, VaultEvent::ScanComplete { .. }).then_some(()));

    h.commands.send(VaultCommand::Create { seed: scrap("a fresh thought\n"), dest: Dest::Inbox }).unwrap();
    let meta = drain_until(&h, |e| match e {
        VaultEvent::Created { meta } => Some(meta.clone()),
        _ => None,
    });
    assert!(meta.rel_path.starts_with("inbox"));
    assert_eq!(meta.status, Status::Fleeting);
    let on_disk = std::fs::read_to_string(t.path().join(&meta.rel_path)).unwrap();
    assert!(on_disk.contains("a fresh thought"));
    assert!(on_disk.starts_with("---\n"), "frontmatter synthesized");
    assert!(h.index.read().unwrap().get(meta.id).is_some());
}

#[test]
fn save_body_preserves_frontmatter_and_updates_modified() {
    let t = TempDir::new();
    let (h, _) = boot(&t);
    drain_until(&h, |e| matches!(e, VaultEvent::ScanComplete { .. }).then_some(()));
    h.commands.send(VaultCommand::Create { seed: scrap("v1\n"), dest: Dest::Inbox }).unwrap();
    let meta = drain_until(&h, |e| match e {
        VaultEvent::Created { meta } => Some(meta.clone()),
        _ => None,
    });

    h.commands.send(VaultCommand::SaveBody { id: meta.id, content: "v2 body\n".into() }).unwrap();
    drain_until(&h, |e| matches!(e, VaultEvent::Saved { id } if *id == meta.id).then_some(()));
    let on_disk = std::fs::read_to_string(t.path().join(&meta.rel_path)).unwrap();
    assert!(on_disk.contains("v2 body"));
    assert!(on_disk.contains(&meta.id.to_string()), "id survives body saves");
    assert!(!on_disk.contains("v1"));
}

#[test]
fn read_body_round_trips() {
    let t = TempDir::new();
    let (h, _) = boot(&t);
    drain_until(&h, |e| matches!(e, VaultEvent::ScanComplete { .. }).then_some(()));
    h.commands.send(VaultCommand::Create { seed: scrap("the body\n"), dest: Dest::Notes }).unwrap();
    let meta = drain_until(&h, |e| match e {
        VaultEvent::Created { meta } => Some(meta.clone()),
        _ => None,
    });
    h.commands.send(VaultCommand::ReadBody { id: meta.id }).unwrap();
    let body = drain_until(&h, |e| match e {
        VaultEvent::Body { id, content } if *id == meta.id => Some(content.clone()),
        _ => None,
    });
    assert!(body.contains("the body"));
}

#[test]
fn our_own_saves_do_not_echo_as_external() {
    let t = TempDir::new();
    let (h, _) = boot(&t);
    drain_until(&h, |e| matches!(e, VaultEvent::ScanComplete { .. }).then_some(()));
    h.commands.send(VaultCommand::Create { seed: scrap("mine\n"), dest: Dest::Inbox }).unwrap();
    let meta = drain_until(&h, |e| match e {
        VaultEvent::Created { meta } => Some(meta.clone()),
        _ => None,
    });
    h.commands.send(VaultCommand::SaveBody { id: meta.id, content: "mine v2\n".into() }).unwrap();
    drain_until(&h, |e| matches!(e, VaultEvent::Saved { .. }).then_some(()));

    // wait past the debounce window; no External event for our own write
    let deadline = Instant::now() + Duration::from_millis(800);
    while Instant::now() < deadline {
        if let Ok(ev) = h.events.recv_timeout(Duration::from_millis(50)) {
            assert!(
                !matches!(&ev, VaultEvent::External { changed, .. } if changed.contains(&meta.id)),
                "self-echo leaked: {ev:?}"
            );
        }
    }
}

#[test]
fn external_edits_reindex_and_emit() {
    let t = TempDir::new();
    let (h, _) = boot(&t);
    drain_until(&h, |e| matches!(e, VaultEvent::ScanComplete { .. }).then_some(()));
    h.commands.send(VaultCommand::Create { seed: scrap("watch me\n"), dest: Dest::Notes }).unwrap();
    let meta = drain_until(&h, |e| match e {
        VaultEvent::Created { meta } => Some(meta.clone()),
        _ => None,
    });

    // an "external tool" rewrites the file
    std::thread::sleep(Duration::from_millis(300));
    std::fs::write(t.path().join(&meta.rel_path), "# Retitled\nexternally edited\n").unwrap();
    drain_until(&h, |e| {
        matches!(e, VaultEvent::External { changed, .. } if changed.contains(&meta.id)).then_some(())
    });
    let ix = h.index.read().unwrap();
    assert_eq!(ix.get(meta.id).unwrap().title.as_deref(), Some("Retitled"));
}

#[test]
fn concurrent_external_edit_diverts_to_conflict_copy() {
    let t = TempDir::new();
    let (h, _) = boot(&t);
    drain_until(&h, |e| matches!(e, VaultEvent::ScanComplete { .. }).then_some(()));
    h.commands.send(VaultCommand::Create { seed: scrap("base\n"), dest: Dest::Notes }).unwrap();
    let meta = drain_until(&h, |e| match e {
        VaultEvent::Created { meta } => Some(meta.clone()),
        _ => None,
    });

    // sneak an external change under the worker (bypassing its ledger),
    // ensuring a different mtime/len than the ledger recorded
    std::thread::sleep(Duration::from_millis(50));
    std::fs::write(t.path().join(&meta.rel_path), "theirs — changed externally\n").unwrap();

    h.commands.send(VaultCommand::SaveBody { id: meta.id, content: "ours\n".into() }).unwrap();
    let copy = drain_until(&h, |e| match e {
        VaultEvent::Conflict { id, conflict_copy } if *id == meta.id => Some(conflict_copy.clone()),
        _ => None,
    });
    // both versions survive (spec §2: never silently clobber either side)
    assert_eq!(
        std::fs::read_to_string(t.path().join(&meta.rel_path)).unwrap(),
        "theirs — changed externally\n"
    );
    let ours = std::fs::read_to_string(t.path().join(&copy)).unwrap();
    assert!(ours.contains("ours"));
}

#[test]
fn shutdown_is_clean() {
    let t = TempDir::new();
    let (h, _) = boot(&t);
    drain_until(&h, |e| matches!(e, VaultEvent::ScanComplete { .. }).then_some(()));
    h.commands.send(VaultCommand::Shutdown).unwrap();
    // after shutdown the events channel eventually disconnects
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        match h.events.recv_timeout(Duration::from_millis(50)) {
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
            _ if Instant::now() > deadline => panic!("worker did not shut down"),
            _ => {}
        }
    }
}
```

- [ ] **Step 2: RED** — compile error.

- [ ] **Step 3: Implement** `crates/jd-core/src/worker.rs` — the full worker per the behavior contract above. Structure (write it exactly in this shape; the bodies follow the contract):

```rust
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
use crate::vault::io::{atomic_save, filename_for};
use crate::vault::recovery::{clear_buffer, journal_buffer};
use crate::vault::scan::{parse_note_file, scan};
use crate::vault::trash::purge_older_than;
use crate::vault::watcher::{VaultWatcher, WatchEvent};
use crate::vault::Vault;

pub enum Dest { Inbox, Notes }
// … VaultCommand, VaultEvent, VaultHandle exactly as the interface block above …

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
    std::thread::Builder::new().name("jd-fwd".into()).spawn(move || {
        while let Ok(cmd) = cmd_rx.recv() {
            let stop = matches!(cmd, VaultCommand::Shutdown);
            if fwd.send(WorkerMsg::Cmd(cmd)).is_err() || stop {
                return;
            }
        }
    }).map_err(|e| CoreError::Io(crate::error::IoError { path: "<threads>".into(), op: "spawn", source: e }))?;

    // watcher → internal msg channel
    let (watch_tx, watch_rx) = mpsc::channel::<WatchEvent>();
    let watcher = VaultWatcher::start(&vault, watch_tx)?;
    let wfwd = msg_tx;
    std::thread::Builder::new().name("jd-watch-fwd".into()).spawn(move || {
        while let Ok(ev) = watch_rx.recv() {
            if wfwd.send(WorkerMsg::Watch(ev)).is_err() {
                return;
            }
        }
    }).map_err(|e| CoreError::Io(crate::error::IoError { path: "<threads>".into(), op: "spawn", source: e }))?;

    let worker_index = index.clone();
    std::thread::Builder::new().name("jd-worker".into()).spawn(move || {
        let _watcher = watcher; // owned by the worker; dropped on exit
        let mut gen = IdGen::new();
        let mut ledger: WriteLedger = HashMap::new();
        let emit = |ev: VaultEvent| {
            let _ = event_tx.send(ev);
            wake();
        };

        run_initial_scan(&vault, &worker_index, &emit);

        while let Ok(msg) = msg_rx.recv() {
            match msg {
                WorkerMsg::Cmd(VaultCommand::Shutdown) => return,
                WorkerMsg::Cmd(cmd) => handle_command(&vault, &worker_index, &mut gen, &mut ledger, &emit, cmd),
                WorkerMsg::Watch(ev) => handle_watch(&vault, &worker_index, &mut ledger, &emit, ev),
            }
        }
    }).map_err(|e| CoreError::Io(crate::error::IoError { path: "<threads>".into(), op: "spawn", source: e }))?;

    Ok(VaultHandle { commands: cmd_tx, events: event_rx, index })
}
```

Then the four helpers, in full:

- `run_initial_scan`: `scan(vault, &progress)` where progress sends `ScanProgress` every 64th file (and the last); upsert every meta+body under ONE write lock; emit `ScanComplete { quarantined_count }`.
- `handle_command`:
  - `Create`: title = first `# ` heading of seed body else first line; `filename_for(display, id, &dir)`; build `FrontmatterDoc::synthesize(id, now, seed.status)` + `set_kind`/`set_source`/`set_tags` as provided; `NoteDoc { fm, body }.serialize()`; `atomic_save`; `ledger.insert(rel, stat(abs))`; `parse_note_file` → upsert → `Created`.
  - `SaveBody`: rel from index (else `Error`); conflict check per decision #5 (ledger entry present AND current stat ≠ ledger → conflict path `"{stem} (conflict {YYYY-MM-DD HHMM}).md"` via `Timestamp::now().to_rfc3339()` reformat, `atomic_save` ours there, emit `Conflict`, and ALSO upsert the conflict copy as a new note so it shows in Needs Attention later); happy path: read file (missing → synthesize), `NoteDoc::parse`, if `fm` is empty synthesize with the note's current id + created=now + current status from index, replace body, `fm.set_modified(Timestamp::now())`, serialize, `atomic_save`, ledger update, re-parse → upsert, `clear_buffer(vault, id)`, `Saved`.
  - `ReadBody`: rel from index; `read_to_string` → `NoteDoc::parse` → `Body { id, content: doc.body }` (or `Error`).
  - `JournalBuffer`: `journal_buffer(...)`, no event on success.
  - `PurgeTrash`: `purge_older_than`, no event on success.
  - `RescanAll`: clear index (fresh `Index::new()` swapped in under the write lock) + `run_initial_scan`.
- `handle_watch`:
  - `Changed(rel)`: if ledger entry matches current stat → drop (echo). Else `parse_note_file` → upsert (removing any previous note at that path with a DIFFERENT id first — path reuse) → `External { changed: vec![id], removed: prior_id_if_different }`.
  - `Removed(rel)`: find note by rel_path in index → remove → `External { removed }`.
  - `Renamed { from, to }`: treat as Removed(from) + Changed(to).

All error paths emit `VaultEvent::Error { context, message }` (message = `CoreError`/`IoError` Display) and never panic the worker.

- [ ] **Step 4: GREEN** — `cargo test -p jd-core --test vault_worker` → 8 passed (these are timing-sensitive; run twice). Full gate.
- [ ] **Step 5: Commit** — `feat(core): vault worker with echo suppression and conflict copies`

---

### Task 7: Performance budgets + CI release step

**Files:**
- Create: `crates/jd-core/tests/perf.rs`
- Modify: `.github/workflows/ci.yml` (add the release perf step)

**Requirements (spec §13):** on a synthetic 20k-note vault: cold `scan` < 1 s · single-file incremental reindex (parse + `Index::upsert`) < 5 ms · palette `query` < 10 ms. Budgets are `#[test]`s so drift is a red build. Release-mode only (decision #6). Also probe the three WP1c-flagged hot spots (large-tag query, `similar()` on a high-degree note) under the same 10 ms budget.

- [ ] **Step 1: Write the test file**

```rust
//! Performance budgets (spec §13) — the tripwire that legally activates the
//! §3 snapshot escape hatch. Release-mode only: debug runs skip via ignore.

mod common;

use std::time::Instant;

use common::TempDir;
use jd_core::index::search::parse_query;
use jd_core::index::Index;
use jd_core::rng::Xorshift128;
use jd_core::vault::scan::{parse_note_file, scan};
use jd_core::vault::Vault;

const NOTES: usize = 20_000;

const WORDS: &[&str] = &[
    "zettelkasten", "method", "note", "thought", "link", "idea", "permanent", "fleeting",
    "structure", "argument", "claim", "evidence", "writing", "reading", "memory", "system",
    "practice", "review", "connect", "emerge", "context", "question", "answer", "draft",
];

fn build_synthetic_vault() -> (TempDir, Vault) {
    let t = TempDir::new();
    let v = Vault::open(t.path()).unwrap();
    let mut rng = Xorshift128::new(0x20_000);
    for i in 0..NOTES {
        let mut body = format!("# Note {i}\n\n");
        for _ in 0..80 {
            body.push_str(WORDS[rng.gen_range(0..WORDS.len() as u64) as usize]);
            body.push(' ');
        }
        body.push_str(&format!("\n\nSee [[Note {}]] and [[Note {}]].\n#tag{} #shared\n",
            rng.gen_range(0..NOTES as u64), rng.gen_range(0..NOTES as u64), i % 50));
        std::fs::write(t.path().join(format!("notes/Note {i}.md")), body).unwrap();
    }
    (t, v)
}

fn build_index(v: &Vault) -> Index {
    let out = scan(v, &|_, _| {}).unwrap();
    let mut ix = Index::new();
    for (meta, body) in out.metas {
        ix.upsert(meta, &body);
    }
    ix
}

#[test]
#[cfg_attr(debug_assertions, ignore = "perf budgets are release-mode only")]
fn cold_scan_under_one_second() {
    let (_t, v) = build_synthetic_vault();
    let start = Instant::now();
    let out = scan(&v, &|_, _| {}).unwrap();
    let elapsed = start.elapsed();
    assert_eq!(out.metas.len(), NOTES);
    assert!(elapsed.as_millis() < 1000, "cold scan took {elapsed:?} (budget 1s)");
}

#[test]
#[cfg_attr(debug_assertions, ignore = "perf budgets are release-mode only")]
fn incremental_reindex_under_five_ms() {
    let (_t, v) = build_synthetic_vault();
    let mut ix = build_index(&v);
    let rel = std::path::Path::new("notes/Note 100.md");
    std::fs::write(v.root().join(rel), "# Note 100\nedited body #shared\n").unwrap();
    let start = Instant::now();
    let (meta, body) = parse_note_file(&v, rel).unwrap();
    ix.upsert(meta, &body);
    let elapsed = start.elapsed();
    assert!(elapsed.as_micros() < 5000, "reindex took {elapsed:?} (budget 5ms)");
}

#[test]
#[cfg_attr(debug_assertions, ignore = "perf budgets are release-mode only")]
fn queries_under_ten_ms() {
    let (_t, v) = build_synthetic_vault();
    let ix = build_index(&v);
    for q in ["zettelkasten method", "\"permanent note\"", "writing -draft", "argu"] {
        let start = Instant::now();
        let _ = ix.query(&parse_query(q), 20);
        let elapsed = start.elapsed();
        assert!(elapsed.as_micros() < 10_000, "query {q:?} took {elapsed:?} (budget 10ms)");
    }
    // WP1c-flagged hot spots under the same budget:
    let start = Instant::now();
    let _ = ix.query(&parse_query("#shared zettelkasten"), 20); // ~20k-member tag filter
    assert!(start.elapsed().as_micros() < 10_000, "large-tag query blew the budget");

    let any_id = ix.iter_meta().next().unwrap().id;
    let start = Instant::now();
    let _ = ix.similar(any_id, 8);
    assert!(start.elapsed().as_millis() < 50, "similar() took {:?} (soft 50ms bound)", start.elapsed());
}
```

- [ ] **Step 2: Run in release** — `cargo test -p jd-core --release --test perf` → 3 passed. **If a budget fails: this is the §3 tripwire — do NOT weaken the budget. Report DONE_WITH_CONCERNS with the numbers; the controller decides between optimization and the sanctioned snapshot escape hatch.** In debug (`cargo test -p jd-core --test perf`) → 3 ignored.

- [ ] **Step 3: CI step** — append to `.github/workflows/ci.yml`'s `steps`:

```yaml
      - run: cargo test -p jd-core --release --test perf
```

- [ ] **Step 4: Full gate + commit** — `test(core): performance budgets on a 20k synthetic vault`

---

## Self-Review Notes

- Spec §13 vault-engine bullets covered: atomic-save torture (T2), title collisions (T2), conflict copies (T6), trash lifecycle (T4), watcher debounce + editor zoo (T5: rename-swap, truncate-rewrite, create-then-rename), perf budgets as failing tests (T7). Rename-rewrites-referrers is WP1e (it's `RenameTitle`).
- Arch §2.10/§2.12 deviations, all pinned in "Decisions": narrower `VaultCommand` set (WP1e refactor), existence-based watch semantics, `ScanComplete { quarantined_count }` instead of the full quarantine list (the list lands in the index-adjacent Needs Attention state in WP1e — the worker keeps it internal for now: NOTE, actually store it: keep `Vec<QuarantinedFile>` in the worker and add a getter later; the count is what the event carries).
- The watcher/worker tests are timing-based integration tests: generous deadlines, semantic assertions. If CI proves flaky on macOS runners, loosen deadlines first, semantics never.
- `notify` is the first dependency — Task 5 adds it; Appendix B already approves it.
