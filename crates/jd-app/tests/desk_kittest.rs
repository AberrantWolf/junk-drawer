mod common;

use eframe::egui;
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use jd_app::app::JdUi;
use jd_core::command::{Dest, OpSource, VaultOp};
use jd_core::geom::Vec2;
use jd_core::id::NoteId;
use jd_core::session::SessionOp;
use jd_core::worker::{VaultCommand, VaultEvent};

/// n notes titled "Card 1".."Card n" (2-line bodies), created through the real
/// worker, placed on the current desk at (i*350, (i/3)*250).
fn app_with_cards(n: usize) -> (common::TempDir, Harness<'static, JdUi>, Vec<NoteId>) {
    let vault = common::temp_vault();
    let mut app = JdUi::new(vault.path()).expect("JdUi::new");
    let mut ids = Vec::new();
    for i in 1..=n {
        // Build the NewNote seed per jd-core's note.rs (title, body, permanent note).
        let seed = common::new_note(&format!("Card {i}"), "some body text");
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
        // Collect the created id synchronously off the event channel.
        // We forward ScanComplete to the app so drain_events() doesn't miss it later.
        loop {
            match app
                .vault
                .events
                .recv_timeout(std::time::Duration::from_secs(5))
                .expect("OpDone")
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
                    // Replicate drain_events ScanComplete handling directly.
                    app.state.scan_done = true;
                    app.state.bodies.invalidate_all();
                    if app.state.session.desks.is_empty() {
                        use jd_core::id::IdGen;
                        use jd_core::session::{SessionOp, SurfaceId};
                        let mut id_gen = IdGen::new();
                        let desk_id = jd_core::session::DeskId::generate(&mut id_gen);
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
    let desk_id = h.state().state.session.desks[0].id;
    // Direct placement pre-Task-9 (place_card replaces this once it exists).
    for (i, id) in ids.iter().enumerate() {
        let pos = Vec2 {
            x: (i as f32) * 350.0,
            y: ((i / 3) as f32) * 250.0,
        };
        let _ = h.state_mut().state.session.apply(&SessionOp::Place {
            desk: desk_id,
            id: *id,
            pos,
        });
    }
    h.run_ok();
    (vault, h, ids)
}

/// Screen position of a card's center, computed via the camera the same way
/// the desk draws it (world → screen through DeskCamera::to_screen).
fn card_center_on_screen(h: &Harness<'_, JdUi>, id: NoteId) -> egui::Pos2 {
    use jd_app::surfaces::desk::DeskCamera;
    let desk = &h.state().state.session.desks[0];
    let placed = desk
        .cards
        .iter()
        .find(|c| c.id == id)
        .expect("card on desk");
    let vp = desk.viewport;
    let cam = DeskCamera {
        center: egui::vec2(vp.center.x, vp.center.y),
        zoom: vp.zoom,
    };
    // Panel rect: the harness window is 1200×800; the status bar is at the
    // bottom (~24 px). The central panel fills the remainder.
    let panel = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1200.0, 800.0 - 24.0));
    let world = egui::pos2(placed.pos.x, placed.pos.y);
    // Add the card size / 2 to land on the center of the card
    let card_half = egui::vec2(150.0, 100.0); // approximate half-size
    let top_left = cam.to_screen(panel, world);
    top_left + card_half
}

#[test]
fn drag_moves_a_card_and_survives_in_session_state() {
    let (_v, mut h, ids) = app_with_cards(2);
    let from = card_center_on_screen(&h, ids[0]);
    let to = from + egui::vec2(200.0, 40.0);
    h.drag_at(from);
    h.run_ok();
    h.hover_at(to);
    h.run_ok();
    h.drop_at(to);
    h.run_ok();
    let desk = &h.state().state.session.desks[0];
    let placed = desk
        .cards
        .iter()
        .find(|c| c.id == ids[0])
        .expect("still on desk");
    assert!(
        (placed.pos.x - 200.0).abs() < 8.0 && (placed.pos.y - 40.0).abs() < 8.0,
        "world delta ≈ screen delta at zoom 1.0, got {:?}",
        placed.pos
    );
    assert_eq!(h.state().state.journal.undo_label(), Some("Move card"));
}

/// Dragging a card to empty background space must NOT pan the viewport.
/// (Regression guard for fix 1: `state.drag.is_none()` in the background-pan guard.)
#[test]
fn drag_to_empty_space_does_not_pan() {
    // Use 1 card placed at (0,0); drag destination at +(0,250) which is below
    // it and over empty canvas (no card sits there).
    let (_v, mut h, ids) = app_with_cards(1);
    let from = card_center_on_screen(&h, ids[0]);
    let to = from + egui::vec2(0.0, 250.0);

    // Capture viewport center before drag.
    let before_center = h.state().state.session.desks[0].viewport.center;

    h.drag_at(from);
    h.run_ok();
    h.hover_at(to);
    h.run_ok();
    h.drop_at(to);
    h.run_ok();

    let desk = &h.state().state.session.desks[0];

    // (a) Card moved by approximately the drag delta (zoom=1.0 so px ≈ world units).
    // Card started at (0,0); drag was +250 in y, 0 in x.
    let placed = desk
        .cards
        .iter()
        .find(|c| c.id == ids[0])
        .expect("card still on desk after drag");
    assert!(
        placed.pos.x.abs() < 8.0,
        "card x should be unchanged (drag was vertical), got x={:.1}",
        placed.pos.x
    );
    assert!(
        (placed.pos.y - 250.0).abs() < 8.0,
        "card should move ≈250 world units down, got y={:.1}",
        placed.pos.y
    );

    // (b) Viewport center UNCHANGED — no pan fired during the card drag.
    let after_center = desk.viewport.center;
    assert!(
        (after_center.x - before_center.x).abs() < 2.0
            && (after_center.y - before_center.y).abs() < 2.0,
        "viewport must not pan during card drag; before={before_center:?} after={after_center:?}"
    );
}

#[test]
fn pan_and_zoom_change_the_camera_and_clamp() {
    let (_v, mut h, _ids) = app_with_cards(1);
    let before = h.state().state.session.desks[0].viewport;
    h.hover_at(egui::pos2(600.0, 400.0));
    h.event(egui::Event::MouseWheel {
        unit: egui::MouseWheelUnit::Point,
        delta: egui::vec2(0.0, -120.0),
        phase: egui::TouchPhase::Move,
        modifiers: egui::Modifiers::NONE,
    });
    h.run_ok();
    assert!(
        h.state().state.session.desks[0].viewport.center.y != before.center.y,
        "scroll pans"
    );
    for _ in 0..200 {
        h.event(egui::Event::MouseWheel {
            unit: egui::MouseWheelUnit::Point,
            delta: egui::vec2(0.0, 120.0),
            phase: egui::TouchPhase::Move,
            modifiers: egui::Modifiers::COMMAND,
        });
    }
    h.run_ok();
    let z = h.state().state.session.desks[0].viewport.zoom;
    assert!((z - 2.0).abs() < 1e-3, "zoom clamps at ZOOM_MAX, got {z}");
}

#[test]
fn offscreen_cards_are_culled_from_the_accesskit_tree() {
    let (_v, mut h, ids) = app_with_cards(1);
    let desk_id = h.state().state.session.desks[0].id;

    // Create a real second note via the worker.
    let seed = common::new_note("Far card", "far away body");
    h.state_mut()
        .vault
        .commands
        .send(VaultCommand::Op {
            op: VaultOp::Create {
                seed,
                dest: Dest::Notes,
            },
            source: OpSource::User,
        })
        .unwrap();
    let far_id;
    loop {
        match h
            .state_mut()
            .vault
            .events
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("OpDone for far card")
        {
            VaultEvent::OpDone { result, .. } => {
                far_id = result
                    .created
                    .into_iter()
                    .next()
                    .expect("Create yields an id");
                break;
            }
            _ => continue,
        }
    }

    // Place the far card at (100_000.0, 0.0) — well off-screen.
    let _ = h.state_mut().state.session.apply(&SessionOp::Place {
        desk: desk_id,
        id: far_id,
        pos: Vec2 {
            x: 100_000.0,
            y: 0.0,
        },
    });
    let _ = &ids;
    h.run_ok();

    assert!(
        h.query_by_label_contains("Card: 'Far card'").is_none(),
        "culled card has no node"
    );
    h.get_by_label_contains("Card: 'Card 1'");

    // zoom_to_fit brings it into view → node appears (drive the status-line Fit button by label).
    h.get_by_label("Fit").click();
    h.run_ok();
    assert!(h.query_by_label_contains("Card: 'Far card'").is_some());
}

/// Arrow-key navigation to an off-screen card triggers reveal(), centering the
/// viewport on it so it is no longer culled (Fix 3: wire reveal in app.rs).
#[test]
fn arrowkey_to_offscreen_card_reveals_it() {
    // Start with 1 card at (0,0); place a far card very far to the right.
    let (_v, mut h, ids) = app_with_cards(1);
    let desk_id = h.state().state.session.desks[0].id;

    let seed = common::new_note("Reveal target", "body");
    h.state_mut()
        .vault
        .commands
        .send(VaultCommand::Op {
            op: VaultOp::Create {
                seed,
                dest: Dest::Notes,
            },
            source: OpSource::User,
        })
        .unwrap();
    let far_id;
    loop {
        match h
            .state_mut()
            .vault
            .events
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("OpDone for reveal target")
        {
            VaultEvent::OpDone { result, .. } => {
                far_id = result
                    .created
                    .into_iter()
                    .next()
                    .expect("Create yields an id");
                break;
            }
            _ => continue,
        }
    }

    // Place it far off to the right — culled from the initial viewport.
    let _ = h.state_mut().state.session.apply(&SessionOp::Place {
        desk: desk_id,
        id: far_id,
        pos: Vec2 {
            x: 50_000.0,
            y: 0.0,
        },
    });
    let _ = &ids;
    h.run_ok();

    // Verify it's culled before reveal.
    assert!(
        h.query_by_label_contains("Card: 'Reveal target'").is_none(),
        "far card must be culled before reveal"
    );

    // ArrowRight until focus lands on the far card (reading order: Card 1 then Reveal target).
    // First ArrowRight selects the first card; second selects far card.
    h.key_press(egui::Key::ArrowRight);
    h.run_ok();
    h.key_press(egui::Key::ArrowRight);
    h.run_ok();

    // Focus should now be on far_id.
    assert_eq!(
        h.state().state.focus,
        Some(far_id),
        "focus should have moved to the far card"
    );

    // reveal() was called → viewport centered on far card → no longer culled.
    assert!(
        h.query_by_label_contains("Card: 'Reveal target'").is_some(),
        "after reveal, far card AccessKit node must exist (not culled)"
    );
}

#[test]
fn enter_opens_focused_card() {
    let (_v, mut h, ids) = app_with_cards(2);
    h.key_press(egui::Key::ArrowRight);
    h.run_ok();
    assert_eq!(h.state().state.focus, Some(ids[0]));
    h.key_press(egui::Key::Enter);
    common::pump(
        &mut h,
        &mut |a: &JdUi| a.state.session.open_card == Some(ids[0]),
        100,
        "open card",
    );
    // Task 10 upgrades this to assert the editor widget exists.
}

#[test]
fn backspace_puts_away_not_deletes() {
    let (_v, mut h, ids) = app_with_cards(2);
    h.key_press(egui::Key::ArrowRight);
    h.run_ok();
    h.key_press(egui::Key::Backspace);
    h.run_ok();
    let desk = &h.state().state.session.desks[0];
    assert!(!desk.cards.iter().any(|c| c.id == ids[0]), "off the desk");
    assert!(
        h.state().vault.index.read().unwrap().get(ids[0]).is_some(),
        "note still exists"
    );
    assert_eq!(h.state().state.journal.undo_label(), Some("Put card away"));
}
