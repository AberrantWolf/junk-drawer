# WP1e — Commands, Journal, Session Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Implement `jd-core`'s command layer — every vault mutation as a `VaultOp` with a computed inverse, the session-long undo `Journal`, and per-vault `SessionState` — per architecture doc §2.11/§2.12/§2.13 and spec §9, closing milestone M1 and discharging the five WP1d review handoffs.

**Architecture:** `command.rs` (the op vocabulary + labels), `geom.rs` (Vec2), `session.rs` (desks/placements, its own journaled ops), `journal.rs` (undo/redo stacks over `InverseAction::{Vault, Session}`), and a worker refactor to `VaultCommand::Op { op, source }` executing every op and returning its inverse. Inverses are race-free because the worker serializes writes (spec §9). Trash is the safety floor; undo is a convenience layer over it.

**Tech Stack:** Rust stable, existing deps only. Branch: `feat/commands` in worktree `.worktrees/wp1e` (parallel to the CI-fix branch in the main checkout).

## Decisions Pinned by This Plan

1. **`VaultOp::Batch(Vec<VaultOp>)`** executes serially; its inverse is the reversed list of member inverses. It carries `Split`'s inverse and the promotion compound (spec §9's single-Ctrl+Z promotion).
2. **The inverse law ignores `modified` timestamps** (and file mtimes): executing an op then its inverse restores content, structure, status, titles, links, tags, and paths exactly — but `modified` legitimately moves forward. The test helper `assert_restored` encodes this.
3. **`RenameTitle` is only valid for notes with a title** (title = first `# ` heading, spec §5); on an untitled note it fails with `OpFailed`. Retitling rewrites the heading line, renames the file (`filename_for`), and rewrites `[[refs]]` in every referrer (target matched case-insensitively, `|display` preserved).
4. **Structural single-writer enforcement (WP1d handoff):** `trash_note`, `restore`, `purge_older_than`, `journal_buffer`, `clear_buffer`, and `parse_note_file` become `pub(crate)`. Their integration tests move in-crate (unit-test modules) using a new `#[cfg(test)] pub(crate) mod testutil` TempDir helper; `tests/vault_trash.rs` is deleted.
5. **Worker command surface after refactor:** `Op { op: VaultOp, source: OpSource }`, `ReadBody`, `JournalBuffer`, `PurgeTrash`, `RescanAll`, `Shutdown`. The old direct `Create`/`SaveBody` variants are REMOVED; `tests/vault_worker.rs` migrates to the op form (same scenarios, same assertions).
6. **Journal lives in jd-core as a pure data structure**; jd-app owns pushing `OpDone{source: User}` results into it (WP3). WP1e proves the loop headlessly: execute op → take inverse from `OpDone` → send it back with `source: UndoRedo` → `assert_restored`.
7. **Session file format** is the line-based `session.jd` pinned in arch §2.13; corrupt or missing → default state, never an error (disposable by design).

## Global Constraints

- Zero NEW dependencies. Every commit leaves `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace` green (run IN THE WORKTREE).
- Public signatures per architecture §2.11–2.13 as refined here; deviations require editing that doc.
- **The inverse law (spec §13): for EVERY op, execute → execute-inverse → `assert_restored`.** This is the WP's load-bearing invariant; Task 7 enforces it op-by-op.
- Worker tests are timing-sensitive: run the worker test file TWICE before committing; flakes mean looser deadlines, never looser semantics.
- Undo labels are user-facing copy (spec §9/§11 voice): "Toss scrap 'egui layouter idea'" — verb + kind-word + quoted display line, no punctuation beyond the quotes.
- TDD; RED evidence is a deliverable per task.

---

### Task 1: `command.rs` — the op vocabulary

**Files:**
- Create: `crates/jd-core/src/command.rs`
- Modify: `crates/jd-core/src/lib.rs` (add `pub mod command;`)

**Interfaces:**
- Consumes: `NoteId`, `Kind`, `Tag`, `NewNote`.
- Produces (pure data — execution lands in Tasks 4–6):

```rust
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Dest { Inbox, Notes }

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum OpSource { User, UndoRedo }

#[derive(Clone, Debug, PartialEq)]
pub enum VaultOp {
    Create { seed: NewNote, dest: Dest },
    SaveBody { id: NoteId, content: String },
    RenameTitle { id: NoteId, new_title: String },
    Promote { id: NoteId },
    Demote { id: NoteId },
    SetKind { id: NoteId, kind: Kind },
    SetSource { id: NoteId, source: Option<String> },
    SetTags { id: NoteId, tags: Vec<Tag> },
    Toss { id: NoteId },
    Delete { id: NoteId },
    Restore { id: NoteId },
    Split { id: NoteId, at_byte: usize },
    Batch(Vec<VaultOp>),
}

#[derive(Clone, Debug)]
pub struct OpResult {
    /// None only for ops that create no meaningful reversal (currently none —
    /// every listed op computes an inverse; the Option is for future ops).
    pub inverse: Option<VaultOp>,
    /// User-facing Edit-menu label, e.g. "Toss scrap 'egui layouter idea'".
    pub label: String,
    /// Ids created by this op (Create → 1, Split → 1, Batch → all).
    pub created: Vec<NoteId>,
}

/// Label vocabulary (spec §9/§11 voice): verb + kind-word + quoted display.
pub fn op_label(verb: &str, is_fleeting: bool, display: &str) -> String {
    let kind_word = if is_fleeting { "scrap" } else { "card" };
    format!("{verb} {kind_word} '{display}'")
}

/// Truncate a display line for labels at a char boundary (~40 chars + …).
pub fn label_display(line: &str) -> String;
```

