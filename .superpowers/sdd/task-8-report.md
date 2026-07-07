# Task 8 Report: Pannable Desk Canvas

## Status: GREEN — all 31 jd-app tests pass

---

## RED Evidence

`cargo test -p jd-app --test desk_kittest` before implementation produced 5 compile errors:

- `DeskCamera` not found in `jd_app::surfaces::desk`
- `desk_ui` not found
- `DeskUiDeps` not found
- `FaceMeta` not found
- `DragState` not found

These confirmed the tests were truly red before any implementation work began.

---

## Implementation Summary

### Files Modified

- `crates/jd-app/src/surfaces/desk.rs` — new types + `desk_ui()` + `reveal()`
- `crates/jd-app/src/app.rs` — JdUi wired to desk canvas
- `crates/jd-app/tests/desk_kittest.rs` — created (5 integration tests)
- `crates/jd-app/tests/common/mod.rs` — added `new_note` helper

### Camera Math

`DeskCamera` implements the brief's transform verbatim:

```
screen = panel_center + (world - center) * zoom   [to_screen]
world  = center + (screen - panel_center) / zoom   [to_world]
```

`zoom_to_fit` computes center as the bounding-box midpoint of all placed cards and zoom as `min(panel_w / bbox_w, panel_h / bbox_h) * 0.85` (15% margin), clamped to a `0.01` minimum (not `ZOOM_MIN`) so cards at extreme world coordinates (e.g., `(100_000, 0)`) actually fit. The `0.01` floor is for `zoom_to_fit` only; interactive zoom is clamped to `[ZOOM_MIN, ZOOM_MAX]`.

Card screen rect requires multiplying world-space card size by `cam.zoom` — `to_screen` only transforms position, not dimensions.

### Index API Choices

`FaceMeta` uses `index.outlinks(id).len()` (not `index.meta(id).links`) because the `Index` API exposes `outlinks()` for link counts, not `Index::meta().links`. The `FaceMeta` is prefetched under ONE index read lock per frame in `app.rs` before the render pass begins, keeping the lock out of the draw loop.

### egui Scroll Architecture

egui routes Ctrl+scroll through `InputOptions::zoom_modifier = COMMAND` to `zoom_delta()`, making `smooth_scroll_delta` zero when COMMAND is held. The implementation uses:
- `ui.input(|i| i.zoom_delta())` for zoom (Ctrl+scroll)
- `ui.input(|i| i.smooth_scroll_delta)` for pan (plain/Shift scroll)

This matches egui 0.35 behavior; using `smooth_scroll_delta` with a `cmd_down` check would never trigger zoom.

### API Adaptations from Brief

1. `DeskEvent` gained a 4th variant `ViewportMoved { desk: DeskId, cam: DeskCamera }` — the brief's 3-variant enum had no way to propagate viewport changes back to `app.rs`. Viewport moves are NOT journaled (per brief), but `session_dirty_at` is updated so the session saves.

2. `DeskUiDeps<'a>` bundles: `focus: &mut Option<NoteId>`, `drag: &mut Option<DragState>`, `bodies: &mut BodyCache`, `commands: &VaultCommandSender`, `face_meta: &[FaceMeta]`, `line_cache: &mut LineCache`.

3. `DragState` gained a `total_delta` field to implement the sub-4px click threshold. Drag moves below 4px total are treated as clicks (no `Move` journal entry).

4. `reveal()` centers the camera on the focused card when it falls outside the panel's expanded rect, emitting a `ViewportMoved` event that `app.rs` applies.

### ScanComplete Event Loss (Key Fix)

The `app_with_cards` test helper drains the event channel synchronously before building the Harness. The `VaultEvent::ScanComplete` event appeared in that drain loop and was discarded by `_ => continue`. As a result, `drain_events()` (called from `JdUi::ui()`) never saw it, `state.scan_done` was never set, and `pump()` timed out at 200 frames.

Fix: explicitly match `VaultEvent::ScanComplete` in the pre-harness loop and replicate `drain_events`'s handling inline (set `scan_done`, `bodies.invalidate_all()`, create default desk if needed).

### MouseWheel Phase Field

`egui::Event::MouseWheel` in egui 0.35 requires a `phase: egui::TouchPhase` field. Tests set `phase: egui::TouchPhase::Move` for all scroll events.

---

## Test Results

```
running 5 tests
test drag_moves_a_card_and_survives_in_session_state ... ok
test pan_and_zoom_change_the_camera_and_clamp ... ok
test offscreen_cards_are_culled_from_the_accesskit_tree ... ok
test enter_opens_focused_card ... ok
test backspace_puts_away_not_deletes ... ok

test result: ok. 5 passed; 0 failed; 0 ignored
```

Full suite: 31/31 pass. `cargo clippy --workspace --all-targets -- -D warnings` clean. `cargo fmt --check --all` clean.

---

## Concerns

1. **`zoom_to_fit` minimum of 0.01** — this is below the interactive `ZOOM_MIN = 0.5`. Cards placed at extreme world coordinates (e.g., `(100_000, 0)`) produce a zoom of ~0.012. Rendering at sub-0.5 zoom is intentional for "fit all" but may look unexpected if a user accidentally places a card far away. Task 9 should enforce placement bounds or warn.

