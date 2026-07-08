//! WP4 Task 1: Ctrl+K palette — overlay, three strata, rendering.
//!
//! Everything drives the real UI through egui_kittest/AccessKit, mirroring
//! the editor_kittest patterns (focused-node type_text, pump for worker events).

mod common;

use eframe::egui;
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use jd_app::app::JdUi;
use jd_app::palette::PaletteRow;
use jd_core::command::{Dest, OpSource, VaultOp};
use jd_core::geom::Vec2;
use jd_core::id::NoteId;
use jd_core::session::SessionOp;
use jd_core::worker::{VaultCommand, VaultEvent};

/// Create an app with the given titled notes (title, body) already in the
/// vault. Returns (vault_dir, harness, note ids in creation order).
fn app_with_notes(
    notes: &[(&str, &str)],
) -> (common::TempDir, Harness<'static, JdUi>, Vec<NoteId>) {
    // Default step_dt (0.25s/frame) — fine for everything except double-click
    // simulation (kittest runs one frame per queued event, so two click pairs
    // span 4 frames = 1s of egui time, past the 0.3s double-click window).
    app_with_notes_dt(notes, 0.25)
}

/// `app_with_notes` with an explicit kittest `step_dt` (seconds of egui time
/// per frame). Use a small dt when simulating double-clicks.
fn app_with_notes_dt(
    notes: &[(&str, &str)],
    step_dt: f32,
) -> (common::TempDir, Harness<'static, JdUi>, Vec<NoteId>) {
    let vault = common::temp_vault();
    let mut app = JdUi::new(vault.path()).expect("JdUi::new");

    let mut ids: Vec<NoteId> = Vec::new();
    for (title, body) in notes {
        app.vault
            .commands
            .send(VaultCommand::Op {
                op: VaultOp::Create {
                    seed: common::new_note(title, body),
                    dest: Dest::Notes,
                },
                source: OpSource::User,
            })
            .unwrap();
        // Wait for this note's OpDone before creating the next one, so ids
        // arrive in creation order.
        loop {
            match app
                .vault
                .events
                .recv_timeout(std::time::Duration::from_secs(5))
                .expect("OpDone")
            {
                VaultEvent::OpDone { result, .. } => {
                    ids.push(result.created.into_iter().next().expect("created id"));
                    break;
                }
                VaultEvent::ScanComplete { .. } => {
                    app.state.scan_done = true;
                    app.state.bodies.invalidate_all();
                    if app.state.session.desks.is_empty() {
                        use jd_core::id::IdGen;
                        use jd_core::session::{DeskId, SurfaceId};
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
        .with_step_dt(step_dt)
        .build_ui_state(|ui, app: &mut JdUi| app.ui(ui), app);

    common::pump(
        &mut h,
        &mut |a: &JdUi| a.state.scan_done && !a.state.session.desks.is_empty(),
        200,
        "scan + default desk",
    );

    (vault, h, ids)
}

/// Open the palette with Ctrl+K and let the input grab focus.
fn open_palette(h: &mut Harness<'_, JdUi>) {
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::K);
    common::pump(
        h,
        &mut |a: &JdUi| a.state.palette.is_some(),
        50,
        "palette opens on Ctrl+K",
    );
    // A few frames so the TextEdit renders and receives focus.
    for _ in 0..3 {
        h.step();
    }
}

/// Type into the palette's input (the focused-node type_text sequence from
/// the editor tests: focus + click, then type_text into the focused TextEdit).
fn type_in_palette(h: &mut Harness<'_, JdUi>, text: &str) {
    h.get_by_role(egui::accesskit::Role::TextInput).focus();
    h.get_by_role(egui::accesskit::Role::TextInput).click();
    h.run_ok();
    let node = h.get_by_role(egui::accesskit::Role::TextInput);
    node.type_text(text);
    h.step();
    h.run_ok();
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// Ctrl+K opens the palette; an empty query shows the query-syntax help
/// verbatim; a second Ctrl+K toggles it closed.
#[test]
fn ctrl_k_toggles_and_empty_query_shows_syntax_help() {
    let (_vault, mut h, _ids) = app_with_notes(&[("Alpha idea", "body")]);

    open_palette(&mut h);

    // Empty palette shows the syntax help, verbatim (queryable by label).
    h.get_by_label("plain words (AND) · \"quoted phrases\" · #tag · -word");

    // Second Ctrl+K closes.
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::K);
    common::pump(
        &mut h,
        &mut |a: &JdUi| a.state.palette.is_none(),
        50,
        "palette closes on second Ctrl+K",
    );
}

/// Esc closes ONLY the palette: no editor opens, no confirm modal appears,
/// and the app keeps running.
#[test]
fn esc_closes_palette_only() {
    let (_vault, mut h, _ids) = app_with_notes(&[("Alpha idea", "body")]);

    open_palette(&mut h);
    h.key_press(egui::Key::Escape);
    common::pump(
        &mut h,
        &mut |a: &JdUi| a.state.palette.is_none(),
        50,
        "palette closes on Esc",
    );
    assert!(h.state().state.editor.is_none(), "editor must stay closed");
    assert!(
        h.state().state.pending_confirm.is_none(),
        "no confirm modal must appear"
    );
    h.run_ok();
}

/// Strata order is pinned: a title match ranks above a body-only match, and
/// the "New scrap" row is always last. Rows carry their AccessKit labels.
#[test]
fn strata_order_title_body_newscrap() {
    let (_vault, mut h, ids) = app_with_notes(&[
        ("Alpha idea", "first body"),
        ("Beta note", "alpha appears in this body"),
    ]);
    let alpha_id = ids[0];
    let beta_id = ids[1];

    open_palette(&mut h);
    type_in_palette(&mut h, "alpha");

    // Pump a few frames so results recompute (and any body fetches land).
    common::pump(
        &mut h,
        &mut |a: &JdUi| {
            a.state
                .palette
                .as_ref()
                .is_some_and(|p| p.results.len() == 3)
        },
        200,
        "palette results: title + body + new-scrap",
    );

    let results = &h.state().state.palette.as_ref().unwrap().results;
    assert!(
        matches!(&results[0], PaletteRow::Title { id, .. } if *id == alpha_id),
        "row 0 must be the title match for 'Alpha idea', got {:?}",
        results[0]
    );
    assert!(
        matches!(&results[1], PaletteRow::Body { id, .. } if *id == beta_id),
        "row 1 must be the body match for 'Beta note', got {:?}",
        results[1]
    );
    assert!(
        matches!(&results[2], PaletteRow::NewScrap),
        "last row must be NewScrap, got {:?}",
        results[2]
    );

    // AccessKit labels on every row.
    h.get_by_label("Result: 'Alpha idea'");
    h.get_by_label("Result: 'Beta note'");
    h.get_by_label("New scrap: 'alpha'");
}

/// Up/Down move the selection; the selection is clamped to the row count.
#[test]
fn up_down_move_selection() {
    let (_vault, mut h, _ids) = app_with_notes(&[
        ("Alpha idea", "first body"),
        ("Beta note", "alpha appears in this body"),
    ]);

    open_palette(&mut h);
    type_in_palette(&mut h, "alpha");
    common::pump(
        &mut h,
        &mut |a: &JdUi| {
            a.state
                .palette
                .as_ref()
                .is_some_and(|p| p.results.len() == 3)
        },
        200,
        "palette results ready",
    );

    assert_eq!(h.state().state.palette.as_ref().unwrap().selected, 0);
    h.key_press(egui::Key::ArrowDown);
    h.run_ok();
    assert_eq!(h.state().state.palette.as_ref().unwrap().selected, 1);
    h.key_press(egui::Key::ArrowDown);
    h.run_ok();
    assert_eq!(h.state().state.palette.as_ref().unwrap().selected, 2);
    // Clamped at the last row.
    h.key_press(egui::Key::ArrowDown);
    h.run_ok();
    assert_eq!(h.state().state.palette.as_ref().unwrap().selected, 2);
    h.key_press(egui::Key::ArrowUp);
    h.run_ok();
    assert_eq!(h.state().state.palette.as_ref().unwrap().selected, 1);
}

// ---------------------------------------------------------------------------
// Task 2: palette actions — place / pan-to-existing / place-and-open / new scrap
// ---------------------------------------------------------------------------

/// Enter on a result row for a card NOT on the current desk places it at the
/// desk's viewport center, journals "Place card", and closes the palette.
#[test]
fn enter_places_card_on_current_desk_at_viewport_center() {
    let (_vault, mut h, ids) = app_with_notes(&[("Alpha idea", "first body")]);
    let note_id = ids[0];

    let center_before = h.state().state.session.desks[0].viewport.center;

    open_palette(&mut h);
    type_in_palette(&mut h, "alpha");
    common::pump(
        &mut h,
        &mut |a: &JdUi| {
            a.state
                .palette
                .as_ref()
                .is_some_and(|p| !p.results.is_empty())
        },
        200,
        "palette results ready",
    );

    h.key_press(egui::Key::Enter);
    common::pump(
        &mut h,
        &mut |a: &JdUi| a.state.palette.is_none(),
        50,
        "palette closes on Enter",
    );

    let desk = &h.state().state.session.desks[0];
    let placed = desk
        .cards
        .iter()
        .find(|c| c.id == note_id)
        .expect("card must be placed on the current desk");
    assert_eq!(placed.pos.x, center_before.x, "placed at viewport center x");
    assert_eq!(placed.pos.y, center_before.y, "placed at viewport center y");
    assert_eq!(
        h.state().state.journal.undo_label(),
        Some("Place card"),
        "placement must be journaled as 'Place card'"
    );
    // Plain Enter does not open the editor.
    assert!(h.state().state.editor.is_none(), "editor must stay closed");
    assert!(h.state().state.session.open_card.is_none());
}

/// THE SACRED RULE: Enter on a card already placed on the CURRENT desk pans
/// to center it — the card's session position is byte-identical after, and
/// NO journal entry is pushed. The highlight pulse is armed (reduced_motion
/// is off by default).
#[test]
fn enter_on_already_placed_card_pans_without_moving_or_journaling() {
    let (_vault, mut h, ids) = app_with_notes(&[("Alpha idea", "first body")]);
    let note_id = ids[0];

    // Place the card off-center directly (not journaled).
    let desk_id = h.state().state.session.desks[0].id;
    let pos = Vec2 { x: 400.0, y: 300.0 };
    let _ = h.state_mut().state.session.apply(&SessionOp::Place {
        desk: desk_id,
        id: note_id,
        pos,
    });
    h.run_ok();

    let journal_len_before = h.state().state.journal.len();
    let pos_before = h.state().state.session.desks[0]
        .cards
        .iter()
        .find(|c| c.id == note_id)
        .unwrap()
        .pos;

    open_palette(&mut h);
    type_in_palette(&mut h, "alpha");
    common::pump(
        &mut h,
        &mut |a: &JdUi| {
            a.state
                .palette
                .as_ref()
                .is_some_and(|p| !p.results.is_empty())
        },
        200,
        "palette results ready",
    );
    h.key_press(egui::Key::Enter);
    common::pump(
        &mut h,
        &mut |a: &JdUi| a.state.palette.is_none(),
        50,
        "palette closes on Enter",
    );

    let desk = &h.state().state.session.desks[0];
    let placed = desk.cards.iter().find(|c| c.id == note_id).unwrap();
    // Byte-identical position (exact float equality against pre-state).
    assert!(
        placed.pos.x.to_bits() == pos_before.x.to_bits()
            && placed.pos.y.to_bits() == pos_before.y.to_bits(),
        "SACRED: the palette must never move an already-placed card, got {:?}",
        placed.pos
    );
    // Viewport centered on the card (IndexCard 300x200 → center = pos + half).
    assert!(
        (desk.viewport.center.x - (400.0 + 150.0)).abs() < 0.5
            && (desk.viewport.center.y - (300.0 + 100.0)).abs() < 0.5,
        "viewport must center on the card, got {:?}",
        desk.viewport.center
    );
    // NO journal entry.
    assert_eq!(
        h.state().state.journal.len(),
        journal_len_before,
        "pan-to-existing must not journal"
    );
    // Highlight pulse armed (reduced_motion off by default).
    assert!(
        h.state()
            .state
            .highlight_pulse
            .is_some_and(|(id, _)| id == note_id),
        "highlight pulse must be armed at the card"
    );
}

/// Ctrl+Enter = place AND open the editor (session.open_card path).
#[test]
fn ctrl_enter_places_and_opens_editor() {
    let (_vault, mut h, ids) = app_with_notes(&[("Alpha idea", "first body")]);
    let note_id = ids[0];

    open_palette(&mut h);
    type_in_palette(&mut h, "alpha");
    common::pump(
        &mut h,
        &mut |a: &JdUi| {
            a.state
                .palette
                .as_ref()
                .is_some_and(|p| !p.results.is_empty())
        },
        200,
        "palette results ready",
    );
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::Enter);
    common::pump(
        &mut h,
        &mut |a: &JdUi| a.state.palette.is_none(),
        50,
        "palette closes on Ctrl+Enter",
    );

    // Placed on the desk…
    assert!(
        h.state().state.session.desks[0]
            .cards
            .iter()
            .any(|c| c.id == note_id),
        "card must be placed"
    );
    assert_eq!(h.state().state.journal.undo_label(), Some("Place card"));
    // …and the editor open path engaged.
    assert_eq!(
        h.state().state.session.open_card,
        Some(note_id),
        "Ctrl+Enter must set open_card"
    );
    common::pump(
        &mut h,
        &mut |a: &JdUi| a.state.editor.is_some(),
        200,
        "editor opens once the body arrives",
    );
}

/// Enter on the NewScrap row dispatches the Ctrl+N create path with the query
/// as the seed body: a fleeting note lands in inbox/ with body "buy milk",
/// it is placed at the viewport center, and the editor opens.
#[test]
fn new_scrap_creates_fleeting_note_with_query_body() {
    let (vault, mut h, _ids) = app_with_notes(&[("Alpha idea", "first body")]);

    open_palette(&mut h);
    type_in_palette(&mut h, "buy milk");
    common::pump(
        &mut h,
        &mut |a: &JdUi| {
            a.state
                .palette
                .as_ref()
                .is_some_and(|p| !p.results.is_empty())
        },
        200,
        "palette results ready (NewScrap row)",
    );
    // "buy milk" matches nothing → the only row is NewScrap (selected = 0).
    assert!(matches!(
        h.state().state.palette.as_ref().unwrap().results[..],
        [PaletteRow::NewScrap]
    ));

    h.key_press(egui::Key::Enter);
    common::pump(
        &mut h,
        &mut |a: &JdUi| a.state.palette.is_none(),
        50,
        "palette closes on Enter",
    );
    // The create round-trips through the worker; the editor opens on Body.
    common::pump(
        &mut h,
        &mut |a: &JdUi| a.state.editor.is_some(),
        400,
        "editor opens for the new scrap",
    );

    // A fleeting note landed in inbox/ with the query as its body.
    let inbox = vault.path().join("inbox");
    let mut found = false;
    for entry in std::fs::read_dir(&inbox).expect("inbox dir") {
        let p = entry.unwrap().path();
        if p.extension().is_some_and(|e| e == "md")
            && std::fs::read_to_string(&p)
                .unwrap_or_default()
                .contains("buy milk")
        {
            found = true;
        }
    }
    assert!(found, "a note with body 'buy milk' must exist in inbox/");

    // Placed on the first desk (pending_create path).
    let open_id = h.state().state.session.open_card.expect("open_card set");
    assert!(
        h.state().state.session.desks[0]
            .cards
            .iter()
            .any(|c| c.id == open_id),
        "the new scrap must be placed on the desk"
    );
}

/// Palette activated from a non-desk surface (Inbox): the card is placed on
/// the FIRST desk, the surface switches to it (not journaled; placement is),
/// and the status echo names the desk.
#[test]
fn palette_from_inbox_places_on_first_desk_switches_and_echoes() {
    use jd_core::session::SurfaceId;

    let (_vault, mut h, ids) = app_with_notes(&[("Alpha idea", "first body")]);
    let note_id = ids[0];
    let first_desk_id = h.state().state.session.desks[0].id;
    let desk_name = h.state().state.session.desks[0].name.clone();

    // Switch to the Inbox surface (navigation, direct set).
    h.state_mut().state.session.current_surface = Some(SurfaceId::Inbox);
    h.run_ok();

    open_palette(&mut h);
    type_in_palette(&mut h, "alpha");
    common::pump(
        &mut h,
        &mut |a: &JdUi| {
            a.state
                .palette
                .as_ref()
                .is_some_and(|p| !p.results.is_empty())
        },
        200,
        "palette results ready",
    );
    h.key_press(egui::Key::Enter);
    common::pump(
        &mut h,
        &mut |a: &JdUi| a.state.palette.is_none(),
        50,
        "palette closes on Enter",
    );

    // Placed on the first desk, journaled.
    assert!(
        h.state().state.session.desks[0]
            .cards
            .iter()
            .any(|c| c.id == note_id),
        "card must be placed on the first desk"
    );
    assert_eq!(h.state().state.journal.undo_label(), Some("Place card"));
    // Surface switched to that desk.
    assert_eq!(
        h.state().state.session.current_surface,
        Some(SurfaceId::Desk(first_desk_id)),
        "surface must switch to the first desk"
    );
    // Status echo names the desk.
    let echo = h
        .state()
        .state
        .status_echo
        .as_ref()
        .map(|(s, _)| s.clone())
        .unwrap_or_default();
    assert!(
        echo.contains(&desk_name),
        "status echo must name the desk, got '{echo}'"
    );
}

/// A double-click on a desk card BEHIND the open palette must not open the
/// editor. After the palette closes, the same double-click opens it (proves
/// the simulated double-click actually works).
#[test]
fn double_click_behind_open_palette_does_not_open_editor() {
    // Small step_dt: kittest runs one frame per queued event, and the two
    // click releases must land within egui's 0.3s double-click window.
    let (_vault, mut h, ids) = app_with_notes_dt(&[("Alpha idea", "first body")], 0.05);
    let note_id = ids[0];

    // Place the card away from the palette overlay (bottom-left region).
    let desk_id = h.state().state.session.desks[0].id;
    let _ = h.state_mut().state.session.apply(&SessionOp::Place {
        desk: desk_id,
        id: note_id,
        pos: Vec2 {
            x: -400.0,
            y: 150.0,
        },
    });
    h.run_ok();

    // Screen position of the card center (camera at origin, zoom 1; central
    // panel: rail 160px left, menu 24px top, status ~24px bottom).
    let desk = &h.state().state.session.desks[0];
    let placed = desk.cards.iter().find(|c| c.id == note_id).unwrap();
    let cam = jd_app::surfaces::desk::DeskCamera {
        center: egui::vec2(desk.viewport.center.x, desk.viewport.center.y),
        zoom: desk.viewport.zoom,
    };
    let panel = egui::Rect::from_min_max(egui::pos2(160.0, 24.0), egui::pos2(1200.0, 776.0));
    let screen_min = cam.to_screen(panel, egui::pos2(placed.pos.x, placed.pos.y));
    let click_pos = screen_min + egui::vec2(150.0, 100.0);

    let double_click = |h: &mut Harness<'_, JdUi>| {
        for _ in 0..2 {
            h.event(egui::Event::PointerButton {
                pos: click_pos,
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: egui::Modifiers::NONE,
            });
            h.event(egui::Event::PointerButton {
                pos: click_pos,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: egui::Modifiers::NONE,
            });
            h.step();
        }
        h.run_ok();
    };

    open_palette(&mut h);
    double_click(&mut h);
    assert!(
        h.state().state.editor.is_none(),
        "double-click behind the open palette must not open the editor"
    );
    assert!(
        h.state().state.session.open_card.is_none(),
        "open_card must not be set while the palette is open"
    );

    // Close the palette; the same double-click now opens the card.
    h.key_press(egui::Key::Escape);
    common::pump(
        &mut h,
        &mut |a: &JdUi| a.state.palette.is_none(),
        50,
        "palette closes",
    );
    // Let egui's click-count timer expire so the next pair registers as a
    // fresh double-click (not clicks 3+4 of the first sequence).
    for _ in 0..40 {
        h.step();
    }
    double_click(&mut h);
    common::pump(
        &mut h,
        &mut |a: &JdUi| a.state.session.open_card.is_some(),
        100,
        "double-click opens the card after the palette closes",
    );
    assert_eq!(h.state().state.session.open_card, Some(note_id));
}

/// While the palette is open, desk surface keys do nothing: ArrowRight must
/// NOT move card focus. After Esc closes the palette, the same key works.
#[test]
fn surface_keys_suppressed_while_palette_open() {
    let (_vault, mut h, ids) = app_with_notes(&[("Alpha idea", "first body")]);
    let note_id = ids[0];

    // Place the card on the desk so ArrowRight has something to focus.
    let desk_id = h.state().state.session.desks[0].id;
    let _ = h.state_mut().state.session.apply(&SessionOp::Place {
        desk: desk_id,
        id: note_id,
        pos: Vec2 { x: 0.0, y: 0.0 },
    });
    h.run_ok();

    open_palette(&mut h);

    // ArrowRight while the palette is open: desk focus must NOT change.
    h.key_press(egui::Key::ArrowRight);
    h.run_ok();
    assert_eq!(
        h.state().state.focus,
        None,
        "desk focus must not move while the palette is open"
    );

    // Close the palette; now the same key moves desk focus.
    h.key_press(egui::Key::Escape);
    common::pump(
        &mut h,
        &mut |a: &JdUi| a.state.palette.is_none(),
        50,
        "palette closes",
    );
    h.key_press(egui::Key::ArrowRight);
    h.run_ok();
    assert_eq!(
        h.state().state.focus,
        Some(note_id),
        "desk focus must work again after the palette closes"
    );
}
