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

#[test]
fn ctrl_n_creates_a_scrap_on_the_desk_with_editor_open() {
    // Ctrl+N → pump until the card exists on the desk (pending_create consumed),
    // session.open_card is the new id, and the index shows a Fleeting note in inbox/.
    let (_v, mut h, _ids) = app_with_cards(0);

    // Send Ctrl+N shortcut.
    h.event(egui::Event::Key {
        key: egui::Key::N,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers: egui::Modifiers::COMMAND,
    });
    h.run_ok();

    // Pump until pending_create is consumed (card placed on desk).
    common::pump(
        &mut h,
        &mut |a: &JdUi| {
            a.state.pending_create.is_none()
                && !a.state.session.desks.is_empty()
                && !a.state.session.desks[0].cards.is_empty()
        },
        200,
        "ctrl+n: scrap placed on desk",
    );

    let desk = &h.state().state.session.desks[0];
    assert_eq!(desk.cards.len(), 1, "exactly one card placed");
    let placed_id = desk.cards[0].id;

    // open_card should be the new note id (editor opened).
    assert_eq!(
        h.state().state.session.open_card,
        Some(placed_id),
        "open_card must be the newly created scrap"
    );

    // The index must show a Fleeting note.
    let idx = h.state().vault.index.read().unwrap();
    let meta = idx.get(placed_id).expect("scrap in index");
    assert_eq!(
        meta.status,
        jd_core::note::Status::Fleeting,
        "created note must be Fleeting"
    );
    // Scrap lives in inbox/ (Dest::Inbox).
    assert!(
        meta.rel_path.starts_with("inbox"),
        "scrap must be in inbox/, got {:?}",
        meta.rel_path
    );
}

#[test]
fn session_survives_restart_exactly() {
    // Build app, place 3 cards, pan+zoom, OPEN one card (open_card set), drop
    // the harness/JdUi (Drop saves), build a NEW JdUi on the same vault root,
    // pump scan; assert desks/positions/viewport/open_card round-trip exactly.
    let vault = common::temp_vault();

    // ---- First session ----
    let app1 = JdUi::new(vault.path()).expect("JdUi::new first");
    // Wait for scan + default desk.
    {
        let mut h = egui_kittest::Harness::builder()
            .with_size(egui::vec2(1200.0, 800.0))
            .build_ui_state(|ui, app: &mut JdUi| app.ui(ui), app1);
        common::pump(
            &mut h,
            &mut |a: &JdUi| a.state.scan_done && !a.state.session.desks.is_empty(),
            200,
            "scan + default desk (session restart test)",
        );

        // Create 3 notes via worker.
        let mut created_ids: Vec<NoteId> = Vec::new();
        for i in 1..=3u32 {
            let seed = common::new_note(&format!("Restart {i}"), "body");
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
            loop {
                match h
                    .state_mut()
                    .vault
                    .events
                    .recv_timeout(std::time::Duration::from_secs(5))
                    .expect("OpDone for restart note")
                {
                    VaultEvent::OpDone { result, .. } => {
                        created_ids
                            .push(result.created.into_iter().next().expect("Create yields id"));
                        break;
                    }
                    _ => continue,
                }
            }
        }

        // Place the 3 cards at specific positions.
        let desk_id = h.state().state.session.desks[0].id;
        for (i, &id) in created_ids.iter().enumerate() {
            let _ = h.state_mut().state.session.apply(&SessionOp::Place {
                desk: desk_id,
                id,
                pos: Vec2 {
                    x: 120.5 + (i as f32) * 200.0,
                    y: -80.0,
                },
            });
        }

        // Pan + zoom to known values that round-trip through f32 serialization.
        if let Some(d) = h
            .state_mut()
            .state
            .session
            .desks
            .iter_mut()
            .find(|d| d.id == desk_id)
        {
            d.viewport.center = jd_core::geom::Vec2 { x: 120.5, y: -80.0 };
            d.viewport.zoom = 1.25;
        }

        // Open card 0.
        h.state_mut().state.session.open_card = Some(created_ids[0]);

        // Mark dirty so Drop saves.
        h.state_mut().state.session_dirty_at = Some(std::time::Instant::now());

        h.run_ok();

        // Extract the session state we expect to survive.
        let expected_desk = h.state().state.session.desks[0].clone();
        let expected_open = h.state().state.session.open_card;

        // Drop the harness, consuming JdUi → Drop impl saves session.
        let jd_ui = h.into_state();
        drop(jd_ui);

        // ---- Second session: new JdUi on same vault ----
        let app2 = JdUi::new(vault.path()).expect("JdUi::new second");
        let mut h2 = egui_kittest::Harness::builder()
            .with_size(egui::vec2(1200.0, 800.0))
            .build_ui_state(|ui, app: &mut JdUi| app.ui(ui), app2);
        common::pump(
            &mut h2,
            &mut |a: &JdUi| a.state.scan_done && !a.state.session.desks.is_empty(),
            200,
            "scan + desk (second session)",
        );

        let reloaded_desk = &h2.state().state.session.desks[0];

        // Same desk id.
        assert_eq!(
            reloaded_desk.id, expected_desk.id,
            "desk id must survive restart"
        );

        // Same viewport (f32 exact round-trip for 120.5, -80.0, 1.25).
        assert_eq!(
            reloaded_desk.viewport.center.x, expected_desk.viewport.center.x,
            "viewport center.x must survive restart"
        );
        assert_eq!(
            reloaded_desk.viewport.center.y, expected_desk.viewport.center.y,
            "viewport center.y must survive restart"
        );
        assert_eq!(
            reloaded_desk.viewport.zoom, expected_desk.viewport.zoom,
            "viewport zoom must survive restart"
        );

        // Same card positions.
        assert_eq!(
            reloaded_desk.cards.len(),
            expected_desk.cards.len(),
            "card count must survive restart"
        );
        for ec in &expected_desk.cards {
            let rc = reloaded_desk
                .cards
                .iter()
                .find(|c| c.id == ec.id)
                .expect("placed card must survive restart");
            assert_eq!(rc.pos.x, ec.pos.x, "card x must survive restart");
            assert_eq!(rc.pos.y, ec.pos.y, "card y must survive restart");
        }

        // open_card survives.
        assert_eq!(
            h2.state().state.session.open_card,
            expected_open,
            "open_card must survive restart"
        );
    }
}

