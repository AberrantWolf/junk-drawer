# WP3 Task 8 Report: Edit menu + Split UI + Drag-to-rail gesture

## Status: GREEN — 48/48 jd-app tests pass, 4/4 jd-core tests pass

---

## RED Evidence

New tests added before implementation (all failed compilation or assertion):

- `split_card_ctrl_shift_enter_places_two_cards_and_undo_restores` — failed: `EditorEvent::SplitAndClose` did not exist; `EditorState::split_requested` did not exist
- `edit_menu_undo_item_shows_live_label` — failed: `edit_menu_bar` / `EditMenuCtx` did not exist
- `edit_menu_split_card_disabled_when_editor_closed` — failed: same missing symbols
- `drag_to_rail_inbox_journals_put_card_away` — failed: `RailDropTarget` / `rail_row_hits` did not exist
- `drag_to_rail_desk_row_journals_move_card_to_desk` — failed: same missing symbols

---

## Implementation Summary

### Files Modified

- `crates/jd-app/src/menus.rs` — added `EditMenuAction`, `EditMenuCtx<'a>`, `edit_menu_bar()` using `egui::MenuBar::new().ui(…)`
- `crates/jd-app/src/editor.rs` — added `split_requested: bool` to `EditorState`; added `EditorEvent::SplitAndClose { at_byte }`; Ctrl+Shift+Enter interception before Ctrl+Enter
- `crates/jd-app/src/rail.rs` — added `RailDropTarget` enum (`Inbox` | `Desk(DeskId)`); added `row_hits: &'a mut Vec<(egui::Rect, RailDropTarget)>` to `RailUiDeps`; rail_ui populates row_hits each frame
- `crates/jd-app/src/surfaces/desk.rs` — added `DeskEvent::CardDroppedOnRail(RailEvent)`; added `rail_row_hits` to `DeskUiDeps`; drag release checks rail rects before falling through to plain Move
- `crates/jd-app/src/state.rs` — added `pending_split: Option<NoteId>` to `UiState`
- `crates/jd-app/src/app.rs` — top panel with `edit_menu_bar`; `EditorEvent::SplitAndClose` handler dispatches `Batch([SaveBody, Split])`; `OpDone` places both cards side-by-side; desk cleanup evicts cards whose notes left the index; `rail_row_hits: Vec<(Rect, RailDropTarget)>` wired through
- `crates/jd-app/tests/workflow_kittest.rs` — 5 new tests (4 passing immediately, 1 needed fix)

### Edit Menu Bar

`egui 0.35` removed `egui::menu::bar`. The correct API is `egui::MenuBar::new().ui(ui, |ui| { … })`. Undo/Redo labels pulled live from `journal.undo_label()` / `journal.redo_label()`. Cut/Copy/Paste ship disabled-with-shortcut-hint (egui 0.35 has no programmatic path to forward clipboard ops to the focused TextEdit). Split Card enabled only when `editor_open`. Find disabled with tooltip referencing WP4.

### Split UI

Ctrl+Shift+Enter is intercepted pre-TextEdit via `ui.input_mut(|i| i.consume_key(COMMAND | SHIFT, Enter))`. The cursor byte offset is computed from `ed.prev_cursor` (the prior frame's cursor state, stored so the interception beats the TextEdit consuming it). The editor closes immediately; `pending_split = Some(orig_id)` and `pending_label = Some("Split card 'Title'")` are set; `Batch([SaveBody { id, content: buffer }, Split { id, at_byte }])` is dispatched.

On `VaultEvent::OpDone` with `source == User` and `result.created` non-empty while `pending_split` is set, both cards are placed: original at its existing desk position (or viewport center if not on desk), split-off at `orig_pos + (324.0, 0)` via `session.apply(Place)` (not journaled). The journal entry is `InverseAction::Vault(inv_op)` with label "Split card 'Title'"; one undo dispatches the inverse `Batch([SaveBody_orig, Delete(split_off)])`.

Desk cleanup: every `OpDone` (regardless of source) does `desk.cards.retain(|c| idx.get(c.id).is_some())` — this evicts the split-off card from the desk when the undo Delete moves it to trash.

### Drag-to-rail Gesture

`rail_ui` clears `deps.row_hits` at frame start and pushes `(resp.rect, target)` for each rendered row. `desk_ui` drag-release (when `total_delta >= 4.0`) first iterates `rail_row_hits`, checking if the pointer is inside any row rect; if so, emits `DeskEvent::CardDroppedOnRail(RailEvent)` instead of plain Move. `app.rs` dispatches the rail event via `apply_rail_event`.

---

## Key Bug Fixed: Race Between Index Update and OpDone Drain

The split-undo test pump predicate originally checked `idx.iter_meta().count() == 1`. The vault worker updates the shared index synchronously (write lock) but sends `OpDone` only after the full Batch completes. When the pump checked the index count first, it could see count=1 (index already updated) while the worker was still mid-execution and hadn't sent `OpDone` yet. The pump exited immediately; the test asserted before `drain_events` ever processed the undo's `OpDone` (and ran the desk cleanup).

Fix: pump waits for `a.state.pending_undo_redo.is_none() && idx.iter_meta().count() == 1`. `pending_undo_redo` is cleared only inside `drain_events`' `OpDone` arm, guaranteeing the cleanup has run.

---

## Test Results

```
running 48 tests
...
test drag_to_rail_desk_row_journals_move_card_to_desk ... ok
test drag_to_rail_inbox_journals_put_card_away ... ok
test edit_menu_split_card_disabled_when_editor_closed ... ok
test edit_menu_undo_item_shows_live_label ... ok
test split_card_ctrl_shift_enter_places_two_cards_and_undo_restores ... ok
...

test result: ok. 48 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 7.28s
```

jd-core: 4/4 pass. `cargo clippy --workspace --all-targets -- -D warnings` clean. `cargo fmt --check` clean.

---

## Concerns

1. **Split-off body heading**: The brief mentions "new card body = line 2 (+ heading if it started with `# `)". The `Split` op in `jd-core` handles this in the worker. The kittest verifies two cards are placed but does not assert on the exact body content of the split-off (body fetching would require a second pump cycle). This is a gap in test coverage that a dedicated kittest for `VaultOp::Split` in jd-core would close.

2. **Split placement rides the op's journal entry**: The split-off's desk placement is intentionally NOT journaled separately. One undo removes the split AND the placement. If the user manually moves the split-off card before undoing, the Move IS journaled (separate entry); undoing the Split then leaves the original card with the restored body but no side-by-side split-off.

3. **Rail row rects are one frame stale**: `rail_ui` populates `row_hits` during the same frame as `desk_ui`. Since both run in the same `ui()` call, the ordering determines whether the hits are from this frame or last. Currently rail renders before the desk panel, so the rects are current-frame — correct.