- [ ] **Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn op_labels_match_the_spec_voice() {
        assert_eq!(op_label("Toss", true, "egui layouter idea"), "Toss scrap 'egui layouter idea'");
        assert_eq!(op_label("Promote", true, "titles are claims"), "Promote scrap 'titles are claims'");
        assert_eq!(op_label("Rename", false, "Egui tradeoffs"), "Rename card 'Egui tradeoffs'");
    }

    #[test]
    fn label_display_truncates_at_char_boundaries() {
        assert_eq!(label_display("short"), "short");
        let long = "x".repeat(60);
        let out = label_display(&long);
        assert!(out.chars().count() <= 41 && out.ends_with('…'));
        let multi = "é".repeat(60);
        let out = label_display(&multi);
        assert!(out.ends_with('…') && out.chars().all(|c| c == 'é' || c == '…'));
    }

    #[test]
    fn ops_are_cloneable_and_comparable() {
        let op = VaultOp::Batch(vec![
            VaultOp::Promote { id: crate::id::NoteId([1; 16]) },
            VaultOp::SaveBody { id: crate::id::NoteId([1; 16]), content: "x".into() },
        ]);
        assert_eq!(op.clone(), op);
    }
}
```

- [ ] **Step 2: RED** — compile error.
- [ ] **Step 3: Implement** — the types above verbatim plus:

```rust
const LABEL_MAX_CHARS: usize = 40;

pub fn label_display(line: &str) -> String {
    let line = line.trim();
    if line.chars().count() <= LABEL_MAX_CHARS {
        return line.to_owned();
    }
    let mut out: String = line.chars().take(LABEL_MAX_CHARS).collect();
    out.push('…');
    out
}
```

`NewNote` needs `PartialEq` for `VaultOp`'s derive — add `PartialEq` to `NewNote`'s (and if the compiler requires, `NoteMeta` is NOT involved; only `NewNote`) derive list in `note.rs`; that's the only permitted outside-file touch.

- [ ] **Step 4: GREEN** (3 tests) — full gate.
- [ ] **Step 5: Commit** — `feat(core): vault op vocabulary and label voice`

---

### Task 2: `geom.rs` + `session.rs` — desks and session state

**Files:**
- Create: `crates/jd-core/src/geom.rs`, `crates/jd-core/src/session.rs`
- Modify: `crates/jd-core/src/lib.rs` (add both)

**Interfaces (arch §2.13):**

```rust
// geom.rs — no egui; jd-app adds From/Into
#[derive(Clone, Copy, PartialEq, Debug, Default)]
pub struct Vec2 { pub x: f32, pub y: f32 }

// session.rs
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct DeskId(pub NoteId);          // reuses ULID machinery; not a note
impl DeskId { pub fn generate(gen: &mut IdGen) -> DeskId; }

#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Viewport { pub center: Vec2, pub zoom: f32 }   // Default: center 0,0 zoom 1.0

#[derive(Clone, PartialEq, Debug)]
pub struct PlacedCard { pub id: NoteId, pub pos: Vec2 }

#[derive(Clone, PartialEq, Debug)]
pub struct Desk { pub id: DeskId, pub name: String, pub viewport: Viewport, pub cards: Vec<PlacedCard> }

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SurfaceId { Desk(DeskId), Inbox, Drawer, Map, Trash }

#[derive(Clone, PartialEq, Debug, Default)]
pub struct SessionState {
    pub desks: Vec<Desk>,               // rail order
    pub current_surface: Option<SurfaceId>,
    pub open_card: Option<NoteId>,
}

#[derive(Clone, PartialEq, Debug)]
pub enum SessionOp {
    Place { desk: DeskId, id: NoteId, pos: Vec2 },
    Move { desk: DeskId, id: NoteId, from: Vec2, to: Vec2 },
    PutAway { desk: DeskId, id: NoteId, was_at: Vec2 },
    CreateDesk { id: DeskId, name: String },
    RenameDesk { id: DeskId, from: String, to: String },
    ReorderDesk { id: DeskId, from_index: usize, to_index: usize },
    DeleteDesk { desk: Desk },          // carries the full desk so the inverse can restore it
}

