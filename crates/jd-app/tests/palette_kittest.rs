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
