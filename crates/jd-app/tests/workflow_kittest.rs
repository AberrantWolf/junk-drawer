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

    // Inverse must restore card to source desk at old position.
    let entry = h.state_mut().state.journal.pop_undo().unwrap();
    assert!(
        matches!(
            &entry.inverse,
            InverseAction::Session(SessionOp::Place { desk, id, pos })
            if *desk == source_desk_id && *id == note_id && *pos == old_pos
        ),
        "inverse must be Place back on source desk at old position, got {:?}",
        entry.inverse
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