impl SessionState {
    /// Applies the op and returns its inverse. Inverse pairs: Place↔PutAway
    /// (PutAway's inverse is Place at the card's actual position), Move swaps
    /// from/to, CreateDesk↔DeleteDesk, Rename/Reorder swap their fields.
    /// Unknown desk/card ids are STRICT no-ops returning an inverse that is
    /// also a no-op — never generative (a PutAway on an absent card must not
    /// conjure it; the undo stack depends on this).
    pub fn apply(&mut self, op: &SessionOp) -> SessionOp;
    pub fn load(vault: &Vault) -> SessionState;                 // .junkdrawer/session/session.jd
    pub fn save(&self, vault: &Vault) -> Result<(), IoError>;   // atomic_save, caller debounces
}
```

**File format** (versioned, hand-parsed, disposable — arch §2.13):

```
jd-session 1
surface = desk 01ARZ3NDEKTSV4RRFFQ69G5FAV      (or: inbox / drawer / map / trash)
open = 01ARZ3NDEKTSV4RRFFQ69G5FA0              (optional line)
[desk 01ARZ3NDEKTSV4RRFFQ69G5FAV]
name = Reading
viewport = 120.5 -80 1.25
card = 01ARZ3NDEKTSV4RRFFQ69G5FA1 100 200
card = 01ARZ3NDEKTSV4RRFFQ69G5FA2 -40 60
```

- [ ] **Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::id::{IdGen, NoteId};

    fn nid(n: u8) -> NoteId {
        NoteId([n; 16])
    }

    fn desk_with(gen: &mut IdGen, name: &str) -> (SessionState, DeskId) {
        let mut s = SessionState::default();
        let id = DeskId::generate(gen);
        s.apply(&SessionOp::CreateDesk { id, name: name.into() });
        (s, id)
    }

    #[test]
    fn op_stack_unwinds_exactly() {
        // Forward pass accumulates state (no mid-loop inversion — each op runs
        // against the state its predecessor left); the unwind pass then proves
        // every inverse restores its exact before-state, LIFO.
        let mut gen = IdGen::new();
        let (mut s, desk) = desk_with(&mut gen, "Work");
        let ops = vec![
            SessionOp::Place { desk, id: nid(1), pos: Vec2 { x: 10.0, y: 20.0 } },
            SessionOp::Move { desk, id: nid(1), from: Vec2 { x: 10.0, y: 20.0 }, to: Vec2 { x: 50.0, y: 60.0 } },
            SessionOp::RenameDesk { id: desk, from: "Work".into(), to: "Deep Work".into() },
            SessionOp::PutAway { desk, id: nid(1), was_at: Vec2 { x: 50.0, y: 60.0 } },
        ];
        let mut stack = Vec::new();
        for op in ops {
            let before = s.clone();
            let inverse = s.apply(&op);
            assert_ne!(s, before, "op must change state: {op:?}");
            stack.push((before, inverse));
        }
        while let Some((before, inverse)) = stack.pop() {
            s.apply(&inverse);
            assert_eq!(s, before, "inverse must restore state for {inverse:?}");
        }
    }

    #[test]
    fn ops_on_absent_cards_are_no_ops() {
        // Leniency contract: session ops on unknown cards/desks change nothing
        // and return an inverse that also changes nothing. Never generative.
        let mut gen = IdGen::new();
        let (mut s, desk) = desk_with(&mut gen, "Work");
        let absent = nid(99);
        for op in [
            SessionOp::Move { desk, id: absent, from: Vec2::default(), to: Vec2 { x: 5.0, y: 5.0 } },
            SessionOp::PutAway { desk, id: absent, was_at: Vec2 { x: 5.0, y: 5.0 } },
        ] {
            let before = s.clone();
            let inverse = s.apply(&op);
            assert_eq!(s, before, "op on absent card must be a no-op: {op:?}");
            s.apply(&inverse);
            assert_eq!(s, before, "its inverse must also be a no-op: {inverse:?}");
        }
    }

    #[test]
    fn create_and_delete_desk_invert() {
        let mut gen = IdGen::new();
        let mut s = SessionState::default();
        let id = DeskId::generate(&mut gen);
        let inv = s.apply(&SessionOp::CreateDesk { id, name: "Fresh".into() });
        assert_eq!(s.desks.len(), 1);
        // populate, then delete carries the whole desk in the inverse
        s.apply(&SessionOp::Place { desk: id, id: nid(9), pos: Vec2::default() });
        let full = s.desks[0].clone();
        let before = s.clone();
        let del_inv = s.apply(&SessionOp::DeleteDesk { desk: full });
        assert!(s.desks.is_empty());
        s.apply(&del_inv);
        assert_eq!(s, before, "delete restores desk with cards and position");
        let _ = inv;
    }

    #[test]
    fn reorder_inverts() {
        let mut gen = IdGen::new();
        let mut s = SessionState::default();
        let a = DeskId::generate(&mut gen);
        let b = DeskId::generate(&mut gen);
        let c = DeskId::generate(&mut gen);
        for (id, name) in [(a, "A"), (b, "B"), (c, "C")] {
            s.apply(&SessionOp::CreateDesk { id, name: name.into() });
        }
        let before = s.clone();
        let inv = s.apply(&SessionOp::ReorderDesk { id: c, from_index: 2, to_index: 0 });
        assert_eq!(s.desks[0].id, c);
        s.apply(&inv);
        assert_eq!(s, before);
    }

    #[test]
    fn save_load_round_trips() {
        let t = crate::vault::testutil::TempDir::new();
        let vault = crate::vault::Vault::open(t.path()).unwrap();
        let mut gen = IdGen::new();
        let (mut s, desk) = desk_with(&mut gen, "Reading Notes");
        s.apply(&SessionOp::Place { desk, id: nid(1), pos: Vec2 { x: 100.0, y: 200.5 } });
        s.apply(&SessionOp::Place { desk, id: nid(2), pos: Vec2 { x: -40.0, y: 60.0 } });
        s.desks[0].viewport = Viewport { center: Vec2 { x: 120.5, y: -80.0 }, zoom: 1.25 };
        s.current_surface = Some(SurfaceId::Desk(desk));
        s.open_card = Some(nid(2));
        s.save(&vault).unwrap();
        let loaded = SessionState::load(&vault);
        assert_eq!(loaded, s);
    }

    #[test]
    fn missing_or_corrupt_file_loads_default() {
        let t = crate::vault::testutil::TempDir::new();
        let vault = crate::vault::Vault::open(t.path()).unwrap();
        assert_eq!(SessionState::load(&vault), SessionState::default());
        std::fs::write(t.path().join(".junkdrawer/session/session.jd"), "garbage ]]] \0\n").unwrap();
        assert_eq!(SessionState::load(&vault), SessionState::default());
    }
}
```

