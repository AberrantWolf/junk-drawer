mod common;

use eframe::egui;
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use jd_core::geom::Vec2;

/// Minimal spike canvas: real focus machinery, dummy rendering.
/// Proves: cards exist as labeled AccessKit nodes; arrows move focus spatially.
struct SpikeDesk {
    cards: Vec<(jd_core::id::NoteId, Vec2, String)>, // id, pos, title
    focus: Option<jd_core::id::NoteId>,
}

impl SpikeDesk {
    fn ui(&mut self, ui: &mut egui::Ui) {
        use jd_app::surfaces::desk::{FocusDir, card_a11y_label, next_focus};
        let positions: Vec<_> = self.cards.iter().map(|(id, p, _)| (*id, *p)).collect();
        // Arrow handling BEFORE widgets, mirroring the real desk's dispatch.
        for (key, dir) in [
            (egui::Key::ArrowLeft, FocusDir::Left),
            (egui::Key::ArrowRight, FocusDir::Right),
            (egui::Key::ArrowUp, FocusDir::Up),
            (egui::Key::ArrowDown, FocusDir::Down),
        ] {
            if ui.input(|i| i.key_pressed(key))
                && let Some(next) = next_focus(&positions, self.focus, dir)
            {
                self.focus = Some(next);
            }
        }
        for (id, pos, title) in &self.cards {
            let rect = egui::Rect::from_min_size(
                egui::pos2(pos.x, pos.y) + ui.min_rect().min.to_vec2(),
                egui::vec2(150.0, 90.0),
            );
            let label = card_a11y_label(title, "", false, 0, 0);
            let resp = ui
                .allocate_rect(rect, egui::Sense::click())
                .on_hover_text(title);
            // The spike's core claim: a free-form-positioned widget can carry
            // proper AccessKit semantics via allocate_rect + widget_info.
            resp.widget_info(|| {
                egui::WidgetInfo::labeled(egui::WidgetType::Button, true, label.clone())
            });
            if self.focus == Some(*id) {
                resp.request_focus();
                ui.painter().rect_stroke(
                    rect,
                    4.0,
                    egui::Stroke::new(2.0, ui.visuals().selection.stroke.color),
                    egui::StrokeKind::Outside,
                );
            } else {
                ui.painter().rect_stroke(
                    rect,
                    4.0,
                    egui::Stroke::new(1.0, ui.visuals().weak_text_color()),
                    egui::StrokeKind::Outside,
                );
            }
        }
    }
}

fn make_desk() -> SpikeDesk {
    let ids: Vec<_> = (1..=4u8)
        .map(|n| {
            let s = format!("01ARZ3NDEKTSV4RRFFQ69G5F{n:02}");
            jd_core::id::NoteId::parse(&s).unwrap()
        })
        .collect();
    SpikeDesk {
        cards: vec![
            (ids[0], Vec2 { x: 20.0, y: 20.0 }, "Alpha".into()),
            (ids[1], Vec2 { x: 300.0, y: 30.0 }, "Beta".into()),
            (ids[2], Vec2 { x: 40.0, y: 250.0 }, "Gamma".into()),
            (ids[3], Vec2 { x: 320.0, y: 260.0 }, "Delta".into()),
        ],
        focus: None,
    }
}

#[test]
fn every_card_is_a_labeled_accesskit_node() {
    let mut h = Harness::builder().build_ui_state(|ui, d: &mut SpikeDesk| d.ui(ui), make_desk());
    h.run_ok();
    for name in ["Alpha", "Beta", "Gamma", "Delta"] {
        h.get_by_label_contains(&format!("Card: '{name}'"));
    }
}

#[test]
fn arrows_walk_reading_order_across_the_canvas() {
    let mut h = Harness::builder().build_ui_state(|ui, d: &mut SpikeDesk| d.ui(ui), make_desk());
    h.run_ok();
    let ids = make_desk()
        .cards
        .iter()
        .map(|(id, _, _)| *id)
        .collect::<Vec<_>>();
    h.key_press(egui::Key::ArrowRight);
    h.run_ok();
    assert_eq!(h.state().focus, Some(ids[0]), "first Right lands on Alpha");
    h.key_press(egui::Key::ArrowRight);
    h.run_ok();
    assert_eq!(h.state().focus, Some(ids[1]), "Beta is next in band 0");
    h.key_press(egui::Key::ArrowDown);
    h.run_ok();
    assert_eq!(
        h.state().focus,
        Some(ids[3]),
        "Down from Beta → Delta (nearest |dx|)"
    );
    h.key_press(egui::Key::ArrowLeft);
    h.run_ok();
    assert_eq!(h.state().focus, Some(ids[2]), "Left in band 2 → Gamma");
    h.key_press(egui::Key::ArrowLeft);
    h.run_ok();
    // Reading order: Left from Gamma goes back up to Beta (previous in order).
    assert_eq!(h.state().focus, Some(ids[1]));
}
