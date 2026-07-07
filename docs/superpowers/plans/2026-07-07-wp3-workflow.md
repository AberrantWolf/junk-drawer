# WP3 — The Workflow: Inbox, Promotion, Undo Wiring — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** M3 — the complete capture→promote→link loop: inbox pile, Enter-promotes, split, toss/trash, the left rail, and both undo stacks wired to keys. After this, the app is dogfoodable.

**Architecture:** Builds directly on WP2's `JdUi` (frame loop, `apply_session`, `DeskEvent`/`EditorEvent` patterns, kittest infrastructure). New surfaces (`inbox.rs`, `trash.rs`) follow `desk.rs`'s shape: render fn takes deps, returns events; app.rs is the single mutation site. WP3 is ALSO allowed to modify `jd-core` for the pinned hardening list (WP1e handoffs) — the single-writer and round-trip laws still bind absolutely.

**Tech Stack:** unchanged — eframe/egui 0.35, egui_kittest 0.35 (dev), jd-core.

## Global Constraints

- **Dependency policy:** NO new dependencies, runtime or dev. `jd-core` = notify only; `jd-app` = eframe + jd-core (+ egui_kittest dev). "Reveal in File Manager" uses `std::process::Command` with `open`/`explorer`/`xdg-open` — no opener crate.
- **jd-core changes are allowed ONLY for the hardening list in Task 1** — everything else is jd-app. Any jd-core change keeps the inverse law green (`cargo test -p jd-core`) and the single-writer contract.
- **Round-trip law** unchanged; the one sanctioned frontmatter mutation remains `set_modified` on save. Checkbox toggling on faces (Task 9) writes through `VaultOp::SaveBody` with the toggled byte — a real user edit, not a rewrite.
- **Journal discipline (established WP2):** user acts journal with human labels; system acts (bootstrap, external events, undo/redo replays) do NOT. All session mutations via `apply_session`; all vault mutations via `VaultCommand::Op`. `OpSource::UndoRedo` for inverse replays (drain_events already skips journaling those).
- **CI green** on all suites; `cargo fmt` last before every commit.
- Commit trailer: `Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>`
- **OVERNIGHT SIGNING POLICY (2026-07-07, Scott asleep):** 1Password is locked and signing HANGS. Commit with `--no-gpg-sign` directly — do NOT attempt `-S` first. All WP3 commits get batch re-signed in the morning. Do NOT push tonight (SSH also needs the 1P agent).
- **BRANCH GUARD:** work on `feat/workflow`. `git status -sb` before starting AND before each commit; never checkout/switch; wrong branch → STOP and report BLOCKED.

## Scope boundaries (WP3 does NOT include)

- Palette (`Ctrl+K`), Drawer surface, ghost fan, edges-on-select — WP4. The Edit menu's "Find" item renders disabled with tooltip "Ctrl+K — arrives with the Drawer".
- Map — WP5. Rail's Drawer/Map rows navigate to a placeholder surface ("Coming in a later milestone" centered label).
- Guidance banner, settings UI/dialog, full File/View/Help menus, muda/macOS native bar, shortcut overlay (`Ctrl+/`) — WP6. WP3 ships ONLY the Edit menu + Card context menu (egui-drawn).
- Global capture hotkey/tray/single-instance — WP7.
- Export Desk as Outline — WP6 (File menu).

## Interfaces consumed (verified in-tree, post-WP2)

