mod common;

use eframe::egui;
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;

const SAMPLE: &str = "# The heading line\nbody text under it\nmore body";

fn edit_harness(initial: &str) -> Harness<'static, String> {
    let mut cache = jd_app::editor::LineCache::default();
    Harness::builder().build_ui_state(
        move |ui, text: &mut String| {
            let mut layouter = |ui: &egui::Ui, buf: &dyn egui::TextBuffer, wrap: f32| {
                jd_app::editor::layout_body(ui, buf.as_str(), wrap, &mut cache, &|_| false)
            };
            ui.add(
                egui::TextEdit::multiline(text)
                    .desired_width(400.0)
                    .layouter(&mut layouter),
            );
        },
        initial.to_owned(),
    )
}

/// Exit criterion 1: the galley really is mixed-size (heading row taller than body row).
/// Uses a kittest Harness to get a real Ui context.
#[test]
fn heading_row_is_taller_than_body_row() {
    // Capture galley data out of the closure via shared state.
    let h0_cell = std::cell::Cell::new(0.0f32);
    let h1_cell = std::cell::Cell::new(0.0f32);
    let rows_cell = std::cell::Cell::new(0usize);

    let h0_ref = &h0_cell;
    let h1_ref = &h1_cell;
    let rows_ref = &rows_cell;

    let mut harness = egui_kittest::Harness::new_ui(move |ui| {
        let mut cache = jd_app::editor::LineCache::default();
        let galley = jd_app::editor::layout_body(ui, SAMPLE, 400.0, &mut cache, &|_| false);
        rows_ref.set(galley.rows.len());
        if galley.rows.len() >= 2 {
            h0_ref.set(galley.rows[0].rect().height());
            h1_ref.set(galley.rows[1].rect().height());
        }
    });
    harness.run_ok();

    let rows = rows_cell.get();
    let h0 = h0_cell.get();
    let h1 = h1_cell.get();
    assert!(rows >= 3, "expected one row per line, got {rows}");
    assert!(
        h0 > h1 * 1.3,
        "heading row ({h0}) must be visibly taller than body ({h1})"
    );
}

/// Exit criterion 2: typing at the end of the heading line inserts THERE, not
/// at a drifted position (cursor mapping across the size boundary is sound).
#[test]
fn typing_across_size_boundary_lands_where_the_cursor_is() {
    let mut h = edit_harness(SAMPLE);
    h.run_ok();
    let edit = h.get_by_role(egui::accesskit::Role::MultilineTextInput);
    edit.click();
    h.run_ok();
    // Move to top of buffer (Ctrl+Home = doc start in egui) then end of line 1.
    h.key_press_modifiers(egui::Modifiers::CTRL, egui::Key::Home);
    h.run_ok();
    h.key_press(egui::Key::End);
    h.run_ok();
    h.get_by_role(egui::accesskit::Role::MultilineTextInput)
        .type_text("!");
    h.run_ok();
    assert!(
        h.state()
            .starts_with("# The heading line!\nbody text under it"),
        "insert landed at end of the heading line, got: {}",
        h.state()
    );
    // And across the boundary: ArrowDown+End then type — lands at end of line 2.
    h.key_press(egui::Key::ArrowDown);
    h.key_press(egui::Key::End);
    h.run_ok();
    h.get_by_role(egui::accesskit::Role::MultilineTextInput)
        .type_text("?");
    h.run_ok();
    assert!(
        h.state().contains("body text under it?"),
        "got: {}",
        h.state()
    );
}

/// Exit criterion 3: select-all covers the full raw source (selection geometry
/// spans size boundaries without dropping lines).
#[test]
fn select_all_spans_the_boundary() {
    let mut h = edit_harness(SAMPLE);
    h.run_ok();
    let edit = h.get_by_role(egui::accesskit::Role::MultilineTextInput);
    edit.click();
    h.run_ok();
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
    h.run_ok();
    // After select-all, typing replaces everything: state proves selection covered all.
    h.get_by_role(egui::accesskit::Role::MultilineTextInput)
        .type_text("x");
    h.run_ok();
    assert_eq!(h.state(), "x");
}
