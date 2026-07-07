//! Placeholder surface for surfaces not yet implemented (Drawer, Map, Inbox, Trash
//! until their real Task implementations land).

use eframe::egui;

/// Render a centered "Coming in a later milestone" label. ~15 lines by design
/// — this is intentionally minimal until the real surface arrives.
pub fn placeholder_ui(ui: &mut egui::Ui) {
    ui.centered_and_justified(|ui| {
        ui.label(
            egui::RichText::new("Coming in a later milestone")
                .weak()
                .size(18.0),
        );
    });
}
