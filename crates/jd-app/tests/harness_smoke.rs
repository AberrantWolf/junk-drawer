mod common;

use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use jd_app::app::JdUi;

#[test]
fn harness_boots_app_and_finds_status_label() {
    let vault = common::temp_vault();
    let app = JdUi::new(vault.path()).expect("JdUi::new");
    let mut harness = Harness::builder().build_ui_state(|ui, app: &mut JdUi| app.ui(ui), app);
    // Step a few frames to let the initial scan land (warmup, no condition).
    for _ in 0..3 {
        harness.step();
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    harness.run_ok();
    harness.get_by_label_contains("Junk Drawer");
}

#[test]
fn snapshot_pipeline_works() {
    // Deliberately trivial: proves wgpu software rendering + dify diffing on CI.
    let mut harness = Harness::new_ui(|ui| {
        ui.label("snapshot pipeline probe");
    });
    harness.run_ok();
    harness.snapshot("pipeline_probe");
}