**Note:** these tests use `crate::vault::testutil::TempDir` — Task 2 CREATES that module: add to `vault/mod.rs`:

```rust
#[cfg(test)]
pub(crate) mod testutil {
    // the ~25-line TempDir from tests/common/mod.rs, verbatim (unit-test twin;
    // integration tests keep their own copy — Rust can't share across the boundary)
}
```

- [ ] **Step 2: RED** — compile error.
- [ ] **Step 3: Implement** — `geom.rs` verbatim; `session.rs` per the interfaces. `apply` match arms are direct state edits returning the mirrored op (Place↔PutAway, Move swaps from/to, CreateDesk↔DeleteDesk carrying the full desk, RenameDesk swaps, ReorderDesk swaps indices). Parser: split lines; `jd-session 1` header required (else default); `surface =`/`open =` scalars; `[desk <ulid>]` section headers; `name/viewport/card` per section; any malformed line → return default (lenient-all-or-nothing keeps the format honest). Floats via `str::parse::<f32>`. Writer mirrors the format exactly; f32 written via `{}` (Display) — round-trip holds because parse accepts what Display emits.
- [ ] **Step 4: GREEN** (5 tests) — full gate.
- [ ] **Step 5: Commit** — `feat(core): session state with invertible desk ops`

---

### Task 3: `journal.rs` — the app undo stack

**Files:**
- Create: `crates/jd-core/src/journal.rs`
- Modify: `crates/jd-core/src/lib.rs`

**Interfaces (arch §2.11):**

```rust
use crate::command::VaultOp;
use crate::id::NoteId;
use crate::session::{DeskId, SessionOp};

#[derive(Clone, Debug, PartialEq)]
pub enum InverseAction { Vault(VaultOp), Session(SessionOp) }

#[derive(Clone, Copy, PartialEq, Debug, Default)]
pub struct OpContext { pub desk: Option<DeskId>, pub note: Option<NoteId> }

#[derive(Clone, Debug)]
pub struct JournalEntry { pub label: String, pub inverse: InverseAction, pub context: OpContext }

pub const JOURNAL_CAP: usize = 200;

#[derive(Default)]
pub struct Journal { /* undo: Vec<JournalEntry>, redo: Vec<JournalEntry> */ }

impl Journal {
    pub fn new() -> Journal;
    pub fn push(&mut self, e: JournalEntry);              // clears redo; evicts oldest past CAP
    pub fn undo_label(&self) -> Option<&str>;
    pub fn redo_label(&self) -> Option<&str>;
    pub fn pop_undo(&mut self) -> Option<JournalEntry>;   // caller executes inverse, then push_redo
    pub fn push_redo(&mut self, e: JournalEntry);
    pub fn pop_redo(&mut self) -> Option<JournalEntry>;   // caller executes, then push (without clearing? see test)
    pub fn push_undo_from_redo(&mut self, e: JournalEntry); // push to undo WITHOUT clearing redo (redo-chain case)
    pub fn len(&self) -> usize;
    pub fn is_empty(&self) -> bool;
}
```

