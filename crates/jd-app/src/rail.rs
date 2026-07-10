//! Left rail: desk list, fixed nav rows (Inbox/Drawer/Map/Trash), surface switching.
//! Follows the same events-out pattern as desk_ui: rail_ui returns Vec<RailEvent>;
//! app.rs is the single mutation site.

use eframe::egui;
use jd_core::geom::Vec2;
use jd_core::id::{IdGen, NoteId};
use jd_core::session::{DeskId, SessionState, SurfaceId};

// ---------------------------------------------------------------------------
// RailEvent
// ---------------------------------------------------------------------------

/// Events emitted by `rail_ui` for `app.rs` to apply.
#[derive(Debug)]
pub enum RailEvent {
    /// Switch to a surface (not journaled — navigation is not undoable).
    Switch(SurfaceId),
    /// Create a new desk (journaled "Create desk").
    CreateDesk,
    /// Rename a desk (journaled "Rename desk").
    RenameDesk { id: DeskId, name: String },
    /// Reorder a desk to a new index (journaled "Reorder desk").
    ReorderDesk { id: DeskId, to: usize },
    /// A dragged card was dropped on the Inbox row (put away from source desk).
    /// Source position is included so app.rs can build the correct inverse.
    CardDroppedOnInbox {
        id: NoteId,
        source_desk: DeskId,
        was_at: Vec2,
    },
    /// A dragged card was dropped on a specific desk row.
    /// Composite journal: ONE entry "Move card to desk '<name>'" with inverse =
    /// Session(Place back on source desk at old pos). See WP3 task-2-brief.md.
    CardDroppedOnDesk {
        target_desk: DeskId,
        id: NoteId,
        source_desk: DeskId,
        was_at: Vec2,
    },
}

// ---------------------------------------------------------------------------
// RailUiDeps
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// RailDropTarget
// ---------------------------------------------------------------------------

/// Where a dragged card should land when dropped on a rail row.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum RailDropTarget {
    Inbox,
    Desk(DeskId),
}

// ---------------------------------------------------------------------------
// RailUiDeps
// ---------------------------------------------------------------------------

/// Everything `rail_ui` reads but does not own.
pub struct RailUiDeps<'a> {
    pub session: &'a SessionState,
    pub theme: &'a crate::theme::Theme,
    /// Number of fleeting notes in the index — computed once per frame in app.rs
    /// under the existing FaceMeta lock pattern.
    pub inbox_count: usize,
    pub id_gen: &'a mut IdGen,
    /// Output: each rail row's screen rect + its drop target.
    /// Cleared and repopulated every frame so desk_ui's drag-release path can
    /// hit-test against it to decide whether to emit CardDroppedOnInbox /
    /// CardDroppedOnDesk instead of a plain Move.
    /// app.rs stores this on JdUi.rail_row_hits between frames.
    pub row_hits: &'a mut Vec<(egui::Rect, RailDropTarget)>,
}

// ---------------------------------------------------------------------------
// Per-desk rename state (stored in egui memory)
// ---------------------------------------------------------------------------

/// Per-session rename state stored in egui's ephemeral memory.
#[derive(Clone, Default)]
struct RenameState {
    desk_id: Option<DeskId>,
    buffer: String,
}

fn rename_state_id() -> egui::Id {
    egui::Id::new("rail_rename_state")
}

// ---------------------------------------------------------------------------
// rail_ui
// ---------------------------------------------------------------------------

/// Render the left rail and return events for app.rs to apply.
/// Full-width rail row: fixed height, LEFT-aligned text, themed hover/active
/// fills and an accent bar on the active row. Painted directly (not
/// `Button::selectable`) so the colors are the THEME's — egui's stock
/// selection blue never appears — and every drawn pair is WCAG-tested.
fn rail_row(
    ui: &mut egui::Ui,
    th: &crate::theme::Theme,
    selected: bool,
    text: &str,
) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), crate::theme::RAIL_ROW_H),
        egui::Sense::click(),
    );
    let fill = if selected {
        th.rail_active_bg
    } else if resp.hovered() {
        th.rail_hover_bg
    } else {
        egui::Color32::TRANSPARENT
    };
    let p = ui.painter();
    p.rect_filled(rect, crate::theme::CARD_CORNER_RADIUS, fill);
    if selected {
        let bar = egui::Rect::from_min_max(rect.min, egui::pos2(rect.min.x + 3.0, rect.max.y));
        p.rect_filled(bar, 0.0, th.accent);
    }
    p.text(
        egui::pos2(rect.min.x + 10.0, rect.center().y),
        egui::Align2::LEFT_CENTER,
        text,
        egui::FontId::new(crate::editor::BODY_SIZE, egui::FontFamily::Proportional),
        th.text,
    );
    resp
}

