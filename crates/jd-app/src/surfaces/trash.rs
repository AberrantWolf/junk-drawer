//! Trash surface: lists trashed notes with title + trashed-when, and a Restore
//! button per row.
//!
//! # Metadata-only listing (sanctioned read approach)
//!
//! The trash listing reads `.junkdrawer/trash/*.meta` files — directory metadata
//! (filenames + mtimes) only, no note body content.  This is the same class of
//! FS read as `SessionState::load` (session.json) — a lightweight, on-UI-thread
//! read of machine-state in `.junkdrawer/`.  Trashed note bodies are NOT in the
//! live index (the index only covers `inbox/` and `notes/`), so the normal
//! worker ReadBody path cannot serve them.  Using `vault_ref` directly here
//! keeps the worker free of a new list-trash query command while staying within
//! the single-writer contract (reads never contend with the worker's writes
//! because: (a) trash operations are single-threaded in the worker, (b) the
//! worst case is a stale listing for one frame before the next Restore/Delete
//! completes — acceptable for a disposable-state surface).

use eframe::egui;
use jd_core::id::NoteId;
use jd_core::vault::Vault;
use jd_core::vault::trash::{TrashEntry, list_trash};

// ---------------------------------------------------------------------------
// TrashEvent
// ---------------------------------------------------------------------------

/// Events emitted by `trash_ui` for `app.rs` to apply.
#[derive(Debug)]
pub enum TrashEvent {
    /// Restore a note from trash back to its original location.
    Restore(NoteId),
}

// ---------------------------------------------------------------------------
// TrashUiDeps
// ---------------------------------------------------------------------------

/// Everything `trash_ui` reads but does not own.
pub struct TrashUiDeps<'a> {
    /// Lightweight vault reference for listing trash (metadata-only, sanctioned).
    pub vault_ref: &'a Vault,
    pub theme: &'a crate::theme::Theme,
}

// ---------------------------------------------------------------------------
// trash_ui
// ---------------------------------------------------------------------------

/// Render the trash surface; returns events for app.rs to apply.
/// `trash_ui` never mutates vault state directly — it only emits events.
pub fn trash_ui(ui: &mut egui::Ui, deps: &mut TrashUiDeps<'_>) -> Vec<TrashEvent> {
    let mut events: Vec<TrashEvent> = Vec::new();

    // Read trash listing (metadata-only, see module-level comment).
    let entries: Vec<TrashEntry> = list_trash(deps.vault_ref);

    // Retention notice at the top.
    ui.label(
        egui::RichText::new("Items in Trash are kept 30 days")
            .weak()
            .size(13.0),
    );
    ui.add_space(8.0);

    if entries.is_empty() {
        ui.centered_and_justified(|ui| {
            ui.label(egui::RichText::new("Trash is empty").weak().size(16.0));
        });
        return events;
    }

    egui::ScrollArea::vertical().show(ui, |ui| {
        for entry in &entries {
            let title = entry.title_or_first_line.as_str();
            let when = entry.deleted.to_rfc3339();

            // Row: a11y label "Trashed: '<title>'" per the brief spec.
            let row_resp = ui.horizontal(|ui| {
                // Accessibility label for the whole row.
                ui.push_id(entry.id, |ui| {
                    let row_label = format!("Trashed: '{title}'");
                    // Title text.
                    ui.label(egui::RichText::new(title).size(14.0))
                        .on_hover_text(&row_label);
                    ui.add_space(8.0);
                    // Trashed-when (muted).
                    ui.label(egui::RichText::new(&when).weak().size(12.0));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let restore_btn = ui.button("Restore");
                        // Expose a11y label on the row container via the outer Ui.
                        restore_btn
                    })
                    .inner
                })
                .inner
            });

            // Emit a11y label on the row's response so kittest can query by label.
            let restore_clicked = row_resp.inner;
            let row_rect = row_resp.response.rect;
            let row_id = ui.id().with(("trash_row", entry.id));
            let row_interact = ui.interact(row_rect, row_id, egui::Sense::hover());
            row_interact.widget_info(|| {
                egui::WidgetInfo::labeled(
                    egui::WidgetType::Label,
                    true,
                    format!("Trashed: '{title}'").as_str(),
                )
            });

            if restore_clicked.clicked() {
                events.push(TrashEvent::Restore(entry.id));
            }

            ui.separator();
        }
    });

    events
}
