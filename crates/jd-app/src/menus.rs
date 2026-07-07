//! Shared card context-menu item list (used by Task 7 context menu and Task 8 Edit menu).
//!
//! `card_menu_items` renders all 9 items in spec order with sensible enablement.
//! Returns `Option<CardMenuEvent>` — at most one action per frame.
//!
//! Item enablement rules:
//!   Promote       — only when `Status::Fleeting` (scraps can be promoted)
//!   Toss          — always enabled
//!   Take to Desk  — always enabled (sub-menu of desks)
//!   Put Away      — only when the card is currently on a desk (not in inbox)
//!   Set Source…   — always enabled
//!   Make Divider  — only when `Kind != Structure` (already a divider → disabled)
//!   Demote to Scrap — only when `Status::Permanent`
//!   Copy Link     — only when `Status::Permanent` AND `title` is non-empty
//!                   (scraps have no canonical title to link)
//!   Reveal in File Manager — always enabled
//!
//! `edit_menu_bar` renders the egui menu bar (top panel) with Edit only:
//!   Undo <label> / Redo <label> — live labels, disabled when None.
//!   Cut / Copy / Paste — disabled (no programmatic egui 0.35 TextEdit path).
//!   Split Card — enabled only when editor is open.
//!   Find — disabled (arrives with the Drawer, WP4).

use eframe::egui;
use jd_core::id::NoteId;
use jd_core::journal::Journal;
use jd_core::note::{Kind, Status};
use jd_core::session::DeskId;

// ---------------------------------------------------------------------------
// Edit menu bar action
// ---------------------------------------------------------------------------

/// Actions that can be fired from the Edit menu bar.
#[derive(Debug, Clone, PartialEq)]
pub enum EditMenuAction {
    Undo,
    Redo,
    SplitCard,
}

/// Context needed to render the Edit menu bar correctly.
pub struct EditMenuCtx<'a> {
    pub journal: &'a Journal,
    /// True when the card editor overlay is currently open.
    pub editor_open: bool,
}

/// Render the egui top-panel menu bar with an "Edit" menu.
/// Returns `Some(EditMenuAction)` if an item was clicked, `None` otherwise.
///
/// Cut/Copy/Paste are rendered disabled with shortcut-hint tooltips — egui 0.35
/// has no programmatic way to forward clipboard operations to the focused TextEdit.
/// The native shortcuts (Cmd+X/C/V) still work inside the TextEdit widget itself.
///
/// Find is disabled with tooltip "Ctrl+K — arrives with the Drawer" (WP4).
pub fn edit_menu_bar(ui: &mut egui::Ui, ctx: &EditMenuCtx<'_>) -> Option<EditMenuAction> {
    let mut action: Option<EditMenuAction> = None;

    egui::MenuBar::new().ui(ui, |ui| {
        ui.menu_button("Edit", |ui| {
            // ── Undo ──────────────────────────────────────────────────────────
            let undo_label = ctx.journal.undo_label();
            let undo_text = match undo_label {
                Some(l) => format!("Undo {l}"),
                None => "Undo".to_owned(),
            };
            let can_undo = undo_label.is_some();
            ui.add_enabled_ui(can_undo, |ui| {
                let btn = ui.button(&undo_text);
                if btn.clicked() {
                    action = Some(EditMenuAction::Undo);
                    ui.close();
                }
            });

            // ── Redo ──────────────────────────────────────────────────────────
            let redo_label = ctx.journal.redo_label();
            let redo_text = match redo_label {
                Some(l) => format!("Redo {l}"),
                None => "Redo".to_owned(),
            };
            let can_redo = redo_label.is_some();
            ui.add_enabled_ui(can_redo, |ui| {
                let btn = ui.button(&redo_text);
                if btn.clicked() {
                    action = Some(EditMenuAction::Redo);
                    ui.close();
                }
            });

            ui.separator();

            // ── Cut / Copy / Paste (disabled) ─────────────────────────────────
            // egui 0.35 provides no programmatic path to forward clipboard ops to
            // the focused TextEdit widget.  The native shortcuts (Cmd+X/C/V) work
            // inside the TextEdit directly; these menu items serve as discoverable
            // placeholders with shortcut hints.
            ui.add_enabled_ui(false, |ui| {
                ui.button("Cut")
                    .on_disabled_hover_text("Use Cmd+X in the editor");
                ui.button("Copy")
                    .on_disabled_hover_text("Use Cmd+C in the editor");
                ui.button("Paste")
                    .on_disabled_hover_text("Use Cmd+V in the editor");
            });

            ui.separator();

            // ── Split Card ────────────────────────────────────────────────────
            let can_split = ctx.editor_open;
            ui.add_enabled_ui(can_split, |ui| {
                let btn = ui.button("Split Card");
                if btn.clicked() {
                    action = Some(EditMenuAction::SplitCard);
                    ui.close();
                }
            });

            // ── Find (disabled) ───────────────────────────────────────────────
            ui.add_enabled_ui(false, |ui| {
                ui.button("Find")
                    .on_disabled_hover_text("Ctrl+K — arrives with the Drawer");
            });
        });
    });

    action
}

// ---------------------------------------------------------------------------
// CardMenuEvent
// ---------------------------------------------------------------------------

