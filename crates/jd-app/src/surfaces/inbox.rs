//! Inbox surface: every `Status::Fleeting` note, oldest-first by `created`.
//!
//! Layout strategy:
//! - Paper style: scattered pile — each card gets a deterministic ±12 px jitter
//!   around a flowing grid using the same seeded-xorshift idiom as shape.rs tears.
//! - Plain style: tidy single column (no jitter).
//!
//! Focus: linear list order (Up/Down AND Left/Right move linearly).
//! Keyboard acts:
//!   Enter       → open editor (same OpenCard path as the desk).
//!   Ctrl+Enter  → promote path (wired; Task 4 will fill the impl).
//!   Del         → Toss (no confirm; journaled via OpDone as usual).
//!   Ctrl+D      → desk picker popup (place on chosen desk at viewport center;
//!                  card STAYS fleeting/inboxed — placement only).

use eframe::egui;
use jd_core::id::NoteId;
use jd_core::rng::Xorshift128;
use jd_core::session::{DeskId, SessionState};

use crate::card::shape::{CardStyle, RuledLines, shape_for};
use crate::surfaces::desk::FaceMeta;

// ---------------------------------------------------------------------------
// InboxEvent
// ---------------------------------------------------------------------------

/// Events emitted by `inbox_ui` for `app.rs` to apply.
#[derive(Debug)]
pub enum InboxEvent {
    /// Open the card editor for `id` (Enter / double-click).
    OpenCard(NoteId),
    /// Promote-without-typing (Ctrl+Enter); Task 4 fills the impl.
    Promote(NoteId),
    /// Toss the card (Del); no confirm, journaled via OpDone.
    Toss(NoteId),
    /// Place the card on `desk` at that desk's viewport center.
    /// Card stays fleeting/inboxed — this is placement only, not a status change.
    PlaceOnDesk { id: NoteId, desk: DeskId },
}

// ---------------------------------------------------------------------------
// InboxUiDeps
// ---------------------------------------------------------------------------

/// Everything `inbox_ui` reads but does not own.
pub struct InboxUiDeps<'a> {
    pub focus: &'a mut Option<NoteId>,
    pub bodies: &'a mut crate::state::BodyCache,
    pub commands: &'a std::sync::mpsc::Sender<jd_core::worker::VaultCommand>,
    pub theme: &'a crate::theme::Theme,
    pub line_cache: &'a mut crate::editor::LineCache,
    /// FaceMeta for all fleeting notes — prefetched under ONE index read lock in app.rs.
    pub face_metas: &'a [FaceMeta],
    /// Current session state — for the desk picker popup.
    pub session: &'a SessionState,
    /// Ordered list of fleeting note ids (oldest-first, by `created`).
    pub ordered_ids: &'a [NoteId],
    pub editor_open: bool,
    /// True while a delete-confirm modal is pending; suppresses all surface
    /// keyboard handling so the modal's Enter/Esc are the only consumers.
    pub confirm_pending: bool,
    /// True while the Ctrl+K palette overlay is open; suppresses all surface
    /// keyboard handling and focus-stealing (same gate pattern as confirm_pending).
    pub palette_open: bool,
}

// ---------------------------------------------------------------------------
// Card layout helpers
// ---------------------------------------------------------------------------

/// Jitter offset for Paper-style pile layout.
/// Seeded from `id` via xorshift so each card is deterministic and stable.
/// Returns (dx, dy) in the range ±12 px.
fn paper_jitter(id: NoteId) -> egui::Vec2 {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut h = DefaultHasher::new();
    id.to_string().as_bytes().hash(&mut h);
    let seed = h.finish();
    let mut rng = Xorshift128::new(seed);

    // Produce two independent ±12 px jitters
    let raw_x = rng.next_u64();
    let raw_y = rng.next_u64();
    let jitter = 12.0_f32;
    let dx = (raw_x & 0xFFFF) as f32 / 65535.0 * 2.0 * jitter - jitter;
    let dy = (raw_y & 0xFFFF) as f32 / 65535.0 * 2.0 * jitter - jitter;
    egui::vec2(dx, dy)
}

