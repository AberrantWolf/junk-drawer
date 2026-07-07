//! HeadingMarker level fix (Task 2 review finding): "## Sub" must render its
//! "## " marker at heading_size(2), not heading_size(1) — the marker must
//! never be BIGGER than its own heading text.
//!
//! This drives the REAL `layout_body` and inspects the sections of the
//! LayoutJob embedded in the returned galley, so it fails on the old
//! behavior (marker hardcoded to heading_size(1) = 24.0).

use eframe::egui;
use egui_kittest::Harness;
use std::sync::{Arc, Mutex};

struct App {
    captured: Arc<Mutex<Option<Vec<f32>>>>,
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let mut cache = jd_app::editor::LineCache::default();
        let galley = jd_app::editor::layout_body(
            ui,
            "## Sub",
            400.0,
            &mut cache,
            &|_| false,
            &jd_app::theme::Theme::light(),
            false,
        );
        let sizes: Vec<f32> = galley
            .job
            .sections
            .iter()
            .map(|s| s.format.font_id.size)
            .collect();
        *self.captured.lock().unwrap() = Some(sizes);
    }
}

#[test]
fn heading_marker_size_matches_heading_text_size() {
    use jd_app::editor::heading_size;

    let captured: Arc<Mutex<Option<Vec<f32>>>> = Arc::new(Mutex::new(None));
    let cap = captured.clone();
    let mut harness = Harness::builder().build_eframe(move |cc| {
        jd_app::theme::install_fonts(&cc.egui_ctx);
        App { captured: cap }
    });
    harness.run_ok();

    let sizes = captured.lock().unwrap().clone().expect("app ran");
    // "## Sub" lexes to: HeadingMarker("## "), Heading(2)("Sub").
    assert_eq!(
        sizes.len(),
        2,
        "expected 2 sections for '## Sub', got {sizes:?}"
    );
    let (marker_size, text_size) = (sizes[0], sizes[1]);

    assert_eq!(
        text_size,
        heading_size(2),
        "Heading(2) text must be heading_size(2)"
    );
    assert_eq!(
        marker_size,
        text_size,
        "HeadingMarker for '##' must match its heading text size (old bug: marker at heading_size(1)={})",
        heading_size(1)
    );
    // Sanity: this test can only catch the bug if the two sizes differ.
    assert_ne!(heading_size(1), heading_size(2));
}