- [ ] **Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::command::VaultOp;
    use crate::id::NoteId;

    fn entry(label: &str) -> JournalEntry {
        JournalEntry {
            label: label.to_owned(),
            inverse: InverseAction::Vault(VaultOp::Promote { id: NoteId([1; 16]) }),
            context: OpContext::default(),
        }
    }

    #[test]
    fn push_pop_labels() {
        let mut j = Journal::new();
        assert!(j.undo_label().is_none());
        j.push(entry("Toss scrap 'a'"));
        j.push(entry("Promote scrap 'b'"));
        assert_eq!(j.undo_label(), Some("Promote scrap 'b'"));
        let e = j.pop_undo().unwrap();
        assert_eq!(e.label, "Promote scrap 'b'");
        assert_eq!(j.undo_label(), Some("Toss scrap 'a'"));
    }

    #[test]
    fn redo_flow_and_new_push_clears_redo() {
        let mut j = Journal::new();
        j.push(entry("one"));
        j.push(entry("two"));
        let e = j.pop_undo().unwrap();
        j.push_redo(e);
        assert_eq!(j.redo_label(), Some("two"));
        // redo it: pop_redo then push_undo_from_redo keeps deeper redo intact
        let e = j.pop_redo().unwrap();
        j.push_undo_from_redo(e);
        assert_eq!(j.undo_label(), Some("two"));
        assert!(j.redo_label().is_none());
        // a NEW user op clears redo
        let e = j.pop_undo().unwrap();
        j.push_redo(e);
        assert_eq!(j.redo_label(), Some("two"));
        j.push(entry("three"));
        assert!(j.redo_label().is_none(), "new op clears redo");
    }

    #[test]
    fn cap_evicts_oldest() {
        let mut j = Journal::new();
        for i in 0..(JOURNAL_CAP + 10) {
            j.push(entry(&format!("op {i}")));
        }
        assert_eq!(j.len(), JOURNAL_CAP);
        // oldest evicted: popping everything ends at "op 10"
        let mut last = None;
        while let Some(e) = j.pop_undo() {
            last = Some(e.label);
        }
        assert_eq!(last.as_deref(), Some("op 10"));
    }
}
```

- [ ] **Step 2: RED** — compile error.
- [ ] **Step 3: Implement** — two `Vec`s; `push` clears redo and `remove(0)`s past cap (200 entries; O(n) eviction is fine).
- [ ] **Step 4: GREEN** (3 tests) — full gate.
- [ ] **Step 5: Commit** — `feat(core): session-long undo journal`

---

### Task 4: Worker refactor — `Op { op, source }` + Create/SaveBody/lifecycle inverses

**Files:**
- Modify: `crates/jd-core/src/worker.rs`, `crates/jd-core/tests/vault_worker.rs`

**Contract (the tests below + these rules are the spec; arch §2.12):**

- `VaultCommand` becomes: `Op { op: VaultOp, source: OpSource }`, `ReadBody { id }`, `JournalBuffer { id, content }`, `PurgeTrash { older_than_days }`, `RescanAll`, `Shutdown`. Old `Create`/`SaveBody` variants REMOVED.
- `VaultEvent` gains `OpDone { result: OpResult, source: OpSource }` and `OpFailed { label: String, message: String }`; loses `Created`/`Saved` (their information moves into `OpDone` — `result.created` + the index).
- Execution + inverses (this task: the ops that reuse WP1d machinery):
  - `Create` → executes as before → inverse `Delete { id }`, label `op_label("Create", fleeting, display)`, created=[id].
  - `SaveBody` → reads the OLD body first (via `parse_note_file`) → inverse `SaveBody { id, content: old_body }`, label "Edit …". Conflict path unchanged (conflict → `Conflict` event, `OpFailed` NOT emitted — the op "succeeded with divergence"; inverse = old-body SaveBody as usual).
  - `Toss`/`Delete` → `trash_note` → inverse `Restore { id }`; labels "Toss …"/"Delete …". (Same mechanics; the UI difference — confirm dialog — is jd-app's.)
  - `Restore` → `restore` → inverse `Toss { id }`.
  - `Promote` → set_status(Permanent) + move file `inbox/`→`notes/` (`filename_for` re-collides; ledger updated; index re-upserted at the new rel_path) → inverse `Demote { id }`. `Demote` mirrors (notes/→inbox/).
  - `SetKind`/`SetSource`/`SetTags` → frontmatter setter + save → inverse carries the OLD value read before the write.
  - `Batch(ops)` → execute members serially; on first failure: execute the already-computed inverses in reverse (rollback), emit `OpFailed`; on success inverse = `Batch(reversed inverses)`, label = first member's label, created = union.
- Every op result flows through ONE code path that emits `OpDone`/`OpFailed` and wakes.
- WP1d handoff while touching `handle_watch`: **carry `created` forward** — on external `Changed` where the prior index entry exists, preserve `prior.created` in the new meta.

- [ ] **Step 1: Migrate + extend the tests.** Rewrite `tests/vault_worker.rs`: keep every existing scenario (boot scan, create, save-preserves-frontmatter, read-body, echo suppression, external reindex incl. BOTH conflict tests, scan progress, shutdown) migrated to `Op { op: VaultOp::…, source: OpSource::User }` + `OpDone` (helper `send_op(&h, op) -> OpResult` wrapping send + drain). Add these NEW tests:

```rust
#[test]
fn save_body_inverse_restores_old_content() {
    let t = TempDir::new();
    let (h, _) = boot(&t);
    drain_until(&h, |e| matches!(e, VaultEvent::ScanComplete { .. }).then_some(()));
    let created = send_op(&h, VaultOp::Create { seed: scrap("v1\n"), dest: Dest::Inbox });
    let id = created.created[0];
    let saved = send_op(&h, VaultOp::SaveBody { id, content: "v2\n".into() });
    let inverse = saved.inverse.clone().unwrap();
    assert!(matches!(&inverse, VaultOp::SaveBody { content, .. } if content == "v1\n"));
    send_op(&h, inverse);
    h.commands.send(VaultCommand::ReadBody { id }).unwrap();
    let body = drain_until(&h, |e| match e {
        VaultEvent::Body { id: bid, content } if *bid == id => Some(content.clone()),
        _ => None,
    });
    assert_eq!(body, "v1\n");
}

#[test]
fn toss_restore_round_trip_via_inverses() {
    let t = TempDir::new();
    let (h, _) = boot(&t);
    drain_until(&h, |e| matches!(e, VaultEvent::ScanComplete { .. }).then_some(()));
    let created = send_op(&h, VaultOp::Create { seed: scrap("doomed\n"), dest: Dest::Inbox });
    let id = created.created[0];
    let rel = h.index.read().unwrap().get(id).unwrap().rel_path.clone();

    let tossed = send_op(&h, VaultOp::Toss { id });
    assert!(tossed.label.starts_with("Toss scrap"), "{}", tossed.label);
    assert!(h.index.read().unwrap().get(id).is_none(), "tossed note leaves the index");
    assert!(!t.path().join(&rel).exists());

    send_op(&h, tossed.inverse.unwrap()); // Restore
    let meta = h.index.read().unwrap().get(id).cloned().expect("restored to index");
    assert_eq!(meta.rel_path, rel);
    assert!(t.path().join(&rel).exists());
}