/// Grid layout for a card at position `index` in the inbox list.
/// Returns the top-left screen-space position within `panel`.
/// For Paper style, adds per-id jitter; for Plain, tidy column.
fn card_pos(index: usize, id: NoteId, style: CardStyle, panel: egui::Rect) -> egui::Pos2 {
    use crate::card::shape::{CardShape, card_size};
    let card_w = card_size(CardShape::Scrap).x;
    let card_h = card_size(CardShape::Scrap).y;

    // Grid: 3 columns for Paper, 1 for Plain
    let (cols, col_gap, row_gap, margin) = match style {
        CardStyle::Paper => (3_usize, 24.0_f32, 24.0_f32, 24.0_f32),
        CardStyle::Plain => (1_usize, 0.0_f32, 16.0_f32, 32.0_f32),
    };

    let col = index % cols;
    let row = index / cols;

    let base_x = panel.min.x + margin + col as f32 * (card_w + col_gap);
    let base_y = panel.min.y + margin + row as f32 * (card_h + row_gap);

    match style {
        CardStyle::Paper => {
            let jitter = paper_jitter(id);
            egui::pos2(base_x + jitter.x, base_y + jitter.y)
        }
        CardStyle::Plain => egui::pos2(base_x, base_y),
    }
}

// ---------------------------------------------------------------------------
// Desk picker popup state (stored in egui memory)
// ---------------------------------------------------------------------------

fn picker_state_id() -> egui::Id {
    egui::Id::new("inbox_desk_picker")
}

#[derive(Clone, Default)]
struct PickerState {
    open: bool,
    for_id: Option<NoteId>,
    /// Index of the currently highlighted desk in the picker list.
    highlight: usize,
}

// ---------------------------------------------------------------------------
// inbox_ui
// ---------------------------------------------------------------------------