- `JdUi { state: UiState, vault: VaultHandle, vault_ref: Vault, theme, id_gen, last_panel_rect, line_cache, ... }` — read `crates/jd-app/src/app.rs` first in every task.
- `UiState { session, session_dirty_at, focus, editor, bodies, journal, scan_done, last_error, pending_create, text_undo }`; `apply_session(&mut self, op, journal: Option<&'static str>)`; `place_card(desk, id, pos)`; `op_subject_ids`.
- `SessionState/SessionOp/SurfaceId` (jd-core session.rs): `current_surface: SurfaceId::{Desk(DeskId), Inbox, Drawer, Map, Trash}`; SessionOps incl. `CreateDesk/RenameDesk/ReorderDesk/DeleteDesk`.
- `VaultOp::{Create, SaveBody, RenameTitle, Promote, Demote, SetKind, SetSource, SetTags, Toss, Delete, Restore, Split, Batch}` + `OpResult { inverse, label, created }` + `Journal`/`JournalEntry { label, inverse: InverseAction::{Vault,Session}, context: OpContext }` — read command.rs/journal.rs/worker.rs for exact shapes; check what `Promote` does on disk (status + inbox/→notes/ move) in worker.rs before building promotion.
- `desk_ui`/`DeskEvent`, `editor_ui`/`EditorEvent::{KeepOpen, CloseAndSave}`/`EditorState`, `card_face`/`CardFace`, `reading_order`/`next_focus`, `Index` via `SharedIndex` (`meta(id)`, iteration — check exact API).
- kittest helpers: `tests/common/mod.rs` (TempDir, pump), `app_with_cards`-style helpers in desk_kittest.rs, the proven typing sequence in editor_kittest.rs.

## File Structure

```
crates/jd-app/src/
├── app.rs           # MODIFY: surface routing (render current_surface), undo/redo dispatch, view-travel, status echo
├── state.rs         # MODIFY: UiState gains pending_confirm (delete dialog), status_echo: Option<(String, Instant)>, nav history hooks NOT yet (WP6 Back/Forward)
├── editor.rs        # MODIFY: promotion detection (PendingPromotion), Ctrl+Enter promote path, Split hook (Ctrl+Shift+Enter)
├── rail.rs          # CREATE: left rail
├── menus.rs         # CREATE: egui menu bar (Edit only) + card context menu (shared item list)
├── surfaces/
│   ├── mod.rs       # MODIFY: pub mod inbox; pub mod trash; pub mod placeholder;
│   ├── inbox.rs     # CREATE
│   ├── trash.rs     # CREATE
│   └── placeholder.rs # CREATE: ~15 lines, centered label for Drawer/Map
crates/jd-core/src/
├── worker.rs        # MODIFY (Task 1 ONLY): Batch rollback error surfacing; RenameTitle rollback discipline
└── (docs in command.rs re path-stability decision)
crates/jd-app/tests/
├── workflow_kittest.rs  # CREATE: inbox/promotion/toss/undo scenarios
└── (desk_kittest.rs, editor_kittest.rs extended where noted)
```

Every task: read the files you modify FIRST; follow their established comment/idiom density; expand the plan's test sketches into real code — assertions have NO latitude.

---

### Task 1: jd-core hardening (the WP1e handoff list)

**Files:** `crates/jd-core/src/worker.rs`, `crates/jd-core/src/command.rs` (doc comments), `crates/jd-core/tests/vault_worker.rs` or `commands` tests (read what exists; add there).

Four pinned items — the ONLY jd-core changes in WP3:

