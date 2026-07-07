mod common;

use eframe::egui;
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use std::sync::{Arc, Mutex};

const SAMPLE: &str = "# The heading line\nbody text under it\nmore body";

/// Minimal eframe App wrapper that runs a UI closure each frame with mutable String state.
struct ClosureApp<F: FnMut(&mut egui::Ui, &mut String) + 'static> {
    state: String,
    run: F,
}

impl<F: FnMut(&mut egui::Ui, &mut String) + 'static> eframe::App for ClosureApp<F> {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        (self.run)(ui, &mut self.state);
    }
}

/// Create a harness with bundled fonts pre-installed via `CreationContext`.
/// Using `build_eframe` is the only way to inject fonts before the initial sizing pass.
fn with_fonts_harness<F>(initial: &str, run: F) -> Harness<'static, ClosureApp<F>>
where
    F: FnMut(&mut egui::Ui, &mut String) + 'static,
{
    let initial = initial.to_owned();
    Harness::builder().build_eframe(move |cc| {
        jd_app::theme::install_fonts(&cc.egui_ctx);
        ClosureApp {
            state: initial,
            run,
        }
    })
}

fn edit_harness(
    initial: &str,
) -> Harness<'static, ClosureApp<impl FnMut(&mut egui::Ui, &mut String) + 'static>> {
    let mut cache = jd_app::editor::LineCache::default();
    with_fonts_harness(initial, move |ui, text| {
        let mut layouter = |ui: &egui::Ui, buf: &dyn egui::TextBuffer, wrap: f32| {
            jd_app::editor::layout_body(
                ui,
                buf.as_str(),
                wrap,
                &mut cache,
                &|_| false,
                &jd_app::theme::Theme::light(),
            )
        };
        ui.add(
            egui::TextEdit::multiline(text)
                .desired_width(400.0)
                .layouter(&mut layouter),
        );
    })
}

/// Exit criterion 1: the galley really is mixed-size (heading row taller than body row).
/// Uses a kittest Harness to get a real Ui context.
#[test]
fn heading_row_is_taller_than_body_row() {
    let sizes: Arc<Mutex<Option<(f32, f32, usize)>>> = Arc::new(Mutex::new(None));
    let sizes_clone = sizes.clone();

    let mut harness = with_fonts_harness("", move |ui, _| {
        let mut cache = jd_app::editor::LineCache::default();
        let galley = jd_app::editor::layout_body(
            ui,
            SAMPLE,
            400.0,
            &mut cache,
            &|_| false,
            &jd_app::theme::Theme::light(),
        );
        if galley.rows.len() >= 2 {
            *sizes_clone.lock().unwrap() = Some((
                galley.rows[0].rect().height(),
                galley.rows[1].rect().height(),
                galley.rows.len(),
            ));
        }
    });
    harness.run_ok();

    let guard = sizes.lock().unwrap();
    let (h0, h1, rows) = guard.expect("galley rows not captured");
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
            .state
            .starts_with("# The heading line!\nbody text under it"),
        "insert landed at end of the heading line, got: {}",
        h.state().state
    );
    // And across the boundary: ArrowDown+End then type — lands at end of line 2.
    h.key_press(egui::Key::ArrowDown);
    h.key_press(egui::Key::End);
    h.run_ok();
    h.get_by_role(egui::accesskit::Role::MultilineTextInput)
        .type_text("?");
    h.run_ok();
    assert!(
        h.state().state.contains("body text under it?"),
        "got: {}",
        h.state().state
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
    assert_eq!(h.state().state, "x");
}
