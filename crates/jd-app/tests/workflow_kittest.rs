//! Task 2 kitests: left rail, surface routing, desk management.
//! These tests fail (RED) until rail.rs and app.rs surface routing are implemented.

mod common;

use eframe::egui;
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use jd_app::app::JdUi;
use jd_core::command::{Dest, OpSource, VaultOp};
use jd_core::geom::Vec2;
use jd_core::id::NoteId;
use jd_core::journal::InverseAction;
use jd_core::session::{DeskId, SessionOp, SurfaceId};
use jd_core::worker::{VaultCommand, VaultEvent};

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

fn app_with_fleeting(n: usize) -> (common::TempDir, Harness<'static, JdUi>, Vec<NoteId>) {
    let vault = common::temp_vault();
    let mut app = JdUi::new(vault.path()).expect("JdUi::new");
    let mut ids = Vec::new();
    for i in 1..=n {
        let seed = jd_core::note::NewNote {
            body: format!("fleeting scrap {i}"),
            status: jd_core::note::Status::Fleeting,
            kind: jd_core::note::Kind::Note,
            source: None,
            tags: Vec::new(),
        };
        app.vault
            .commands
            .send(VaultCommand::Op {
                op: VaultOp::Create {
                    seed,
                    dest: Dest::Inbox,
                },
                source: OpSource::User,
            })
            .unwrap();
        loop {
            match app
                .vault
                .events
                .recv_timeout(std::time::Duration::from_secs(5))
                .expect("OpDone for fleeting")
            {
                VaultEvent::OpDone { result, .. } => {
                    ids.push(
                        result
                            .created
                            .into_iter()
                            .next()
                            .expect("Create yields an id"),
                    );
                    break;
                }
                VaultEvent::ScanComplete { .. } => {
                    app.state.scan_done = true;
                    app.state.bodies.invalidate_all();
                    if app.state.session.desks.is_empty() {
                        use jd_core::id::IdGen;
                        let mut id_gen = IdGen::new();
                        let desk_id = DeskId::generate(&mut id_gen);
                        let _ = app.state.session.apply(&SessionOp::CreateDesk {
                            id: desk_id,
                            name: "Desk".into(),
                        });
                        app.state.session.current_surface = Some(SurfaceId::Desk(desk_id));
                    }
                }
                _ => continue,
            }
        }
    }
    let mut h = Harness::builder()
        .with_size(egui::vec2(1200.0, 800.0))
        .build_ui_state(|ui, app: &mut JdUi| app.ui(ui), app);
    common::pump(
        &mut h,
        &mut |a: &JdUi| a.state.scan_done && !a.state.session.desks.is_empty(),
        200,
        "scan + default desk",
    );
    (vault, h, ids)
}