#[test]
fn promote_moves_file_and_demote_reverses() {
    let t = TempDir::new();
    let (h, _) = boot(&t);
    drain_until(&h, |e| matches!(e, VaultEvent::ScanComplete { .. }).then_some(()));
    let created = send_op(&h, VaultOp::Create { seed: scrap("# A claim\nbody\n"), dest: Dest::Inbox });
    let id = created.created[0];

    let promoted = send_op(&h, VaultOp::Promote { id });
    let meta = h.index.read().unwrap().get(id).cloned().unwrap();
    assert_eq!(meta.status, Status::Permanent);
    assert!(meta.rel_path.starts_with("notes"), "{:?}", meta.rel_path);
    let on_disk = std::fs::read_to_string(t.path().join(&meta.rel_path)).unwrap();
    assert!(on_disk.contains("status: permanent"));

    send_op(&h, promoted.inverse.unwrap()); // Demote
    let meta = h.index.read().unwrap().get(id).cloned().unwrap();
    assert_eq!(meta.status, Status::Fleeting);
    assert!(meta.rel_path.starts_with("inbox"));
}

#[test]
fn set_ops_carry_old_values_in_inverses() {
    let t = TempDir::new();
    let (h, _) = boot(&t);
    drain_until(&h, |e| matches!(e, VaultEvent::ScanComplete { .. }).then_some(()));
    let id = send_op(&h, VaultOp::Create { seed: scrap("# T\nx\n"), dest: Dest::Notes }).created[0];

    let r1 = send_op(&h, VaultOp::SetSource { id, source: Some("Ahrens (2017)".into()) });
    assert!(matches!(r1.inverse, Some(VaultOp::SetSource { source: None, .. })));
    let r2 = send_op(&h, VaultOp::SetSource { id, source: Some("Luhmann".into()) });
    assert!(matches!(&r2.inverse, Some(VaultOp::SetSource { source: Some(s), .. }) if s == "Ahrens (2017)"));

    let r3 = send_op(&h, VaultOp::SetKind { id, kind: Kind::Literature });
    assert!(matches!(r3.inverse, Some(VaultOp::SetKind { kind: Kind::Note, .. })));
    let meta = h.index.read().unwrap().get(id).cloned().unwrap();
    assert_eq!(meta.kind, Kind::Literature);
    assert_eq!(meta.source.as_deref(), Some("Luhmann"));
}

#[test]
fn batch_rolls_back_on_member_failure() {
    let t = TempDir::new();
    let (h, _) = boot(&t);
    drain_until(&h, |e| matches!(e, VaultEvent::ScanComplete { .. }).then_some(()));
    let id = send_op(&h, VaultOp::Create { seed: scrap("body\n"), dest: Dest::Inbox }).created[0];
    let bogus = NoteId([0xEE; 16]);

    h.commands.send(VaultCommand::Op {
        op: VaultOp::Batch(vec![
            VaultOp::SaveBody { id, content: "changed\n".into() },
            VaultOp::Promote { id: bogus }, // fails: unknown id
        ]),
        source: OpSource::User,
    }).unwrap();
    drain_until(&h, |e| matches!(e, VaultEvent::OpFailed { .. }).then_some(()));

    h.commands.send(VaultCommand::ReadBody { id }).unwrap();
    let body = drain_until(&h, |e| match e {
        VaultEvent::Body { id: bid, content } if *bid == id => Some(content.clone()),
        _ => None,
    });
    assert_eq!(body, "body\n", "failed batch must roll back completed members");
}