1. **Batch rollback error surfacing.** In the worker's Batch execution, mid-batch failure currently rolls back already-executed members with `let _ =` on the rollback results. Change: collect rollback failures; if any, emit `VaultEvent::Error { context: "batch rollback", message }` naming the op labels that failed to roll back (the vault may be mixed-state — the user must be able to see that). Test: a Batch of [SaveBody(ok), RenameTitle(forced-fail via a collision you construct)] where the SaveBody rollback is ALSO forced to fail (e.g. delete the file between — use the existing failpoint/testutil patterns from WP1d if present, else construct via permissions or a nonexistent id in the rollback path) → assert an Error event mentioning rollback. If constructing a double-failure is impractical with existing seams, add the narrowest possible test seam consistent with WP1d's failpoint style and note it.
2. **RenameTitle rollback discipline.** RenameTitle rewrites `[[links]]` in N referrer files after renaming self; a mid-loop failure leaves partial state with no rollback. Wrap: on referrer-rewrite failure, roll back already-rewritten referrers and the self-rename (Batch-style), surface rollback failures per item 1. The inverse law tests must stay green. Test: force a referrer write failure mid-loop (failpoint seam again) → assert vault restored (self name back, referrers back) or Error surfaced when rollback itself fails.
3. **Path-stability decision — accept-and-document.** Body-derived filenames make some undo paths rel_path-unstable (untitled-note Batch case; RenameTitle-undo when the old name was re-claimed). DECISION (controller, 2026-07-07): accept and document — collision suffixing already guarantees no clobber; a path-drifted undo is still a correct content restore. Add a doc-comment block on `VaultOp` in command.rs stating this + a pinned test demonstrating the drifted-but-correct behavior (undo RenameTitle after re-claiming the old title → content restored under a suffixed name, index consistent).
4. **Split-undo-leaves-trash documented.** Doc-comment on `VaultOp::Split` + ensure the op label used for Split undo journaling reads naturally (the WP3 status echo will show it; e.g. label "Split card" and undo echo appends "(split-off card moved to trash)" — the echo suffix lives in jd-app Task 6, but pin the label string here).

**Verify:** `cargo test -p jd-core` fully green (inverse law suite especially); fmt/clippy clean.
**Commit:** `fix(core): surface batch/rename rollback failures; document path-stability` (with `--no-gpg-sign` per overnight policy).

---

### Task 2: Surface routing + rail

**Files:** create `rail.rs`, `surfaces/placeholder.rs`; modify `app.rs` (render current_surface; rail always visible on the left), `surfaces/mod.rs`, `state.rs` if needed.