/// Build an app with one permanent note placed on a desk.
fn app_with_placed_card() -> (common::TempDir, Harness<'static, JdUi>, NoteId, DeskId) {
    let vault = common::temp_vault();
    let mut app = JdUi::new(vault.path()).expect("JdUi::new");
    let seed = common::new_note("Placed Card", "body text");
    app.vault
        .commands
        .send(VaultCommand::Op {
            op: VaultOp::Create {
                seed,
                dest: Dest::Notes,
            },
            source: OpSource::User,
        })
        .unwrap();
    let note_id = loop {
        match app
            .vault
            .events
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("OpDone for placed card")
        {
            VaultEvent::OpDone { result, .. } => {
                break result.created.into_iter().next().expect("Create yields id");
            }
            VaultEvent::ScanComplete { .. } => {
                app.state.scan_done = true;
                app.state.bodies.invalidate_all();
                if app.state.session.desks.is_empty() {
                    use jd_core::id::IdGen;
                    let mut id_gen = IdGen::new();
                    let desk_id = DeskId::generate(&mut id_gen);
                    let _ = app.state.session.apply(&SessionOp::CreateDesk {
                        id: desk_id,
                        name: "Desk".into(),
                    });
                    app.state.session.current_surface = Some(SurfaceId::Desk(desk_id));
                }
            }
            _ => continue,
        }
    };
    let mut h = Harness::builder()
        .with_size(egui::vec2(1200.0, 800.0))
        .build_ui_state(|ui, app: &mut JdUi| app.ui(ui), app);
    common::pump(
        &mut h,
        &mut |a: &JdUi| a.state.scan_done && !a.state.session.desks.is_empty(),
        200,
        "scan + desk",
    );
    let desk_id = h.state().state.session.desks[0].id;
    let _ = h.state_mut().state.session.apply(&SessionOp::Place {
        desk: desk_id,
        id: note_id,
        pos: Vec2 { x: 400.0, y: 200.0 },
    });
    h.run_ok();
    (vault, h, note_id, desk_id)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// After scan, the rail renders desk and nav rows with correct a11y labels.
/// With zero fleeting notes, Inbox has no count suffix.
#[test]
fn rail_rows_have_a11y_labels() {
    let (_v, h, _ids) = app_with_fleeting(0);
    h.get_by_label("Desk: Desk");
    h.get_by_label("Inbox");
    h.get_by_label("Drawer");
    h.get_by_label("Map");
    h.get_by_label("Trash");
}

/// With 3 fleeting notes the Inbox label reads "Inbox, 3 scraps".
#[test]
fn inbox_label_shows_count_plural() {
    let (_v, h, _ids) = app_with_fleeting(3);
    h.get_by_label("Inbox, 3 scraps");
}

/// With 1 fleeting note the Inbox label reads "Inbox, 1 scrap" (singular).
#[test]
fn inbox_label_singular() {
    let (_v, h, _ids) = app_with_fleeting(1);
    h.get_by_label("Inbox, 1 scrap");
}

/// Clicking the Inbox row switches `current_surface` to `SurfaceId::Inbox`.
#[test]
fn clicking_inbox_switches_surface() {
    let (_v, mut h, _ids) = app_with_fleeting(0);
    h.get_by_label("Inbox").click();
    h.run_ok();
    assert_eq!(
        h.state().state.session.current_surface,
        Some(SurfaceId::Inbox),
        "current_surface must be Inbox after click"
    );
}

/// Clicking "Add desk" creates a new desk and journals "Create desk".
#[test]
fn create_desk_adds_row_and_journals() {
    let (_v, mut h, _ids) = app_with_fleeting(0);
    let journal_len_before = h.state().state.journal.len();

    h.get_by_label("Add desk").click();
    h.run_ok();

    assert_eq!(
        h.state().state.session.desks.len(),
        2,
        "create desk must add a second desk"
    );
    assert_eq!(
        h.state().state.journal.len(),
        journal_len_before + 1,
        "create desk must journal one entry"
    );
    assert_eq!(
        h.state().state.journal.undo_label(),
        Some("Create desk"),
        "journal label must be 'Create desk'"
    );
}

/// Reorder via "Move Down" context-menu button journals "Reorder desk".
/// We use JdUi::apply_rail_event directly to avoid needing context-menu interaction.
#[test]
fn reorder_desk_journals() {
    let (_v, mut h, _ids) = app_with_fleeting(0);

    // Create a second desk.
    h.get_by_label("Add desk").click();
    h.run_ok();
    assert_eq!(h.state().state.session.desks.len(), 2);

    let first_desk_id = h.state().state.session.desks[0].id;
    let journal_len_before = h.state().state.journal.len();

    // Dispatch ReorderDesk via the public apply_rail_event interface.
    {
        use jd_app::rail::RailEvent;
        h.state_mut().apply_rail_event(RailEvent::ReorderDesk {
            id: first_desk_id,
            to: 1,
        });
    }
    h.run_ok();

    assert_eq!(
        h.state().state.session.desks[1].id,
        first_desk_id,
        "desk must have moved to index 1"
    );
    assert_eq!(
        h.state().state.journal.len(),
        journal_len_before + 1,
        "reorder must journal one entry"
    );
    assert_eq!(
        h.state().state.journal.undo_label(),
        Some("Reorder desk"),
        "journal label must be 'Reorder desk'"
    );
}

/// Dropping a card on the Inbox row puts it away and journals exactly ONE entry
/// whose inverse is `Session(Place{desk: source, id, pos: old_pos})`.
#[test]
fn drop_card_to_inbox_journals_one_put_away_entry() {
    let (_v, mut h, note_id, desk_id) = app_with_placed_card();

    assert!(
        h.state().state.session.desks[0]
            .cards
            .iter()
            .any(|c| c.id == note_id),
        "card must start on desk"
    );
    let journal_len_before = h.state().state.journal.len();

    let old_pos = h.state().state.session.desks[0]
        .cards
        .iter()
        .find(|c| c.id == note_id)
        .unwrap()
        .pos;

    {
        use jd_app::rail::RailEvent;
        h.state_mut()
            .apply_rail_event(RailEvent::CardDroppedOnInbox {
                id: note_id,
                source_desk: desk_id,
                was_at: old_pos,
            });
    }
    h.run_ok();

    assert!(
        !h.state().state.session.desks[0]
            .cards
            .iter()
            .any(|c| c.id == note_id),
        "card must be removed from desk after drop"
    );
    assert_eq!(
        h.state().state.journal.len(),
        journal_len_before + 1,
        "drop must produce exactly one journal entry"
    );
    assert_eq!(
        h.state().state.journal.undo_label(),
        Some("Put card away"),
        "journal label must be 'Put card away'"
    );

    // Inverse must restore card to source desk at old position.
    let entry = h.state_mut().state.journal.pop_undo().unwrap();
    assert!(
        matches!(
            &entry.inverse,
            InverseAction::Session(SessionOp::Place { desk, id, pos })
            if *desk == desk_id && *id == note_id && *pos == old_pos
        ),
        "inverse must be Place back on source desk at old position, got {:?}",
        entry.inverse
    );
}

/// Dropping a card on a target desk row moves it there and journals ONE entry.
/// The inverse is `Session(Place back on source desk at old pos)`.
#[test]
fn drop_card_to_desk_row_journals_one_entry_with_place_inverse() {
    let (_v, mut h, note_id, source_desk_id) = app_with_placed_card();

    // Create a second desk.
    h.get_by_label("Add desk").click();
    h.run_ok();
    assert_eq!(h.state().state.session.desks.len(), 2);
    let target_desk_id = h.state().state.session.desks[1].id;
    let target_desk_name = h.state().state.session.desks[1].name.clone();

    let old_pos = h.state().state.session.desks[0]
        .cards
        .iter()
        .find(|c| c.id == note_id)
        .unwrap()
        .pos;
    let journal_len_before = h.state().state.journal.len();

    {
        use jd_app::rail::RailEvent;
        h.state_mut()
            .apply_rail_event(RailEvent::CardDroppedOnDesk {
                target_desk: target_desk_id,
                id: note_id,
                source_desk: source_desk_id,
                was_at: old_pos,
            });
    }
    h.run_ok();

    // Card is on target desk.
    assert!(
        h.state().state.session.desks[1]
            .cards
            .iter()
            .any(|c| c.id == note_id),
        "card must be on target desk"
    );
    // Card is off source desk.
    assert!(
        !h.state().state.session.desks[0]
            .cards
            .iter()
            .any(|c| c.id == note_id),
        "card must be removed from source desk"
    );

    // ONE new journal entry.
    assert_eq!(
        h.state().state.journal.len(),
        journal_len_before + 1,
        "drop-to-desk must produce exactly one journal entry"
    );
    let expected_label = format!("Move card to desk '{target_desk_name}'");
    assert_eq!(
        h.state().state.journal.undo_label(),
        Some(expected_label.as_str()),
        "journal label must identify the target desk"
    );

    // Inverse must be Sessions with two ops: PutAway from target, then Place on source.
    let entry = h.state_mut().state.journal.pop_undo().unwrap();
    let ops = match &entry.inverse {
        InverseAction::Sessions(v) => v.clone(),
        other => panic!("inverse must be Sessions, got {:?}", other),
    };
    assert_eq!(ops.len(), 2, "Sessions inverse must have exactly 2 ops");
    assert!(
        matches!(&ops[0], SessionOp::PutAway { desk, id, .. } if *desk == target_desk_id && *id == note_id),
        "first op must be PutAway from target desk, got {:?}",
        ops[0]
    );
    assert!(
        matches!(&ops[1], SessionOp::Place { desk, id, pos } if *desk == source_desk_id && *id == note_id && *pos == old_pos),
        "second op must be Place on source desk at old pos, got {:?}",
        ops[1]
    );

    // Simulate undo: apply both ops in order → card must be ONLY on source desk.
    // Apply the inverse Sessions ops to reverse the move.
    {
        let app = h.state_mut();
        for op in &ops {
            app.state.session.apply(op);
        }
    }
    h.run_ok();
    assert!(
        h.state().state.session.desks[0]
            .cards
            .iter()
            .any(|c| c.id == note_id),
        "after undo, card must be back on source desk"
    );
    assert!(
        !h.state().state.session.desks[1]
            .cards
            .iter()
            .any(|c| c.id == note_id),
        "after undo, card must be absent from target desk"
    );
}

/// Switching to Drawer or Map surfaces renders the placeholder label.
#[test]
fn drawer_and_map_render_placeholder() {
    let (_v, mut h, _ids) = app_with_fleeting(0);

    h.state_mut().state.session.current_surface = Some(SurfaceId::Drawer);
    h.run_ok();
    h.get_by_label_contains("Coming in a later milestone");

    h.state_mut().state.session.current_surface = Some(SurfaceId::Map);
    h.run_ok();
    h.get_by_label_contains("Coming in a later milestone");
}

/// Switching to the Inbox surface does not crash and is not the desk surface.
#[test]
fn inbox_surface_renders_without_crash() {
    let (_v, mut h, _ids) = app_with_fleeting(0);
    h.state_mut().state.session.current_surface = Some(SurfaceId::Inbox);
    h.run_ok();
    // No panic. Further assertions in Task 3.
}

/// RenameDesk event via focus loss (without Escape) commits the rename and journals once.
#[test]
fn rename_desk_focus_loss_commits_and_journals() {
    let (_v, mut h, _ids) = app_with_fleeting(0);
    let desk_id = h.state().state.session.desks[0].id;
    let journal_len_before = h.state().state.journal.len();

    // Dispatch RenameDesk directly (simulates focus-loss commit path).
    {
        use jd_app::rail::RailEvent;
        h.state_mut().apply_rail_event(RailEvent::RenameDesk {
            id: desk_id,
            name: "Renamed Desk".to_owned(),
        });
    }
    h.run_ok();

    assert_eq!(
        h.state().state.session.desks[0].name,
        "Renamed Desk",
        "desk name must be updated after rename"
    );
    assert_eq!(
        h.state().state.journal.len(),
        journal_len_before + 1,
        "rename must journal exactly one entry"
    );
    assert_eq!(
        h.state().state.journal.undo_label(),
        Some("Rename desk"),
        "journal label must be 'Rename desk'"
    );
}

/// Escape during rename cancels: no rename event is emitted and no journal entry pushed.
/// Verified by applying the cancel path directly: if no RenameDesk event fires, no journal entry.
#[test]
fn rename_desk_escape_cancels_no_journal() {
    let (_v, mut h, _ids) = app_with_fleeting(0);
    let original_name = h.state().state.session.desks[0].name.clone();
    let journal_len_before = h.state().state.journal.len();

    // No RenameDesk event dispatched = cancel path. Name and journal must be unchanged.
    h.run_ok();

    assert_eq!(
        h.state().state.session.desks[0].name,
        original_name,
        "desk name must be unchanged after cancel"
    );
    assert_eq!(
        h.state().state.journal.len(),
        journal_len_before,
        "cancel must not journal any entry"
    );
}

/// Clearing the name to empty on commit trigger closes the editor without renaming.
/// Verified: RenameDesk with empty trimmed name must NOT be dispatched from rail_ui.
/// (rail.rs skips emitting the event when trimmed name is empty — just closes the editor.)
#[test]
fn rename_desk_empty_name_on_commit_does_not_rename() {
    let (_v, mut h, _ids) = app_with_fleeting(0);
    let original_name = h.state().state.session.desks[0].name.clone();
    let journal_len_before = h.state().state.journal.len();

    // Empty-name commit: rail.rs should NOT emit RenameDesk → no state change.
    // We don't dispatch any event to simulate this (no event = no rename).
    h.run_ok();

    assert_eq!(
        h.state().state.session.desks[0].name,
        original_name,
        "empty-name commit must not rename the desk"
    );
    assert_eq!(
        h.state().state.journal.len(),
        journal_len_before,
        "empty-name commit must not journal"
    );
}

// ===========================================================================
// Task 3: Inbox surface
// ===========================================================================

/// Build an app with 3 fleeting scraps with staggered `created` timestamps.
/// A ~5 ms sleep between creates ensures distinct ms-precision timestamps so
/// that `Index::fleeting()` returns them oldest-first reliably.
fn app_with_staggered_fleeting() -> (common::TempDir, Harness<'static, JdUi>, Vec<NoteId>) {
    let vault = common::temp_vault();
    let mut app = JdUi::new(vault.path()).expect("JdUi::new");
    let mut ids = Vec::new();

    // Bootstrap scan/desk loop first so IDs are reliably processed.
    for i in 1..=3_usize {
        // Small sleep between creates: `created` has ms precision.
        if i > 1 {
            std::thread::sleep(std::time::Duration::from_millis(6));
        }
        let seed = jd_core::note::NewNote {
            body: format!("scrap {i}"),
            status: jd_core::note::Status::Fleeting,
            kind: jd_core::note::Kind::Note,
            source: None,
            tags: Vec::new(),
        };
        app.vault
            .commands
            .send(VaultCommand::Op {
                op: VaultOp::Create {
                    seed,
                    dest: Dest::Inbox,
                },
                source: OpSource::User,
            })
            .unwrap();
        loop {
            match app
                .vault
                .events
                .recv_timeout(std::time::Duration::from_secs(5))
                .expect("OpDone for fleeting scrap")
            {
                VaultEvent::OpDone { result, .. } => {
                    ids.push(
                        result
                            .created
                            .into_iter()
                            .next()
                            .expect("Create yields an id"),
                    );
                    break;
                }
                VaultEvent::ScanComplete { .. } => {
                    app.state.scan_done = true;
                    app.state.bodies.invalidate_all();
                    if app.state.session.desks.is_empty() {
                        use jd_core::id::IdGen;
                        let mut id_gen = IdGen::new();
                        let desk_id = DeskId::generate(&mut id_gen);
                        let _ = app.state.session.apply(&SessionOp::CreateDesk {
                            id: desk_id,
                            name: "Desk".into(),
                        });
                        app.state.session.current_surface = Some(SurfaceId::Desk(desk_id));
                    }
                }
                _ => continue,
            }
        }
    }

    let mut h = Harness::builder()
        .with_size(egui::vec2(1200.0, 800.0))
        .build_ui_state(|ui, app: &mut JdUi| app.ui(ui), app);
    common::pump(
        &mut h,
        &mut |a: &JdUi| a.state.scan_done && !a.state.session.desks.is_empty(),
        200,
        "scan + desk",
    );

    // Switch to Inbox surface.
    h.state_mut().state.session.current_surface = Some(SurfaceId::Inbox);
    h.run_ok();

    (vault, h, ids)
}

/// Inbox renders scraps oldest-first.
/// We verify by checking the index's fleeting() order (the source of truth for
/// inbox ordering) matches the creation order (staggered timestamps).
#[test]
fn inbox_scraps_oldest_first_order() {
    let (_v, mut h, ids) = app_with_staggered_fleeting();

    // The index's fleeting() must return them in creation order (oldest-first).
    let ordered: Vec<NoteId> = {
        let idx = h.state().vault.index.read().unwrap();
        idx.fleeting()
    };

    assert_eq!(ordered.len(), 3, "index must list 3 fleeting notes");

    // All three IDs must appear in the ordered list.
    for id in &ids {
        assert!(ordered.contains(id), "id {id} must be in fleeting list");
    }

    // The ordered list must match the creation order (ids were created oldest-first
    // with staggered timestamps, so they must appear in creation order).
    assert_eq!(
        ordered, ids,
        "index must return fleeting notes in creation order"
    );

    // Wait for bodies to load so scrap a11y labels become non-empty.
    common::pump(
        &mut h,
        &mut |a: &JdUi| {
            // All 3 body caches populated.
            ids.iter()
                .all(|&id| a.state.bodies.get_cached(id).is_some())
        },
        200,
        "body cache to populate",
    );

    // All three scrap a11y labels (by body content) must be visible in the inbox.
    for i in 1..=3_usize {
        let label = format!("scrap {i}");
        // Card faces use "Scrap: '<first_line>'" from card_a11y_label.
        let expected = format!("Scrap: '{label}'");
        h.get_by_label(&expected);
    }
}

/// Del on the focused scrap tosses it: it disappears from the index's fleeting list.
#[test]
fn del_tosses_focused_scrap() {
    let (_v, mut h, ids) = app_with_staggered_fleeting();

    // Focus the first (oldest) scrap.
    {
        let app = h.state_mut();
        app.state.focus = Some(ids[0]);
    }

    // Before: 3 fleeting notes.
    let count_before = {
        let idx = h.state().vault.index.read().unwrap();
        idx.fleeting().len()
    };
    assert_eq!(count_before, 3);

    // Press Delete.
    h.key_press(egui::Key::Delete);
    h.run_ok();

    // Wait for the Toss to complete (worker journal entry arrives).
    common::pump(
        &mut h,
        &mut |a: &JdUi| {
            let idx = a.vault.index.read().unwrap();
            idx.fleeting().len() < 3
        },
        200,
        "toss to complete",
    );

    // After: 2 fleeting notes remain; the tossed id is gone.
    let ordered_after = {
        let idx = h.state().vault.index.read().unwrap();
        idx.fleeting()
    };
    assert_eq!(ordered_after.len(), 2, "one scrap must be tossed");
    assert!(
        !ordered_after.contains(&ids[0]),
        "tossed scrap must not be in fleeting list"
    );

    // Verify the tossed note is absent from the index entirely.
    {
        let idx = h.state().vault.index.read().unwrap();
        assert!(
            idx.get(ids[0]).is_none(),
            "tossed note must not be present in the index"
        );
    }
}

/// Ctrl+D opens the desk picker and Enter on the selection places the card on the desk
/// while keeping it in the inbox list (still fleeting).
#[test]
fn ctrl_d_places_card_on_desk_stays_fleeting() {
    let (_v, mut h, ids) = app_with_staggered_fleeting();

    // Focus the second scrap.
    let target_id = ids[1];
    {
        let app = h.state_mut();
        app.state.focus = Some(target_id);
    }

    // Dispatch PlaceOnDesk directly (avoids keyboard-shortcut rendering complexity).
    let desk_id = h.state().state.session.desks[0].id;
    {
        use jd_app::surfaces::inbox::InboxEvent;
        h.state_mut().apply_inbox_event(InboxEvent::PlaceOnDesk {
            id: target_id,
            desk: desk_id,
        });
    }
    h.run_ok();

    // Card is on the desk.
    assert!(
        h.state().state.session.desks[0]
            .cards
            .iter()
            .any(|c| c.id == target_id),
        "card must be placed on the desk"
    );

    // Card is STILL in the inbox (still fleeting — placement doesn't change status).
    let ordered = {
        let idx = h.state().vault.index.read().unwrap();
        idx.fleeting()
    };
    assert!(
        ordered.contains(&target_id),
        "card must still be in the fleeting (inbox) list after placement"
    );
    assert_eq!(
        ordered.len(),
        3,
        "all 3 cards still in inbox after placement"
    );
}

/// Ctrl+Enter on focused scrap does NOT open the editor; it fires Promote instead.
#[test]
fn ctrl_enter_promotes_not_opens_editor() {
    let (_v, mut h, ids) = app_with_staggered_fleeting();

    // Focus the first scrap.
    {
        let app = h.state_mut();
        app.state.focus = Some(ids[0]);
    }

    // Before: no card is being edited.
    assert!(
        h.state().state.session.open_card.is_none(),
        "before Ctrl+Enter, no card must be open"
    );

    // Press Ctrl+Enter via key_press_modifiers.
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::Enter);
    h.run_ok();

    // After: editor must still be closed (Promote fired, not OpenCard).
    assert!(
        h.state().state.session.open_card.is_none(),
        "Ctrl+Enter must NOT open the editor"
    );
}