#[test]
fn external_edit_preserves_created_timestamp() {
    let t = TempDir::new();
    let (h, _) = boot(&t);
    drain_until(&h, |e| matches!(e, VaultEvent::ScanComplete { .. }).then_some(()));
    let id = send_op(&h, VaultOp::Create { seed: scrap("original\n"), dest: Dest::Notes }).created[0];
    let created_before = h.index.read().unwrap().get(id).unwrap().created;

    std::thread::sleep(Duration::from_millis(1100)); // ensure fs mtime differs at second granularity
    let rel = h.index.read().unwrap().get(id).unwrap().rel_path.clone();
    std::fs::write(t.path().join(&rel), "externally rewritten, no frontmatter\n").unwrap();
    drain_until(&h, |e| {
        matches!(e, VaultEvent::External { changed, .. } if changed.contains(&id)).then_some(())
    });
    let created_after = h.index.read().unwrap().get(id).unwrap().created;
    assert_eq!(created_after, created_before, "WP1d handoff: created carries forward");
}
```

- [ ] **Step 2: RED** — the migrated file fails to compile against the old worker API. Capture it.
- [ ] **Step 3: Implement the refactor** per the contract. Keep `handle_command` as the single dispatch; add `execute_op(&mut Ctx, op: VaultOp) -> Result<OpResult, String>` (recursive for Batch). All prior behavior (ledger, conflict, echo, scan) preserved.
- [ ] **Step 4: GREEN** — full worker suite (old scenarios + 6 new ≈ 16 tests), run TWICE. Full gate.
- [ ] **Step 5: Commit** — `feat(core): vault ops with computed inverses in the worker`

---

### Task 5: Structural single-writer + in-crate trash/recovery tests

**Files:**
- Modify: `crates/jd-core/src/vault/{trash,recovery,scan,mod}.rs`
- Delete: `crates/jd-core/tests/vault_trash.rs`

**Requirements (WP1d handoff, decision #4):**
- Move the 4 tests from `tests/vault_trash.rs` into `#[cfg(test)] mod tests` blocks inside `trash.rs` and `recovery.rs` (same assertions; imports adjusted to `super::*` + `crate::vault::testutil::TempDir` from Task 2).
- Downgrade to `pub(crate)`: `trash_note`, `restore`, `purge_older_than` (trash.rs), `journal_buffer`, `clear_buffer` (recovery.rs), and `parse_note_file` — wait: `parse_note_file` is used by `tests/perf.rs` (integration). It stays `pub` with its single-writer note REMOVED (it's a read-only helper — no mutation; document that instead). `list_trash` and `pending_recoveries` stay `pub` (read-only, jd-app's Trash surface and boot-recovery need them).
- Replace the five "# Single-writer contract" doc blocks' warning text with: "pub(crate): callable only via the vault worker's `VaultOp`s — enforced structurally since WP1e."
- `tests/perf.rs` and `tests/vault_worker.rs` must still compile (they don't use the downgraded functions).

- [ ] **Step 1: RED** — move tests + downgrade visibility; run; fix imports until green (the "test" here is the gate itself: `cargo test --workspace` proves integration tests no longer reach the internals AND the moved tests pass in-crate).
- [ ] **Step 2: GREEN** — full gate.
- [ ] **Step 3: Commit** — `refactor(core): enforce single-writer structurally; trash tests in-crate`

---

### Task 6: `RenameTitle`, `Split`, and `Index::replace_at_path`

**Files:**
- Modify: `crates/jd-core/src/worker.rs`, `crates/jd-core/src/index/mod.rs`, `crates/jd-core/tests/vault_worker.rs`, `crates/jd-core/tests/index_integration.rs`

**Contract:**
- `Index::replace_at_path(&mut self, old_id: NoteId, meta: NoteMeta, body: &str)` — atomic remove+upsert under ONE `&mut self` call (WP1d handoff; the watcher's path-reuse case in `handle_watch` uses it; add an integration test: replace keeps count stable and never leaves a gap — plus migrate the existing remove+upsert call site).
- `RenameTitle { id, new_title }` (decision #3): rewrite the heading line (via `extract_title`'s span), rename the file (`filename_for(new_title, id, same_dir)`, ledger both paths), rewrite every referrer's `[[old]]`→`[[new]]` (match `link.target` case-insensitively == old title; preserve `|display`; bodies via `parse_note_file`, saved atomically, ledger'd, re-upserted), re-upsert self. Inverse: `RenameTitle { id, old_title }`. Untitled note → `Err` → `OpFailed`.
- `Split { id, at_byte }` (spec §5): text after `at_byte` becomes a new note — Permanent-with-title if it starts with `# `, else Fleeting into `inbox/`; the removed text is replaced by `[[New Title]]` (or `[[<first line of new scrap>]]`) + `\n`; both saved. Executes as a `Batch` internally: inverse = `Batch([SaveBody{id, old_body}, Delete{new_id}])`, created = [new_id]. `at_byte` not on a char boundary or out of range → `OpFailed`.

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn rename_title_rewrites_referrers_and_inverts() {
    let t = TempDir::new();
    let (h, _) = boot(&t);
    drain_until(&h, |e| matches!(e, VaultEvent::ScanComplete { .. }).then_some(()));
    let target = send_op(&h, VaultOp::Create { seed: perm("# Old Name\nbody\n"), dest: Dest::Notes }).created[0];
    let referrer = send_op(&h, VaultOp::Create {
        seed: perm("# Referrer\nsee [[Old Name]] and [[old name|shown]]\n"),
        dest: Dest::Notes,
    }).created[0];

    let renamed = send_op(&h, VaultOp::RenameTitle { id: target, new_title: "New Name".into() });
    let tmeta = h.index.read().unwrap().get(target).cloned().unwrap();
    assert_eq!(tmeta.title.as_deref(), Some("New Name"));
    assert!(tmeta.rel_path.to_string_lossy().contains("New Name"));
    let ref_body = std::fs::read_to_string(
        t.path().join(&h.index.read().unwrap().get(referrer).unwrap().rel_path)
    ).unwrap();
    assert!(ref_body.contains("[[New Name]]"), "{ref_body}");
    assert!(ref_body.contains("[[New Name|shown]]"), "display preserved: {ref_body}");
    assert_eq!(h.index.read().unwrap().backlinks(target), vec![referrer], "links stay resolved");

    send_op(&h, renamed.inverse.unwrap());
    let tmeta = h.index.read().unwrap().get(target).cloned().unwrap();
    assert_eq!(tmeta.title.as_deref(), Some("Old Name"));
}

#[test]
fn rename_untitled_fails_cleanly() {
    let t = TempDir::new();
    let (h, _) = boot(&t);
    drain_until(&h, |e| matches!(e, VaultEvent::ScanComplete { .. }).then_some(()));
    let id = send_op(&h, VaultOp::Create { seed: scrap("no heading here\n"), dest: Dest::Inbox }).created[0];
    h.commands.send(VaultCommand::Op { op: VaultOp::RenameTitle { id, new_title: "X".into() }, source: OpSource::User }).unwrap();
    drain_until(&h, |e| matches!(e, VaultEvent::OpFailed { .. }).then_some(()));
}

#[test]
fn split_creates_linked_note_and_inverse_unsplits() {
    let t = TempDir::new();
    let (h, _) = boot(&t);
    drain_until(&h, |e| matches!(e, VaultEvent::ScanComplete { .. }).then_some(()));
    let body = "# Host\nintro text\n# Second Idea\ntail text\n";
    let id = send_op(&h, VaultOp::Create { seed: perm(body), dest: Dest::Notes }).created[0];
    let at = body.find("# Second Idea").unwrap();

    let split = send_op(&h, VaultOp::Split { id, at_byte: at });
    assert_eq!(split.created.len(), 1);
    let new_id = split.created[0];
    let new_meta = h.index.read().unwrap().get(new_id).cloned().unwrap();
    assert_eq!(new_meta.title.as_deref(), Some("Second Idea"));
    assert_eq!(new_meta.status, Status::Permanent);
    let host_body = read_body(&h, id);
    assert!(host_body.contains("[[Second Idea]]"), "{host_body}");
    assert!(!host_body.contains("tail text"));

    send_op(&h, split.inverse.unwrap()); // Batch: restore body + delete new note
    assert_eq!(read_body(&h, id), body);
    assert!(h.index.read().unwrap().get(new_id).is_none());
}

#[test]
fn split_of_untitled_tail_makes_a_scrap() {
    let t = TempDir::new();
    let (h, _) = boot(&t);
    drain_until(&h, |e| matches!(e, VaultEvent::ScanComplete { .. }).then_some(()));
    let body = "# Host\nkeep this\nand split from here onward\n";
    let id = send_op(&h, VaultOp::Create { seed: perm(body), dest: Dest::Notes }).created[0];
    let at = body.find("and split").unwrap();
    let split = send_op(&h, VaultOp::Split { id, at_byte: at });
    let new_meta = h.index.read().unwrap().get(split.created[0]).cloned().unwrap();
    assert_eq!(new_meta.status, Status::Fleeting);
    assert!(new_meta.rel_path.starts_with("inbox"));
    assert!(read_body(&h, id).contains("[[and split from here onward]]"));
}
```

(Plus in `index_integration.rs`: `replace_at_path_is_atomic_and_relinks` — build two notes A→B, `replace_at_path(B, meta_with_new_id_at_same_path, body)`, assert count unchanged, old id gone, new id present, A's link resolution updated. Add `read_body`/`perm` helpers to vault_worker.rs mirroring `scrap`.)

- [ ] **Step 2: RED.** — compile/behavior failures captured.
- [ ] **Step 3: Implement** per contract.
- [ ] **Step 4: GREEN** (worker suite + index suite; worker file run TWICE). Full gate.
- [ ] **Step 5: Commit** — `feat(core): rename with referrer rewrites, split, atomic path replace`

---

### Task 7: The inverse law + setter coverage (M1 gate)

**Files:**
- Create: `crates/jd-core/tests/inverse_law.rs`
- Modify: `crates/jd-core/src/frontmatter.rs` (only if a gap test exposes a bug — otherwise untouched)

**Requirements (spec §13 "Undo" + WP1a setter-coverage handoff):**

- `inverse_law.rs`: for EVERY op variant, execute → capture inverse → execute inverse → `assert_restored(before, after)` where a snapshot is `Vec<(NoteId, rel_path, status, kind, title, tags, source, body)>` sorted by id, plus the sorted on-disk `.md` file list — `modified` excluded (decision #2). Ops covered: Create (inverse Delete empties), SaveBody, RenameTitle, Promote, Demote, SetKind, SetSource, SetTags, Toss, Delete, Restore, Split, and one nontrivial Batch. Structure: one `#[test]` per op, shared `snapshot(&h)` + `assert_restored` helpers, each test builds its own vault (worker boot per test — they're fast).
- Setter coverage through the worker (WP1a gaps): a note whose source contains a double quote round-trips through `SetSource` (`say "hi"` → stored with the documented `'` substitution → inverse restores the SUBSTITUTED value — pin that reality in the assertion with a comment); `SetTags { tags: vec![] }` removes the tags line; a hand-written file with bare-scalar `tags: solo` scans into the index with tag "solo".

- [ ] **Step 1: Write the test file** (RED = helpers/ops referenced before any needed adjustments; most tests may pass immediately against Tasks 4–6 — the deliverable is the LAW enforced in CI, plus any bug it flushes out; fix bugs in the op implementations if a restoration mismatch appears, never relax `assert_restored`).
- [ ] **Step 2: GREEN** — full gate; worker-dependent tests run TWICE.
- [ ] **Step 3: Commit** — `test(core): inverse law for every vault op`

---

## Self-Review Notes

- Arch §2.11/§2.13 coverage: `VaultOp` (all 12 variants + Batch), `OpResult`, `Journal` + `InverseAction`/`OpContext`, `SessionState`/`SessionOp`/`DeskId`/`SurfaceId`, worker `Op{op, source}` refactor. §2.12's `OpDone/OpFailed` events. Spec §9: named labels, ~200 cap, compound (Batch), routing is jd-app's (WP3).
- WP1d handoffs discharged: single-writer structural (T5), Op-shape refactor (T4), created carry-forward (T4), replace_at_path (T6), setter coverage (T7).
- Deliberately NOT here: text undo stacks (jd-app, WP2/3), view-travel on undo (jd-app), guidance strings, demotion UI policy.
- M1 closes when this merges: the entire vault engine tested headless.