- `rail_ui(ui, deps) -> Vec<RailEvent>`; `RailEvent::{Switch(SurfaceId), CreateDesk, RenameDesk{id,name}, ReorderDesk{id,to}, CardDropped{target: SurfaceId | DeskId, id: NoteId}}` — same events-out pattern as desk_ui. app.rs applies: Switch sets `session.current_surface` via `apply_session`? — NO: current_surface is a field, not a SessionOp; set it directly + mark `session_dirty_at` (navigation is not undoable, like viewport). CreateDesk/RenameDesk/ReorderDesk ARE journaled SessionOps (labels "Create desk"/"Rename desk"/"Reorder desk") via `apply_session`.
- Rail contents top-to-bottom: desk list (rail order = `session.desks` order), separator, Inbox (with quiet count of Fleeting notes from the index — computed once per frame under the existing FaceMeta lock), Drawer, Map, Trash. Current surface highlighted. Every row is a labeled AccessKit widget (`"Desk: Reading"`, `"Inbox, 3 scraps"` — count in the label, singular/plural correct, zero → just "Inbox").
- Desk create: a small "+" button → creates "Desk N" (rename via double-click → inline TextEdit, or context menu Rename). Reorder: drag rows (egui drag on the row; simplest correct approach — up/down context-menu items are an acceptable keyboard-accessible fallback and REQUIRED regardless, since drag-only violates the no-spatial-only law).
- **Drag a card to the rail** = PutAway; onto a desk row = move to that desk (`SessionOp::PutAway` from source + `Place` on target desk at that desk's viewport center, one journaled compound — check whether Journal supports one entry with multiple session inverses; if not, journal as two entries is WRONG (two Ctrl+Z for one act) — instead add the composite as a single JournalEntry whose inverse is... read journal.rs; if InverseAction can't compose, dispatch both ops but journal only a synthetic entry that the WP3 undo executor (Task 6) understands: simplest robust choice — extend NOTHING in jd-core; use `InverseAction::Session` twice? DECISION: implement rail-drop-to-desk as Place-on-target THEN PutAway-from-source and journal ONLY the composite label "Move card to desk '<name>'" with inverse = Session(Place back on source at old pos); the target-place's inverse (PutAway from target) composes as the redo — work through this carefully in the task and document what you shipped; the kittest asserts ONE undo restores the card to the source desk position).
- Switching surfaces renders: Desk → existing desk_ui; Inbox/Trash → Tasks 3/5 (until then placeholder); Drawer/Map → placeholder.
- Kittest (new `workflow_kittest.rs`): rail rows exist with a11y labels incl. inbox count; clicking Inbox switches surface; create desk adds a row + journals; drag-card-to-rail puts away (reuse desk drag helpers).

---

### Task 3: Inbox surface

**Files:** create `surfaces/inbox.rs`; modify `app.rs` (route), `workflow_kittest.rs`.

- Every `Status::Fleeting` note, **oldest-first** by `created`. Scattered pile under Paper (deterministic per-id jitter ±12px around a flowing grid — same seeded-xorshift idiom as shape.rs tears), tidy column under Plain. Uses `card_face` (scrap faces) with the same BodyCache blank-face discipline.
- Focus/keyboard: reading order = list order (it's a pile/column, not a spatial canvas — `reading_order` unnecessary; Up/Down or Left/Right move focus linearly), Enter opens the editor (same OpenCard path).
- The three acts: **Ctrl+Enter** on focused scrap = promote-without-typing → opens the editor in promotion mode (Task 4's path — until Task 4 lands, wire the event and leave a `// wired in promotion task` no-op); **Del** = Toss, no confirm (journaled via OpDone as usual, label from jd-core); **Ctrl+D** = desk picker (small egui popup listing desks; Enter/click → Place on that desk at viewport center + card STAYS fleeting/inboxed — placement only).
- Quiet count already in the rail (Task 2).
- Kittest: create 3 scraps with staggered `created` (drive the worker; oldest-first assert by a11y order or positions); Del tosses (index shows gone → present in trash listing via whatever trash query API exists); Ctrl+D places on desk while staying in inbox list.

---

### Task 4: Promotion — the milestone centerpiece

**Files:** modify `editor.rs`, `app.rs`, `card/mod.rs` if face restyle needs a hint; extend `editor_kittest.rs` + `workflow_kittest.rs`.

Spec §6 + architecture §3 promotion-detection, exactly:

- `EditorState` gains `pending_promotion: bool` and knows the note's status (thread `is_fleeting` in when opening — from FaceMeta/index).
- **Trigger:** in a FLEETING card's editor, pressing Enter when (a) the buffer currently has exactly one line, and (b) the cursor is at end-of-first-line → intercept (before TextEdit, the established pattern): allow the newline insertion, set `pending_promotion = true`, and restyle immediately — the first line renders as title (the layouter already styles a leading `# `? NO — the scrap's first line has no `#`; promotion PREPENDS nothing visually... DESIGN, pinned: promotion sets pending; the VISUAL restyle = layout the first line with the Heading(1) format even without markers while pending (a `promote_restyle: bool` param to `layout_body` — a face-side/editor-side presentation transform like the divider strip; the BUFFER stays raw). On close, the compound op writes the title heading: final body = `# <first line>\n<rest>` — VERIFY against jd-core's Promote op: read worker.rs Promote execution — does it derive/require the `# ` heading? WP1e's Promote does status+move; the title comes from the first `# ` heading via extract_title. So the editor's close-time SaveBody must prepend `# ` to line 1 (that's the text transformation the user SAW as the restyle). Ctrl+Z while pending: unset pending, remove the added newline (text undo already holds it) — in-editor only, no vault op.
- **Ctrl+Enter in a fleeting editor** = same code path: seed = set pending, ensure two lines (append `\n` if single-line), cursor at end of line 1... spec: "promote-without-typing, seeding the first line as title with the cursor on it". Implement: pending=true, cursor stays at end of first line, no newline needed until close. Then falls through to close (Ctrl+Enter also closes) → compound commit.
- **Commit on editor close** (Esc or Ctrl+Enter) when pending: dispatch ONE compound `VaultOp::Batch([SaveBody { id, content: "# line1\nrest" }, Promote { id }])` — verify Batch returns a single OpResult with a composed inverse (it does; inverse law covers it) → ONE journal entry, label "Promote scrap '<first line>'" (override the worker label? OpResult carries label from jd-core — if Batch's label is generic, override at journal-push time in drain_events? drain_events uses result.label; add a WP3 mechanism: `pending_label: Option<String>` on UiState set when dispatching a compound whose label should be human (consumed by the next matching OpDone) — document it). Editor closes; the card on the desk now renders as an index card (shape follows status via FaceMeta refresh).
- **Reduced motion**: the reshape animation is out of scope until WP6 theming of motion; the restyle is instant in WP3 (note it).
- Kittest (the spec's pedagogy, pinned): create scrap "egui layouter idea" via Ctrl+N path; type Enter at end of first line → assert editor still open + pending (galley first-row taller — restyle visible); type "body words"; Esc → pump → assert: file now in `notes/`, body starts `# egui layouter idea`, status Permanent, desk face is IndexCard, journal has ONE entry labeled with the scrap's first line; **Ctrl+Z (editor closed → app stack, Task 6) reverses the WHOLE thing** — file back in inbox/, fleeting, body without `# ` (this last assert lands in Task 6's test if undo isn't wired yet — split the scenario across the two tasks explicitly, don't skip it).
- Multi-line captures stay fleeting until worked on (test: 2-line scrap, opening + typing at end does NOT trigger; only the Enter-at-end-of-single-line edit action does).

---

### Task 5: Toss / Delete / Trash surface

**Files:** create `surfaces/trash.rs`; modify `app.rs` (Del key routing by focused card status + confirm dialog), `state.rs` (`pending_confirm: Option<NoteId>`), `workflow_kittest.rs`.

- **Del** on focused card (desk or inbox): Fleeting → `VaultOp::Toss` immediately (no confirm); Permanent → confirm modal ("Delete '<title>'? It moves to Trash." / Delete / Cancel; Enter=Delete Esc=Cancel) → `VaultOp::Delete`. Both journaled via OpDone (labels from jd-core).
- **Trash surface:** list trashed notes (read the trash listing API in vault/trash.rs — worker command or direct read? trash listing is a READ of .junkdrawer/trash; if no worker query exists, add NOTHING to jd-core — read the trash dir via `vault_ref` on the UI thread? NO — FS reads for content are worker-only by law... but trash listing is directory metadata, and session.jd reads are already sanctioned. DECISION: metadata-only listing (filenames + mtimes) via vault_ref is sanctioned (same class as session load); body previews come via the normal ReadBody? trashed notes aren't in the index... Keep faces title-only from the trash filename. Document the choice.) Rows: title, trashed-when, Restore button (`VaultOp::Restore`), retention notice line at top: "Items in Trash are kept 30 days" (constant; settings arrive WP6).
- **Demote to Scrap** exists ONLY in the card context menu (Task 7) — no shortcut. (Wire `VaultOp::Demote` there.)
- Kittest: toss scrap → gone from inbox, appears in trash list; restore → back (status preserved); permanent delete requires the confirm (Del then Esc = still present; Del then Enter = trashed).

---

### Task 6: App-stack undo/redo — routing, named entries, echo, view-travel

**Files:** modify `app.rs`, `state.rs`; extend `workflow_kittest.rs`.

- **Routing rule (spec §9):** editor open → Ctrl+Z/Ctrl+Shift+Z/Ctrl+Y go to text undo (already true); editor closed → app stack. Wire the app side in the frame loop's shortcut step.
- **Undo executes the inverse:** pop `JournalEntry`; `InverseAction::Vault(op)` → send `VaultCommand::Op { op, source: OpSource::UndoRedo }`; `InverseAction::Session(op)` → `apply_session(op, None)` (not re-journaled). Push the popped entry to the redo stack (Journal has redo support — pop_redo/push_redo/push_undo_from_redo per WP1e; read journal.rs and use its intended flow). Redo mirrors. **The redo inverse for vault ops arrives asynchronously** (the UndoRedo OpDone carries the new inverse) — journal.rs's flow was designed for this in WP1e; follow it and test it (undo then redo then undo again = original state).
- **Named + echoed:** status line shows "Undid: <label>" / "Redid: <label>" for ~4s (`status_echo: Option<(String, Instant)>`); the Edit menu (Task 8) shows "Undo <label>" enabled-ness from `journal.undo_label()`.
- **View-travel:** `OpContext` (journal.rs) carries desk/note — POPULATE IT at dispatch time now (WP2 left it default): when journaling OpDone entries, attach `OpContext { desk: current desk if Desk surface, note: subject id }` (extend the drain_events journal push using op_subject_ids' first id; for session ops, the desk in the op). On undo/redo: if the entry's context names a desk ≠ current surface, switch to it; if it names a note on a desk, `reveal` it (focus + center). Spec: "if the change happened elsewhere, the view travels there so you see it."
- Split-undo echo suffix: when the undone label is the Split label (Task 1 pinned string), echo appends " (split-off card moved to trash)".
- Kittest: move a card, Ctrl+Z (editor closed) → position restored + echo label present (query the status-line a11y text); redo → re-moved; toss + undo → note restored from trash; the Task 4 promotion single-Ctrl+Z scenario completes here; undo from a DIFFERENT surface travels back to the desk (switch to Inbox, Ctrl+Z a desk move → surface switches to that desk).

---

### Task 7: Card context menu

**Files:** create menu items in `menus.rs` (shared fn used by both context menu and Task 8's Card menu later — WP3 ships context-menu only); modify `card/mod.rs` (right-click + Shift+F10 open), `app.rs` (apply `MenuEvent`s), `workflow_kittest.rs`.

Items (spec §10 Card menu, exact order): Promote · Toss · Take to Desk ▸ (desk submenu) · Put Away · Set Source… (small text-input modal → `VaultOp::SetSource`) · Make Divider (`VaultOp::SetKind(Structure)`) · Demote to Scrap (`VaultOp::Demote`) · Copy Link (`[[<title>]]` → `ui.ctx().copy_text`) · Reveal in File Manager (`open -R`/`explorer /select,`/`xdg-open parent` via std::process::Command, spawn, ignore result with a status echo on error). Items disable sensibly (Promote only on fleeting; Demote only on permanent; Make Divider only on non-divider). `Response::context_menu` + Shift+F10 opens at the focused card. All items are AccessKit-visible (egui menu items are buttons — verify labels queryable in kittest).
Kittest: Shift+F10 opens menu on focused card (query an item by label); Copy Link puts `[[Title]]` in the clipboard (`harness` clipboard access — check egui_kittest for output events / `ctx.output` inspection); Make Divider changes the face shape; Demote returns a card to the inbox.

---

### Task 8: Edit menu + Split UI

**Files:** modify `menus.rs` (egui `MenuBar` with Edit only — File/View/Help are WP6), `app.rs` (top panel), `editor.rs` (Ctrl+Shift+Enter → Split), tests.

- **Edit menu:** Undo <label> / Redo <label> (disabled when empty; live labels), separator, Cut/Copy/Paste (forward to the focused TextEdit via egui commands — if egui 0.35 lacks a clean programmatic path, ship them disabled-with-shortcut-hint and note it: the shortcuts themselves already work natively in TextEdit), separator, Split Card (enabled only when editor open; dispatches the same path as Ctrl+Shift+Enter), Find (disabled, tooltip "Ctrl+K — arrives with the Drawer").
- **Split (`Ctrl+Shift+Enter` in the editor):** intercept pre-TextEdit; compute the cursor BYTE offset; close-and-save first? Spec: split the OPEN card at the cursor: dispatch `VaultOp::Split { id, at_byte }` — but the buffer may be dirty: send `Batch([SaveBody{current buffer}, ...])`? Split operates on the SAVED body — DECISION (pinned): dispatch `Batch([SaveBody { id, content: buffer }, Split { id, at_byte }])` so the split sees exactly what the user sees; single journal entry (label "Split card"); editor closes; on the OpDone, place BOTH cards side by side on the current desk (original stays at its position; the split-off (result.created) places at original + (card_width + 24, 0)) via `place_card`… which journals separately — NO: the placement of the split-off is part of the same user act. Use the Task 4 `pending_label`-style compound handling: suppress the Place journal for this one (apply_session(op, None)) and rely on the Split undo (which trashes the split-off) — document this in code.
- Kittest: open a 2-line permanent card, cursor at start of line 2, Ctrl+Shift+Enter → two cards on desk side by side; new card body = line 2 (+ heading if it started with `# `); original body ends with a `[[link]]` to the new card; ONE undo removes the split (original restored, split-off in trash).

---

### Task 9: WP2 handoff cleanups

**Files:** `card/mod.rs` + `theme.rs` (checkbox), `app.rs` (pending_create sweep), `editor.rs` (ac_dismissed), tests where touched.

1. **Face checkbox ☐/☑ + click-to-toggle:** face-side text transform (like the divider heading strip): when building the FACE body for layout, replace `- [ ]` span text with `☐` and `- [x]` with `☑` (the transform is presentation-only; buffer/save path untouched — the editor still shows raw). Click hit-test on the checkbox span's rect → `VaultOp::SaveBody` with the byte toggled (`[ ]`↔`[x]`) — a real journaled edit. Test: face shows the glyphs (snapshot the task-list card face — ONE new golden) and clicking toggles the file on disk.
2. **pending_create sweep:** on `ScanComplete`, if `pending_create` is Some and a desk now exists, keep it armed (the OpDone may still come); if `pending_create` is Some and older than ~5s with no desk… simplest correct sweep per the comment at the consumption site: when ScanComplete fires and pending_create is set, re-check on subsequent OpDones as now, but ALSO handle the reversed order: buffer the earliest orphaned Create OpDone (its created id) while no desk exists and consume it right after the bootstrap desk lands. Implement whichever is cleanest against the actual event order; test: Ctrl+N pressed before ScanComplete (drive events manually into drain via the worker on a slow-scan fixture, or simulate by reordering — a unit-style test on drain_events with hand-built events is acceptable and cheaper).
3. **ac_dismissed re-shows on query change:** store the dismissed query string; popup re-appears when the query differs. Test row in the existing autocomplete kittest.

---

### Task 10: M3 integration scenarios + docs + full check

**Files:** `workflow_kittest.rs`, architecture doc, ledger.

- **The M3 story test:** capture 2 scraps → both in inbox oldest-first → promote one via Enter-typing (full pedagogy path) → link it to the other via `[[` → toss the second scrap → undo the toss → take the restored scrap to a desk via Ctrl+D → put it away via Backspace → verify trash empty, inbox 1, desk state, all journal labels named. Restart → everything persists.
- Architecture doc: WP3→WP4 handoffs block (anything deferred/discovered); update the WP2→WP3 handoff items as resolved (checkbox ✓, pending_create ✓, ac_dismissed ✓); note the promotion `pending_label` mechanism and the split placement journaling decision under a new decision §6.15 if the controller hasn't already.
- Full check: `cargo test --workspace`, clippy `-D warnings`, `fmt --check`. jd-core diff vs main allowed ONLY in worker.rs/command.rs/tests (Task 1) — print `git diff --stat main -- crates/jd-core` in the report.

---

## Verification (WP definition of done)

- All 10 tasks green locally (CI validation happens after the morning re-sign + push).
- M3 bar (spec §14): "capture, inbox pile, Enter-promotes, split, toss/trash, both undo stacks."
- Inverse-law suite still green with the Task 1 hardening.
- The M3 story test passes; every journal entry a user can create has a human label; undo is never silent (echo + view-travel).