/// The rail is always visible regardless of the current surface.
///
/// Row rects are recorded into `deps.row_hits` (cleared first) each frame so
/// desk_ui's drag-release path can hit-test them without an extra pass.
pub fn rail_ui(ui: &mut egui::Ui, deps: &mut RailUiDeps<'_>) -> Vec<RailEvent> {
    let mut events: Vec<RailEvent> = Vec::new();
    let current = deps.session.current_surface;

    // Clear last frame's hits; we'll repopulate below.
    deps.row_hits.clear();

    // ── Desk list ────────────────────────────────────────────────────────────
    let desks: Vec<(DeskId, String)> = deps
        .session
        .desks
        .iter()
        .map(|d| (d.id, d.name.clone()))
        .collect();
    let desk_count = desks.len();

    for (idx, (desk_id, desk_name)) in desks.iter().enumerate() {
        let is_current = current == Some(SurfaceId::Desk(*desk_id));

        // Check rename state.
        let renaming_this = ui
            .memory(|m| m.data.get_temp::<RenameState>(rename_state_id()))
            .as_ref()
            .is_some_and(|r| r.desk_id == Some(*desk_id));

        if renaming_this {
            // Inline rename TextEdit.
            let mut buf = ui
                .memory(|m| m.data.get_temp::<RenameState>(rename_state_id()))
                .map(|r| r.buffer.clone())
                .unwrap_or_else(|| desk_name.clone());
            let resp = ui.text_edit_singleline(&mut buf);
            // Update buffer in memory.
            let new_buf = buf.clone();
            ui.memory_mut(|m| {
                let s = m
                    .data
                    .get_temp_mut_or(rename_state_id(), RenameState::default());
                s.buffer = new_buf;
            });
            let enter_pressed = ui.input(|i| i.key_pressed(egui::Key::Enter));
            let esc_pressed = ui.input(|i| i.key_pressed(egui::Key::Escape));
            // Escape → cancel (discard).
            // Enter OR lost_focus (without Escape) → commit if name is non-empty and changed.
            // Empty name on commit trigger → cancel (close editor, revert).
            let cancelled = esc_pressed;
            let commit_trigger = !cancelled && (enter_pressed || resp.lost_focus());
            if cancelled {
                ui.memory_mut(|m| {
                    m.data
                        .insert_temp(rename_state_id(), RenameState::default())
                });
            } else if commit_trigger {
                let trimmed = buf.trim().to_owned();
                if !trimmed.is_empty() && trimmed != *desk_name {
                    events.push(RailEvent::RenameDesk {
                        id: *desk_id,
                        name: trimmed,
                    });
                }
                // Close editor whether or not we committed (empty or same-name = revert).
                ui.memory_mut(|m| {
                    m.data
                        .insert_temp(rename_state_id(), RenameState::default())
                });
            }
        } else {
            let label_text = format!("Desk: {desk_name}");
            let resp = rail_row(ui, deps.theme, is_current, desk_name.as_str());
            // Override AccessKit label to include "Desk: " prefix per spec.
            resp.widget_info(|| {
                egui::WidgetInfo::labeled(
                    egui::WidgetType::SelectableLabel,
                    is_current,
                    label_text.as_str(),
                )
            });

            // Record this row's screen rect for drag-to-rail hit testing.
            deps.row_hits
                .push((resp.rect, RailDropTarget::Desk(*desk_id)));

            if resp.clicked() {
                events.push(RailEvent::Switch(SurfaceId::Desk(*desk_id)));
            }
            if resp.double_clicked() {
                // Enter inline rename mode.
                let state = RenameState {
                    desk_id: Some(*desk_id),
                    buffer: desk_name.clone(),
                };
                ui.memory_mut(|m| m.data.insert_temp(rename_state_id(), state));
            }

            // Context menu with keyboard-accessible reorder.
            // Drag reorder is optional; Move Up/Move Down are REQUIRED (no-spatial-only law).
            resp.context_menu(|ui| {
                if ui.button("Rename").clicked() {
                    let state = RenameState {
                        desk_id: Some(*desk_id),
                        buffer: desk_name.clone(),
                    };
                    ui.memory_mut(|m| m.data.insert_temp(rename_state_id(), state));
                    ui.close();
                }
                ui.add_enabled_ui(idx > 0, |ui| {
                    if ui.button("Move Up").clicked() {
                        events.push(RailEvent::ReorderDesk {
                            id: *desk_id,
                            to: idx - 1,
                        });
                        ui.close();
                    }
                });
                ui.add_enabled_ui(idx + 1 < desk_count, |ui| {
                    if ui.button("Move Down").clicked() {
                        events.push(RailEvent::ReorderDesk {
                            id: *desk_id,
                            to: idx + 1,
                        });
                        ui.close();
                    }
                });
            });
        }
    }

    // "+" button to create a new desk.
    let add_resp = ui.button("+");
    add_resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, false, "Add desk"));
    if add_resp.clicked() {
        events.push(RailEvent::CreateDesk);
    }

    ui.separator();

    // ── Fixed nav rows ───────────────────────────────────────────────────────
    let inbox_label = match deps.inbox_count {
        0 => "Inbox".to_owned(),
        1 => "Inbox, 1 scrap".to_owned(),
        n => format!("Inbox, {n} scraps"),
    };
    let inbox_selected = current == Some(SurfaceId::Inbox);
    let inbox_resp = rail_row(ui, deps.theme, inbox_selected, inbox_label.as_str());
    inbox_resp.widget_info(|| {
        egui::WidgetInfo::labeled(
            egui::WidgetType::SelectableLabel,
            inbox_selected,
            inbox_label.as_str(),
        )
    });
    // Record Inbox row rect for drag-to-rail hit testing.
    deps.row_hits.push((inbox_resp.rect, RailDropTarget::Inbox));
    if inbox_resp.clicked() {
        events.push(RailEvent::Switch(SurfaceId::Inbox));
    }

    for (label, surface) in [
        ("Drawer", SurfaceId::Drawer),
        ("Map", SurfaceId::Map),
        ("Trash", SurfaceId::Trash),
    ] {
        let is_sel = current == Some(surface);
        let resp = rail_row(ui, deps.theme, is_sel, label);
        resp.widget_info(|| {
            egui::WidgetInfo::labeled(egui::WidgetType::SelectableLabel, is_sel, label)
        });
        if resp.clicked() {
            events.push(RailEvent::Switch(surface));
        }
    }

    let _ = deps.id_gen; // id_gen used by CreateDesk path in app.rs

    events
}