/// Actions that can be fired from the card context menu.
/// `app.rs` maps these to vault / session ops.
#[derive(Debug, Clone)]
pub enum CardMenuEvent {
    Promote(NoteId),
    Toss(NoteId),
    TakeToDesk { id: NoteId, desk: DeskId },
    PutAway(NoteId),
    SetSource(NoteId),
    MakeDivider(NoteId),
    Demote(NoteId),
    CopyLink(NoteId),
    RevealInFileManager(NoteId),
}

// ---------------------------------------------------------------------------
// card_menu_items
// ---------------------------------------------------------------------------

/// Struct that bundles the per-card state needed to draw menu items with correct
/// enablement.  Callers build this once per card from FaceMeta.
pub struct CardMenuCtx<'a> {
    pub id: NoteId,
    pub status: Status,
    pub kind: Kind,
    /// Non-empty title — used to gate Copy Link.
    pub title: &'a str,
    /// Desks for the "Take to Desk ▸" submenu.
    pub desks: &'a [(DeskId, &'a str)],
    /// True when this card is currently placed on any desk (Put Away enabled).
    pub on_desk: bool,
    /// True while the card editor overlay is open.  When set, the menu must
    /// not be interactive — return None immediately so no events are emitted.
    pub editor_open: bool,
    /// True while a delete-confirm modal is pending.  Same guard as editor_open.
    pub confirm_pending: bool,
}

/// Render the 9 card-menu items inside the calling `ui` (which may be a
/// `Response::context_menu` closure or an anchored `Popup`).
///
/// Returns `Some(CardMenuEvent)` if an item was clicked, `None` otherwise.
/// The caller is responsible for closing the menu/popup after dispatching.
pub fn card_menu_items(ui: &mut egui::Ui, ctx: &CardMenuCtx<'_>) -> Option<CardMenuEvent> {
    // Modal stacking guard: if the editor or delete-confirm modal is open,
    // the context menu must not emit any actions.  This prevents a right-click
    // menu opened on the card before the modal appeared from being acted on
    // while the modal is in front.
    if ctx.editor_open || ctx.confirm_pending {
        ui.close();
        return None;
    }

    let mut event: Option<CardMenuEvent> = None;

    // ── Promote ─────────────────────────────────────────────────────────────
    let can_promote = ctx.status == Status::Fleeting;
    ui.add_enabled_ui(can_promote, |ui| {
        let btn = ui.button("Promote");
        if btn.clicked() {
            event = Some(CardMenuEvent::Promote(ctx.id));
        }
    });

    // ── Toss ────────────────────────────────────────────────────────────────
    if ui.button("Toss").clicked() {
        event = Some(CardMenuEvent::Toss(ctx.id));
    }

    // ── Take to Desk ▸ (submenu) ────────────────────────────────────────────
    ui.menu_button("Take to Desk ▸", |ui| {
        if ctx.desks.is_empty() {
            ui.add_enabled_ui(false, |ui| {
                let _ = ui.button("No desks available");
            });
        } else {
            for &(desk_id, desk_name) in ctx.desks {
                if ui.button(desk_name).clicked() {
                    event = Some(CardMenuEvent::TakeToDesk {
                        id: ctx.id,
                        desk: desk_id,
                    });
                    ui.close();
                }
            }
        }
    });

    // ── Put Away ────────────────────────────────────────────────────────────
    ui.add_enabled_ui(ctx.on_desk, |ui| {
        if ui.button("Put Away").clicked() {
            event = Some(CardMenuEvent::PutAway(ctx.id));
        }
    });

    // ── Set Source… ─────────────────────────────────────────────────────────
    if ui.button("Set Source…").clicked() {
        event = Some(CardMenuEvent::SetSource(ctx.id));
    }

    // ── Make Divider ────────────────────────────────────────────────────────
    let can_make_divider = ctx.kind != Kind::Structure;
    ui.add_enabled_ui(can_make_divider, |ui| {
        if ui.button("Make Divider").clicked() {
            event = Some(CardMenuEvent::MakeDivider(ctx.id));
        }
    });

    // ── Demote to Scrap ─────────────────────────────────────────────────────
    let can_demote = ctx.status == Status::Permanent;
    ui.add_enabled_ui(can_demote, |ui| {
        if ui.button("Demote to Scrap").clicked() {
            event = Some(CardMenuEvent::Demote(ctx.id));
        }
    });

    // ── Copy Link ───────────────────────────────────────────────────────────
    // Disabled for scraps (untitled notes have no canonical title to link).
    let can_copy_link = ctx.status == Status::Permanent && !ctx.title.is_empty();
    ui.add_enabled_ui(can_copy_link, |ui| {
        let btn = ui.button("Copy Link");
        let clicked = btn.clicked();
        if !can_copy_link && ctx.status == Status::Fleeting {
            btn.on_hover_text("Scraps have no title to link");
        }
        if clicked {
            event = Some(CardMenuEvent::CopyLink(ctx.id));
        }
    });

    // ── Reveal in File Manager ───────────────────────────────────────────────
    if ui.button("Reveal in File Manager").clicked() {
        event = Some(CardMenuEvent::RevealInFileManager(ctx.id));
    }

    event
}
