//! Shared desk-picker popup (Ctrl+D): pick a desk to place a card on.
//!
//! Extracted from the Inbox surface (WP3) so the Drawer (WP4 Task 4) reuses
//! the exact same component: state lives in egui memory keyed by a caller
//! `egui::Id` (one picker per surface), Up/Down move the highlight, Enter or
//! click picks, Esc dismisses. Rows are AccessKit-labeled "Desk: <name>".

use eframe::egui;
use jd_core::id::NoteId;
use jd_core::session::{DeskId, SessionState};

#[derive(Clone, Default)]
pub struct PickerState {
    pub open: bool,
    pub for_id: Option<NoteId>,
    /// Index of the currently highlighted desk in the picker list.
    pub highlight: usize,
}

/// True while the picker keyed by `key` is open.
pub fn is_open(ui: &egui::Ui, key: egui::Id) -> bool {
    ui.memory(|m| m.data.get_temp::<PickerState>(key))
        .is_some_and(|p| p.open)
}

/// Open the picker keyed by `key` for card `id` (highlight resets to 0).
pub fn open_for(ui: &egui::Ui, key: egui::Id, id: NoteId) {
    ui.memory_mut(|m| {
        m.data.insert_temp(
            key,
            PickerState {
                open: true,
                for_id: Some(id),
                highlight: 0,
            },
        )
    });
}

/// Render the picker popup centered in `panel` and handle its keys.
/// Returns `Some((card_id, desk_id))` when a desk is chosen (Enter or click);
/// the picker closes itself on pick and on Esc.
///
/// Call only while `is_open(ui, key)`; the caller must skip its own keyboard
/// handling for that frame (the picker absorbs Enter/Esc/arrows).
pub fn desk_picker_ui(
    ui: &mut egui::Ui,
    key: egui::Id,
    panel: egui::Rect,
    session: &SessionState,
    theme: &crate::theme::Theme,
) -> Option<(NoteId, DeskId)> {
    let picker = ui
        .memory(|m| m.data.get_temp::<PickerState>(key))
        .unwrap_or_default();
    let mut picked: Option<(NoteId, DeskId)> = None;

    // Key handling inside the picker.
    let enter_pressed = ui.input(|i| i.key_pressed(egui::Key::Enter));
    let esc_pressed = ui.input(|i| i.key_pressed(egui::Key::Escape));
    let down_pressed = ui.input(|i| i.key_pressed(egui::Key::ArrowDown));
    let up_pressed = ui.input(|i| i.key_pressed(egui::Key::ArrowUp));

    let desk_count = session.desks.len();
    let mut highlight = picker.highlight;
    if down_pressed && highlight + 1 < desk_count {
        highlight += 1;
    }
    if up_pressed && highlight > 0 {
        highlight -= 1;
    }

    if esc_pressed {
        ui.memory_mut(|m| m.data.insert_temp(key, PickerState::default()));
    } else if enter_pressed {
        if let Some(for_id) = picker.for_id
            && let Some(desk) = session.desks.get(highlight)
        {
            picked = Some((for_id, desk.id));
        }
        ui.memory_mut(|m| m.data.insert_temp(key, PickerState::default()));
    } else {
        // Update highlight if it changed.
        ui.memory_mut(|m| {
            let s = m.data.get_temp_mut_or(key, PickerState::default());
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
    let rows: Vec<(egui::Rect, DeskId, String, bool)> = session
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
        painter.rect_filled(picker_rect, 6.0, theme.card_plain_bg);
        painter.rect_stroke(
            picker_rect,
            6.0,
            egui::Stroke::new(1.0, theme.card_border),
            egui::StrokeKind::Outside,
        );
        for (row_rect, _desk_id, desk_name, is_hl) in &rows {
            if *is_hl {
                painter.rect_filled(*row_rect, 4.0, theme.focus_ring.gamma_multiply(0.25));
            }
            let font = egui::FontId::new(14.0, egui::FontFamily::Proportional);
            painter.text(
                egui::pos2(row_rect.min.x + 12.0, row_rect.center().y),
                egui::Align2::LEFT_CENTER,
                desk_name.as_str(),
                font,
                theme.text,
            );
        }
    }

    // Handle click after painting.
    if let Some(clicked_desk) = row_clicked
        && let Some(for_id) = picker.for_id
    {
        picked = Some((for_id, clicked_desk));
        ui.memory_mut(|m| m.data.insert_temp(key, PickerState::default()));
    }

    picked
}