/// Render the inbox surface; returns events for app.rs to apply.
/// `inbox_ui` never mutates session state directly — it only emits events.
pub fn inbox_ui(ui: &mut egui::Ui, deps: &mut InboxUiDeps<'_>) -> Vec<InboxEvent> {
    let mut events: Vec<InboxEvent> = Vec::new();
    let ids = deps.ordered_ids;

    if ids.is_empty() {
        ui.centered_and_justified(|ui| {
            ui.label(egui::RichText::new("Inbox is empty").weak().size(16.0));
        });
        return events;
    }

    // Determine card style. All fleeting notes are Scraps — use Paper.
    // (WP3 has no settings UI; hardcode Paper consistent with the desk surface.)
    let style = CardStyle::Paper;

    let panel = ui.max_rect();

    // -------------------------------------------------------------------------
    // Keyboard handling (only when editor is closed and picker is not open)
    // -------------------------------------------------------------------------
    let picker_open = ui
        .memory(|m| m.data.get_temp::<PickerState>(picker_state_id()))
        .is_some_and(|p| p.open);

    if !deps.editor_open && !picker_open && !deps.confirm_pending && !deps.palette_open {
        // Up/Down/Left/Right: linear navigation
        let go_prev =
            ui.input(|i| i.key_pressed(egui::Key::ArrowUp) || i.key_pressed(egui::Key::ArrowLeft));
        let go_next = ui
            .input(|i| i.key_pressed(egui::Key::ArrowDown) || i.key_pressed(egui::Key::ArrowRight));

        if go_prev || go_next {
            let current_idx = deps.focus.and_then(|f| ids.iter().position(|id| *id == f));
            let next_idx = if go_next {
                match current_idx {
                    None => Some(0),
                    Some(i) if i + 1 < ids.len() => Some(i + 1),
                    _ => None,
                }
            } else {
                match current_idx {
                    None if !ids.is_empty() => Some(ids.len() - 1),
                    Some(i) if i > 0 => Some(i - 1),
                    _ => None,
                }
            };
            if let Some(idx) = next_idx {
                *deps.focus = Some(ids[idx]);
            }
        }

        // Enter → open editor (plain Enter, not Ctrl+Enter)
        if ui.input(|i| i.key_pressed(egui::Key::Enter) && !i.modifiers.command)
            && let Some(id) = *deps.focus
        {
            events.push(InboxEvent::OpenCard(id));
        }

        // Ctrl+Enter → promote (wired; no-op until Task 4)
        let ctrl_enter = ui.input(|i| {
            i.events.iter().any(|e| {
                matches!(
                    e,
                    egui::Event::Key {
                        key: egui::Key::Enter,
                        pressed: true,
                        modifiers,
                        ..
                    } if modifiers.command
                )
            })
        });
        if ctrl_enter && let Some(id) = *deps.focus {
            events.push(InboxEvent::Promote(id));
            // wired in promotion task
        }

        // Del → Toss (no confirm)
        if ui.input(|i| i.key_pressed(egui::Key::Delete))
            && let Some(id) = *deps.focus
        {
            // Advance focus before removal so the user stays near their position.
            let current_idx = ids.iter().position(|i| *i == id).unwrap_or(0);
            let next_focus = if current_idx + 1 < ids.len() {
                Some(ids[current_idx + 1])
            } else if current_idx > 0 {
                Some(ids[current_idx - 1])
            } else {
                None
            };
            *deps.focus = next_focus;
            events.push(InboxEvent::Toss(id));
        }

        // Ctrl+D → open desk picker for focused card
        let ctrl_d = ui.input(|i| {
            i.events.iter().any(|e| {
                matches!(
                    e,
                    egui::Event::Key {
                        key: egui::Key::D,
                        pressed: true,
                        modifiers,
                        ..
                    } if modifiers.command
                )
            })
        });
        if ctrl_d
            && let Some(id) = *deps.focus
            && !deps.session.desks.is_empty()
        {
            let state = PickerState {
                open: true,
                for_id: Some(id),
                highlight: 0,
            };
            ui.memory_mut(|m| m.data.insert_temp(picker_state_id(), state));
        }
    }

    // -------------------------------------------------------------------------
    // Desk picker popup (Ctrl+D)
    // -------------------------------------------------------------------------
    if picker_open {
        let picker = ui
            .memory(|m| m.data.get_temp::<PickerState>(picker_state_id()))
            .unwrap_or_default();

        // Key handling inside the picker
        let enter_pressed = ui.input(|i| i.key_pressed(egui::Key::Enter));
        let esc_pressed = ui.input(|i| i.key_pressed(egui::Key::Escape));
        let down_pressed = ui.input(|i| i.key_pressed(egui::Key::ArrowDown));
        let up_pressed = ui.input(|i| i.key_pressed(egui::Key::ArrowUp));

        let desk_count = deps.session.desks.len();
        let mut highlight = picker.highlight;
        if down_pressed && highlight + 1 < desk_count {
            highlight += 1;
        }
        if up_pressed && highlight > 0 {
            highlight -= 1;
        }

        if esc_pressed {
            ui.memory_mut(|m| {
                m.data
                    .insert_temp(picker_state_id(), PickerState::default())
            });
        } else if enter_pressed {
            if let Some(for_id) = picker.for_id
                && let Some(desk) = deps.session.desks.get(highlight)
            {
                events.push(InboxEvent::PlaceOnDesk {
                    id: for_id,
                    desk: desk.id,
                });
            }
            ui.memory_mut(|m| {
                m.data
                    .insert_temp(picker_state_id(), PickerState::default())
            });
        } else {
            // Update highlight if it changed
            ui.memory_mut(|m| {
                let s = m
                    .data
                    .get_temp_mut_or(picker_state_id(), PickerState::default());
                s.highlight = highlight;
            });
        }

        // Render picker as a small window in the center.
        let panel_center = panel.center();
        let picker_width = 280.0_f32;
        let row_h = 28.0_f32;
        let picker_h = 8.0 + desk_count as f32 * row_h + 8.0;
        let picker_rect =
            egui::Rect::from_center_size(panel_center, egui::vec2(picker_width, picker_h));

        // Collect (row_rect, desk_id, desk_name, is_hl) before any painting.
        let rows: Vec<(egui::Rect, DeskId, String, bool)> = deps
            .session
            .desks
            .iter()
            .enumerate()
            .map(|(i, desk)| {
                let row_rect = egui::Rect::from_min_size(
                    egui::pos2(
                        picker_rect.min.x,
                        picker_rect.min.y + 8.0 + i as f32 * row_h,
                    ),
                    egui::vec2(picker_width, row_h),
                );
                (row_rect, desk.id, desk.name.clone(), i == highlight)
            })
            .collect();

        // Allocate interactive rects first (mutable borrow of ui).
        let mut row_clicked: Option<DeskId> = None;
        for (row_rect, desk_id, desk_name, is_hl) in &rows {
            let label = format!("Desk: {desk_name}");
            let row_resp = ui.allocate_rect(*row_rect, egui::Sense::click());
            row_resp.widget_info(|| {
                egui::WidgetInfo::labeled(egui::WidgetType::Button, *is_hl, label.as_str())
            });
            if row_resp.clicked() {
                row_clicked = Some(*desk_id);
            }
        }

        // Now paint (painter borrows ui immutably).
        {
            let painter = ui.painter();
            painter.rect_filled(picker_rect, 6.0, deps.theme.card_plain_bg);
            painter.rect_stroke(
                picker_rect,
                6.0,
                egui::Stroke::new(1.0, deps.theme.card_border),
                egui::StrokeKind::Outside,
            );
            for (row_rect, _desk_id, desk_name, is_hl) in &rows {
                if *is_hl {
                    painter.rect_filled(*row_rect, 4.0, deps.theme.focus_ring.gamma_multiply(0.25));
                }
                let font = egui::FontId::new(14.0, egui::FontFamily::Proportional);
                painter.text(
                    egui::pos2(row_rect.min.x + 12.0, row_rect.center().y),
                    egui::Align2::LEFT_CENTER,
                    desk_name.as_str(),
                    font,
                    deps.theme.text,
                );
            }
        }

        // Handle click after painting.
        if let Some(clicked_desk) = row_clicked
            && let Some(for_id) = picker.for_id
        {
            events.push(InboxEvent::PlaceOnDesk {
                id: for_id,
                desk: clicked_desk,
            });
            ui.memory_mut(|m| {
                m.data
                    .insert_temp(picker_state_id(), PickerState::default())
            });
        }

        // Absorb keyboard so the picker doesn't bleed onto card actions.
        // Returning here means card rendering below is skipped while picker is open.
        return events;
    }

    // -------------------------------------------------------------------------
    // Render card faces
    // -------------------------------------------------------------------------
    for (idx, &id) in ids.iter().enumerate() {
        let meta_opt = deps.face_metas.iter().find(|m| m.id == id);

        let card_size = crate::card::shape::card_size(crate::card::shape::CardShape::Scrap);
        let top_left = card_pos(idx, id, style, panel);
        let rect = egui::Rect::from_min_size(top_left, card_size);

        // Cull cards fully outside the panel
        if !panel.expand(200.0).intersects(rect) {
            continue;
        }

        let is_focused = *deps.focus == Some(id);

        let (title, body_str, links, tags, source, shape) = if let Some(meta) = meta_opt {
            let sh = shape_for(meta.status, meta.kind);
            (
                meta.title.as_str(),
                deps.bodies
                    .get_or_request(id, deps.commands)
                    .map(|b| b.text.as_str()),
                meta.links,
                meta.tags,
                meta.source.as_deref(),
                sh,
            )
        } else {
            (
                "",
                deps.bodies
                    .get_or_request(id, deps.commands)
                    .map(|b| b.text.as_str()),
                0usize,
                0usize,
                None,
                crate::card::shape::CardShape::Scrap,
            )
        };

        let face = crate::card::CardFace {
            id,
            title,
            body: body_str,
            shape,
            style,
            lines: RuledLines::Natural,
            source,
            links,
            tags,
            focused: is_focused,
        };

        let (resp, _checkbox_ordinal) =
            crate::card::card_face(ui, rect, &face, deps.theme, deps.line_cache);
        // Inbox face: checkbox click-to-toggle not supported (no VaultOp channel here);
        // ignore toggled_ordinal — cards in the inbox are edited via the editor instead.

        if resp.clicked() && *deps.focus != Some(id) {
            *deps.focus = Some(id);
        }
        // Gated while the palette overlay is open (same discipline as the
        // keyboard block): a double-click behind the palette must not open
        // the editor.
        if resp.double_clicked() && !deps.palette_open {
            events.push(InboxEvent::OpenCard(id));
        }
        if is_focused && !deps.editor_open && !deps.palette_open {
            resp.request_focus();
        }
    }

    events
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paper_jitter_is_deterministic_and_bounded() {
        use jd_core::id::NoteId;
        let id = NoteId::parse("01ARZ3NDEKTSV4RRFFQ69G5FA1").unwrap();
        let a = paper_jitter(id);
        let b = paper_jitter(id);
        assert_eq!(a, b, "jitter must be deterministic");
        assert!(a.x.abs() <= 12.0, "|dx| <= 12: got {}", a.x);
        assert!(a.y.abs() <= 12.0, "|dy| <= 12: got {}", a.y);
    }

    #[test]
    fn paper_jitter_differs_per_id() {
        use jd_core::id::NoteId;
        let a = NoteId::parse("01ARZ3NDEKTSV4RRFFQ69G5FA1").unwrap();
        let b = NoteId::parse("01ARZ3NDEKTSV4RRFFQ69G5FA2").unwrap();
        // Very unlikely to be equal by coincidence
        assert_ne!(paper_jitter(a), paper_jitter(b));
    }

    #[test]
    fn card_pos_plain_layout_single_column_increasing_y() {
        use jd_core::id::NoteId;
        let panel = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1200.0, 800.0));
        let id = NoteId::parse("01ARZ3NDEKTSV4RRFFQ69G5FA1").unwrap();
        let margin = 32.0;
        let row_gap = 16.0;
        let card_h = crate::card::shape::card_size(crate::card::shape::CardShape::Scrap).y;

        // Plain layout: single column, consecutive indices should have same x, increasing y
        let pos0 = card_pos(0, id, CardStyle::Plain, panel);
        let pos1 = card_pos(1, id, CardStyle::Plain, panel);
        let pos2 = card_pos(2, id, CardStyle::Plain, panel);

        // All positions should have the same x (left margin)
        assert_eq!(
            pos0.x, pos1.x,
            "Plain layout column 0 and 1 must have same x"
        );
        assert_eq!(
            pos1.x, pos2.x,
            "Plain layout column 1 and 2 must have same x"
        );

        // y should increase by card_h + row_gap for each consecutive position
        assert_eq!(
            pos0.x,
            panel.min.x + margin,
            "Plain layout x must be at margin"
        );
        assert_eq!(
            pos0.y,
            panel.min.y + margin,
            "Plain layout first position y must be at margin"
        );
        assert_eq!(
            pos1.y,
            pos0.y + card_h + row_gap,
            "Plain layout position 1 y must be position 0 y + card_h + row_gap"
        );
        assert_eq!(
            pos2.y,
            pos1.y + card_h + row_gap,
            "Plain layout position 2 y must be position 1 y + card_h + row_gap"
        );
    }
}