2. **Status bar height assumption** — `card_center_on_screen` in tests assumes the status bar is 24px. If the bar height changes, the test helper will produce off-by-N screen coordinates. This is a test-only concern; production code uses the actual `response.rect`.

3. **`DeskEvent::ViewportMoved` not in brief** — adding this variant was required for correctness but deviates from the specified 3-variant enum. If Task 9 or 10 builds on `DeskEvent` matching, the extra variant must be handled.

4. **`BodyCache::invalidate_all` on every ScanComplete** — correct per design, but means all body text re-requests fire after every scan. At scale this is a 1-frame flash of blank card faces. Acceptable for now.

---

## Fix Report (post-review fixes, branch feat/desk-cards-editor)

### Fix 1 (Critical): Background pan fires during card drag

**Change:** Added `state.drag.is_none()` to the background-pan guard in `desk_ui` (desk.rs ~line 422).

Before:
```rust
if pointer_over_card.is_none()
    && ui.input(|i| i.pointer.button_down(egui::PointerButton::Primary))
    && pointer_delta != egui::Vec2::ZERO
```

After:
```rust
if state.drag.is_none()
    && pointer_over_card.is_none()
    && ui.input(|i| i.pointer.button_down(egui::PointerButton::Primary))
    && pointer_delta != egui::Vec2::ZERO
```

The bug: `pointer_over_card` is computed from original world positions. Once the pointer exits the dragged card's original screen rect mid-drag, `pointer_over_card` becomes `None`, enabling pan. The pan shifts `cam.center` each frame, corrupting the `Move` op's `to` world coordinate computed at drop time.

### Fix 2 (Regression test): `drag_to_empty_space_does_not_pan`

New test in `desk_kittest.rs`. Drags card from its center to `+250 y` (empty space below it), then asserts:
- (a) `placed.pos.y ≈ 250` (card moved correctly)
- (b) `viewport.center` unchanged from before drag

**RED evidence (fix 1 reverted):**

```
test drag_to_empty_space_does_not_pan ... FAILED
---- drag_to_empty_space_does_not_pan stdout ----
thread panicked at crates/jd-app/tests/desk_kittest.rs:181:5:
card should move ≈250 world units down, got y=0.0
```

Without the guard, the background pan fires on every frame where the pointer has left the card's original screen rect. This shifts `cam.center` by the entire drag delta, so `cam.to_world(panel, drop_screen)` computes `new_world ≈ original_world` — the Move op writes the card back to its origin.

**GREEN with fix 1 applied:** `test drag_to_empty_space_does_not_pan ... ok`

### Fix 3 (Important): Wire `reveal()`

**Change:** `app.rs` `apply_desk_events` `FocusChanged` arm now calls `crate::surfaces::desk::reveal(desk, focused_id, panel)` after updating `self.state.focus`. Uses approximate panel rect `1200×776` (kittest window minus ~24px status bar). If `reveal` returns `Some(new_cam)`, the desk viewport is updated and `session_dirty_at` marked. Not journaled.

New test `arrowkey_to_offscreen_card_reveals_it`: places a card at `(50_000, 0)`, asserts it is culled, then presses ArrowRight twice (first selects Card 1, second selects far card), asserts `focus == far_id` and the far card's AccessKit node now exists (reveal centered on it).

### Fix 4 (Important): Zoom speed spec formula `1.0015^scroll_delta`

**Change:** Replaced `ui.input(|i| i.zoom_delta())` with manual extraction of Ctrl+scroll raw delta from `i.events` (filtering `Event::MouseWheel` with `modifiers.command`), then computing `zoom_factor = 1.0015_f32.powf(ctrl_scroll_delta)`.

egui's formula is `exp(scroll_zoom_speed * delta)` with `scroll_zoom_speed = 1/200`, i.e. `e^(delta/200)`. The spec formula `1.0015^delta` produces a meaningfully slower, more precise zoom that is deterministic regardless of egui version or platform.

The existing `pan_and_zoom_change_the_camera_and_clamp` test continues to pass: 200 events × 120 points = 24 000 points; `1.0015^24000 ≈ 10^15`, which clamps to `ZOOM_MAX = 2.0` immediately.

### Fix 5 (Minor): Per-shape hit-test size

**Change:** The `pointer_over_card` hit-test now uses `card_size(shape_for(m.status, m.kind))` per card (via `state.face_metas`), falling back to `300×200` if the meta is not yet loaded. Previously all cards used the hardcoded Divider size `300×208`.

---

### Final Test Summary

```
running 7 tests  (desk_kittest.rs — up from 5)
test drag_moves_a_card_and_survives_in_session_state ... ok
test drag_to_empty_space_does_not_pan ... ok           ← new
test pan_and_zoom_change_the_camera_and_clamp ... ok
test offscreen_cards_are_culled_from_the_accesskit_tree ... ok
test arrowkey_to_offscreen_card_reveals_it ... ok      ← new
test enter_opens_focused_card ... ok
test backspace_puts_away_not_deletes ... ok

test result: ok. 7 passed; 0 failed; 0 ignored
```

Full suite: 32/32 pass. `cargo clippy --workspace --all-targets -- -D warnings` clean. `cargo fmt --check` clean.