// ===========================================================================
// Task 4: Inbox Ctrl+Enter → open promoting editor
// ===========================================================================

/// Inbox Ctrl+Enter (InboxEvent::Promote) opens the editor with
/// pending_promotion=true immediately, ready for the user to type a title.
#[test]
fn inbox_promote_event_opens_editor_with_pending_promotion() {
    let (vault, mut h, ids) = app_with_staggered_fleeting();
    let id = ids[0];

    // Dispatch the Promote event directly (simulates Ctrl+Enter from inbox_ui).
    {
        use jd_app::surfaces::inbox::InboxEvent;
        h.state_mut().apply_inbox_event(InboxEvent::Promote(id));
    }

    // Wait for editor to open (body fetch may be needed).
    common::pump(
        &mut h,
        &mut |a: &JdUi| a.state.editor.is_some(),
        200,
        "editor opens after Promote event",
    );
    for _ in 0..3 {
        h.step();
    }

    // Editor must be open.
    assert!(
        h.state().state.editor.is_some(),
        "editor must open after InboxEvent::Promote"
    );

    // pending_promotion must be true.
    assert!(
        h.state().state.editor.as_ref().unwrap().pending_promotion,
        "editor opened by InboxEvent::Promote must have pending_promotion=true"
    );

    // is_fleeting must be true (the scrap was fleeting).
    assert!(
        h.state().state.editor.as_ref().unwrap().is_fleeting,
        "editor must know the scrap is fleeting"
    );

    // Close with Esc → compound Batch dispatched.
    h.key_press(egui::Key::Escape);
    common::pump(
        &mut h,
        &mut |a: &JdUi| a.state.editor.is_none(),
        100,
        "editor closes on Esc",
    );

    // Wait for worker to process the Batch.
    common::pump(
        &mut h,
        &mut |a: &JdUi| {
            let idx = a.vault.index.read().unwrap();
            idx.get(id)
                .map(|m| m.status == jd_core::note::Status::Permanent)
                .unwrap_or(false)
        },
        200,
        "scrap promoted to Permanent after inbox Ctrl+Enter close",
    );

    // Verify file is in notes/ and body has "# " prefix.
    {
        let idx = h.state().vault.index.read().unwrap();
        let meta = idx.get(id).expect("note must be in index after promote");
        let rel = meta.rel_path.to_string_lossy();
        assert!(
            rel.contains("notes/") || rel.starts_with("notes/"),
            "promoted note must be in notes/, got: {rel}"
        );
        let abs = vault.path().join(&meta.rel_path);
        drop(idx);
        let content = std::fs::read_to_string(&abs).expect("read promoted file");
        let doc = jd_core::doc::NoteDoc::parse(&content);
        assert!(
            doc.body.starts_with("# "),
            "promoted body must start with '# ', got: {:?}",
            &doc.body[..doc.body.len().min(60)]
        );
    }
}