/// Helper: open the editor for a card that is currently focused.
/// Presses Enter and waits until `state.editor.is_some()`.
/// Mirrors the open_editor() helper in editor_kittest.rs.
fn open_editor_for_focused(h: &mut Harness<'_, JdUi>, id: NoteId) {
    h.key_press(egui::Key::Enter);
    common::pump(
        h,
        &mut |a: &JdUi| a.state.editor.is_some(),
        200,
        "editor opens",
    );
    for _ in 0..3 {
        h.step();
    }
    h.get_by_role(egui::accesskit::Role::MultilineTextInput)
        .focus();
    h.get_by_role(egui::accesskit::Role::MultilineTextInput)
        .click();
    h.run_ok();
    let _ = id; // used only for doc clarity
}

/// End-to-end M2 scenario: fresh vault → Ctrl+N → type a thought → Esc → card on
/// desk → drag it → Enter reopens → type `[[Target Note]]` via autocomplete →
/// Esc → restart → verify everything persisted.
#[test]
fn m2_end_to_end_scenario() {
    // --- Setup: fresh vault with one pre-existing note "Target Note" ----------
    let vault = common::temp_vault();
    let mut app = JdUi::new(vault.path()).expect("JdUi::new");

    // Create "Target Note" via worker so the index can offer it for autocomplete.
    let target_seed = jd_core::note::NewNote {
        body: "# Target Note\ncontent of target".to_owned(),
        status: jd_core::note::Status::Permanent,
        kind: jd_core::note::Kind::Note,
        source: None,
        tags: Vec::new(),
    };
    app.vault
        .commands
        .send(VaultCommand::Op {
            op: VaultOp::Create {
                seed: target_seed,
                dest: Dest::Notes,
            },
            source: OpSource::User,
        })
        .unwrap();
    // Drain until we get the OpDone; handle ScanComplete inline.
    loop {
        match app
            .vault
            .events
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("OpDone for Target Note")
        {
            VaultEvent::OpDone { .. } => break,
            VaultEvent::ScanComplete { .. } => {
                app.state.scan_done = true;
                app.state.bodies.invalidate_all();
                if app.state.session.desks.is_empty() {
                    use jd_core::id::IdGen;
                    use jd_core::session::{DeskId, SessionOp, SurfaceId};
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

    let mut h = Harness::builder()
        .with_size(egui::vec2(1200.0, 800.0))
        .build_ui_state(|ui, app: &mut JdUi| app.ui(ui), app);

    // Pump until scan is done and the desk exists.
    common::pump(
        &mut h,
        &mut |a: &JdUi| {
            a.state.scan_done
                && !a.state.session.desks.is_empty()
                && a.vault
                    .index
                    .read()
                    .unwrap()
                    .resolve_title("Target Note")
                    .is_some()
        },
        200,
        "scan + Target Note indexed",
    );

    // --- Step 1: Ctrl+N → scrap created, placed on desk, editor open ----------
    h.event(egui::Event::Key {
        key: egui::Key::N,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers: egui::Modifiers::COMMAND,
    });
    h.run_ok();

    common::pump(
        &mut h,
        &mut |a: &JdUi| {
            a.state.pending_create.is_none()
                && !a.state.session.desks[0].cards.is_empty()
                && a.state.editor.is_some()
        },
        200,
        "ctrl+n: scrap on desk with editor open",
    );

    let scrap_id = h.state().state.session.desks[0].cards[0].id;
    assert_eq!(
        h.state().state.session.open_card,
        Some(scrap_id),
        "open_card must be the new scrap"
    );
    assert!(
        h.state().state.editor.is_some(),
        "editor must be open after Ctrl+N"
    );

    // --- Step 2: Type a thought into the editor --------------------------------
    // Ensure the editor has focus first (mirrors editor_kittest focus sequence).
    for _ in 0..3 {
        h.step();
    }
    h.get_by_role(egui::accesskit::Role::MultilineTextInput)
        .focus();
    h.get_by_role(egui::accesskit::Role::MultilineTextInput)
        .click();
    h.run_ok();

    let node = h.get_by_role(egui::accesskit::Role::MultilineTextInput);
    node.type_text("a first thought");
    h.step();
    h.run_ok();

    // Verify the buffer contains the typed text.
    let buf_after_typing = h.state().state.editor.as_ref().unwrap().buffer.clone();
    assert!(
        buf_after_typing.contains("a first thought"),
        "buffer must contain the typed text, got: {:?}",
        buf_after_typing
    );

    // --- Step 3: Esc → editor closes, card stays on desk ----------------------
    h.key_press(egui::Key::Escape);
    common::pump(
        &mut h,
        &mut |a: &JdUi| a.state.editor.is_none(),
        100,
        "editor closes on Esc",
    );
    assert!(
        h.state().state.editor.is_none(),
        "editor must be closed after Esc"
    );
    assert!(
        h.state().state.session.desks[0]
            .cards
            .iter()
            .any(|c| c.id == scrap_id),
        "scrap card must still be on the desk after Esc"
    );

    // --- Step 4: Drag the card to a new position ------------------------------
    let from = card_center_on_screen(&h, scrap_id);
    let to = from + egui::vec2(200.0, 50.0);
    h.drag_at(from);
    h.run_ok();
    h.hover_at(to);
    h.run_ok();
    h.drop_at(to);
    h.run_ok();

    // The card's position in session state must have moved.
    let pos_after_drag = h.state().state.session.desks[0]
        .cards
        .iter()
        .find(|c| c.id == scrap_id)
        .map(|c| c.pos)
        .expect("card still on desk after drag");
    // At zoom=1.0 screen delta ≈ world delta; original pos was (≈0,0), expect ≈(200,50).
    assert!(
        pos_after_drag.x.abs() > 10.0 || pos_after_drag.y.abs() > 10.0,
        "card world pos must have changed after drag, got {:?}",
        pos_after_drag
    );

    // --- Step 5: Focus the card and reopen the editor via Enter ---------------
    // There is only one card on the desk (the scrap). Clear focus first so that
    // a single ArrowRight lands on it (next_focus with current=None returns the
    // first card in reading order).
    h.state_mut().state.focus = None;
    h.run_ok();
    h.key_press(egui::Key::ArrowRight);
    h.run_ok();
    assert_eq!(
        h.state().state.focus,
        Some(scrap_id),
        "ArrowRight must focus the scrap card"
    );

    open_editor_for_focused(&mut h, scrap_id);

    assert_eq!(
        h.state().state.session.open_card,
        Some(scrap_id),
        "open_card must be scrap after reopening"
    );

    // --- Step 6: Type "[[Tar" to trigger autocomplete -------------------------
    let node = h.get_by_role(egui::accesskit::Role::MultilineTextInput);
    node.type_text("[[Tar");
    h.step();
    h.step();
    h.run_ok();

    // The autocomplete popup must show "Target Note".
    h.get_by_label("Target Note");

    // --- Step 7: Enter accepts the autocomplete candidate ---------------------
    h.key_press(egui::Key::Enter);
    h.step();
    h.run_ok();

    let buf_with_link = h.state().state.editor.as_ref().unwrap().buffer.clone();
    assert!(
        buf_with_link.contains("[[Target Note]]"),
        "accepting autocomplete must insert [[Target Note]], got: {:?}",
        buf_with_link
    );
    assert!(
        !buf_with_link.contains("]]]]"),
        "closing brackets must not be doubled, got: {:?}",
        buf_with_link
    );

    // --- Step 8: Esc → saves, closes editor -----------------------------------
    h.key_press(egui::Key::Escape);
    common::pump(
        &mut h,
        &mut |a: &JdUi| a.state.editor.is_none(),
        100,
        "editor closes on Esc (after link)",
    );
    assert!(
        h.state().state.editor.is_none(),
        "editor must be closed after second Esc"
    );

    // Give the worker time to write the SaveBody to disk.
    std::thread::sleep(std::time::Duration::from_millis(300));
    h.run_ok();

    // Verify the body on disk contains [[Target Note]].
    let scrap_rel_path = {
        let idx = h.state().vault.index.read().unwrap();
        idx.get(scrap_id).expect("scrap in index").rel_path.clone()
    };
    let scrap_abs = vault.path().join(&scrap_rel_path);
    let disk_content = std::fs::read_to_string(&scrap_abs).expect("read scrap file from disk");
    let doc = jd_core::doc::NoteDoc::parse(&disk_content);
    assert!(
        doc.body.contains("[[Target Note]]"),
        "disk body must contain [[Target Note]], got: {:?}",
        doc.body
    );

    // Capture the exact card positions and viewport for the restart assertions.
    let expected_pos = pos_after_drag;
    let expected_viewport = h.state().state.session.desks[0].viewport;
    let expected_open_card = h.state().state.session.open_card; // None (editor just closed)

    // Mark session dirty and set known viewport values that round-trip through f32.
    h.state_mut().state.session.desks[0].viewport.center =
        jd_core::geom::Vec2 { x: 10.0, y: -20.0 };
    h.state_mut().state.session.desks[0].viewport.zoom = 1.0;
    let expected_center_x = 10.0_f32;
    let expected_center_y = -20.0_f32;
    h.state_mut().state.session_dirty_at = Some(std::time::Instant::now());
    h.run_ok();

    // Force a save by sleeping past the 1s debounce.
    std::thread::sleep(std::time::Duration::from_millis(1100));
    h.run_ok();

    let _ = expected_viewport;
    let _ = expected_open_card;

    // --- Step 9: Drop harness (saves session on Drop), restart, verify --------
    let jd_ui = h.into_state();
    drop(jd_ui);

    // Build a new JdUi on the same vault root.
    let app2 = JdUi::new(vault.path()).expect("JdUi::new second session");
    let mut h2 = Harness::builder()
        .with_size(egui::vec2(1200.0, 800.0))
        .build_ui_state(|ui, app: &mut JdUi| app.ui(ui), app2);
    common::pump(
        &mut h2,
        &mut |a: &JdUi| a.state.scan_done && !a.state.session.desks.is_empty(),
        200,
        "scan + desk (second session)",
    );

    // Both cards (scrap + Target Note) must be present in the index.
    {
        let idx = h2.state().vault.index.read().unwrap();
        assert!(
            idx.get(scrap_id).is_some(),
            "scrap note must survive restart in the index"
        );
        assert!(
            idx.resolve_title("Target Note").is_some(),
            "Target Note must survive restart in the index"
        );
    }

    // Scrap card must be on the desk at (approximately) the post-drag position.
    let reloaded_desk = &h2.state().state.session.desks[0];
    let reloaded_scrap = reloaded_desk
        .cards
        .iter()
        .find(|c| c.id == scrap_id)
        .expect("scrap card must be on the desk after restart");
    assert!(
        (reloaded_scrap.pos.x - expected_pos.x).abs() < 1.0
            && (reloaded_scrap.pos.y - expected_pos.y).abs() < 1.0,
        "scrap card position must survive restart; expected {:?}, got {:?}",
        expected_pos,
        reloaded_scrap.pos
    );

    // Viewport must be the values we set explicitly.
    assert_eq!(
        reloaded_desk.viewport.center.x, expected_center_x,
        "viewport center.x must survive restart"
    );
    assert_eq!(
        reloaded_desk.viewport.center.y, expected_center_y,
        "viewport center.y must survive restart"
    );

    // open_card: we closed the editor (None) before drop; must be None after restart.
    assert_eq!(
        h2.state().state.session.open_card,
        None,
        "open_card must be None after restart (editor was closed before drop)"
    );

    // Finally: confirm the body on disk still contains [[Target Note]] (no rewrite).
    let disk_content2 = std::fs::read_to_string(&scrap_abs).expect("read scrap after restart");
    let doc2 = jd_core::doc::NoteDoc::parse(&disk_content2);
    assert!(
        doc2.body.contains("[[Target Note]]"),
        "body on disk must still contain [[Target Note]] after restart, got: {:?}",
        doc2.body
    );
}

#[test]
fn session_save_is_debounced_not_per_frame() {
    // Move a card, read session.jd mtime immediately (unchanged),
    // pump past 1s (std::thread::sleep(1100ms) + step), assert file updated.
    use std::time::{Duration, SystemTime};

    let vault = common::temp_vault();
    let session_path = vault
        .path()
        .join(".junkdrawer")
        .join("session")
        .join("session.jd");

    // Build a fresh app on our vault.
    let app = JdUi::new(vault.path()).expect("JdUi::new debounce");
    let mut h = egui_kittest::Harness::builder()
        .with_size(egui::vec2(1200.0, 800.0))
        .build_ui_state(|ui, app: &mut JdUi| app.ui(ui), app);
    common::pump(
        &mut h,
        &mut |a: &JdUi| a.state.scan_done && !a.state.session.desks.is_empty(),
        200,
        "scan + desk (debounce test)",
    );

    // Capture mtime before any change.
    let mtime_before: Option<SystemTime> =
        session_path.metadata().ok().and_then(|m| m.modified().ok());

    // Mark session dirty (simulates a Move) — but do NOT wait 1s.
    h.state_mut().state.session_dirty_at = Some(std::time::Instant::now());
    h.run_ok();

    // Immediately after marking dirty, file must NOT yet be updated.
    let mtime_immediate: Option<SystemTime> =
        session_path.metadata().ok().and_then(|m| m.modified().ok());
    assert_eq!(
        mtime_before, mtime_immediate,
        "session must NOT be saved immediately after marking dirty (debounce)"
    );

    // Sleep past the 1s debounce, then step to trigger the save.
    std::thread::sleep(Duration::from_millis(1100));
    h.run_ok();

    // File must now exist and be newer.
    let mtime_after: Option<SystemTime> =
        session_path.metadata().ok().and_then(|m| m.modified().ok());
    assert!(
        session_path.exists(),
        "session.jd must exist after debounce save"
    );
    assert!(
        mtime_after > mtime_before,
        "session.jd mtime must advance after debounce save; before={mtime_before:?} after={mtime_after:?}"
    );
}
