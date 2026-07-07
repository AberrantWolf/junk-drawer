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

use eframe::egui;
use jd_core::id::NoteId;
use jd_core::note::{Kind, Status};
use jd_core::session::DeskId;

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
}

/// Render the 9 card-menu items inside the calling `ui` (which may be a
/// `Response::context_menu` closure or an anchored `Popup`).
///
/// Returns `Some(CardMenuEvent)` if an item was clicked, `None` otherwise.
/// The caller is responsible for closing the menu/popup after dispatching.
pub fn card_menu_items(ui: &mut egui::Ui, ctx: &CardMenuCtx<'_>) -> Option<CardMenuEvent> {
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
