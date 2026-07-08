//! WP4 Task 4: the Drawer — mini grid + filter chips.
//!
//! Everything drives the real UI through egui_kittest/AccessKit, mirroring
//! the palette_kittest patterns (pump for worker events, a11y-label queries).

mod common;

use eframe::egui;
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use jd_app::app::JdUi;
use jd_core::command::{Dest, OpSource, VaultOp};
use jd_core::id::NoteId;
use jd_core::note::{Kind, NewNote, Status};
use jd_core::session::{SessionOp, SurfaceId};
use jd_core::worker::{VaultCommand, VaultEvent};

/// Create an app with the given note seeds already in the vault.
/// Returns (vault_dir, harness, note ids in creation order).
fn app_with_seeds(
    seeds: Vec<(NewNote, Dest)>,
) -> (common::TempDir, Harness<'static, JdUi>, Vec<NoteId>) {
    let vault = common::temp_vault();
    let mut app = JdUi::new(vault.path()).expect("JdUi::new");

    let mut ids: Vec<NoteId> = Vec::new();
    for (seed, dest) in seeds {
        app.vault
            .commands
            .send(VaultCommand::Op {
                op: VaultOp::Create { seed, dest },
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
                        use jd_core::session::DeskId;
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

fn permanent(title: &str, body: &str) -> (NewNote, Dest) {
    (common::new_note(title, body), Dest::Notes)
}

fn fleeting(body: &str) -> (NewNote, Dest) {
    (
        NewNote {
            body: body.to_owned(),
            status: Status::Fleeting,
            kind: Kind::Note,
            source: None,
            tags: Vec::new(),
        },
        Dest::Inbox,
    )
}

/// Switch the app to the Drawer surface (navigation, direct set — the rail
/// Switch idiom) and render a few frames.
fn to_drawer(h: &mut Harness<'_, JdUi>) {
    h.state_mut().state.session.current_surface = Some(SurfaceId::Drawer);
    h.run_ok();
}

/// Click the chip whose current a11y label is "Filter: <name>, <state>".
fn click_chip(h: &mut Harness<'_, JdUi>, name: &str, currently_active: bool) {
    let state = if currently_active {
        "active"
    } else {
        "inactive"
    };
    h.get_by_label(format!("Filter: {name}, {state}").as_str())
        .click();
    h.run_ok();
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// The grid orders minis newest-modified first. Modified timestamps are
/// second-precision on disk, so the test staggers SaveBody ops >1s apart.
#[test]
fn grid_orders_newest_modified_first() {
    let (_vault, mut h, ids) = app_with_seeds(vec![
        permanent("Alpha", "a"),
        permanent("Beta", "b"),
        permanent("Gamma", "c"),
    ]);
    let (a, b, _c) = (ids[0], ids[1], ids[2]);

    // Stagger: save A, then (>1s later) save B. Modified: B > A > C.
    // Bodies keep their `# ` heading so the titles survive the save.
    for &(id, body) in &[(a, "# Alpha\na2"), (b, "# Beta\nb2")] {
        std::thread::sleep(std::time::Duration::from_millis(1100));
        let before = {
            let idx = h.state().vault.index.read().unwrap();
            idx.get(id).unwrap().modified
        };
        h.state()
            .vault
            .commands
            .send(VaultCommand::Op {
                op: VaultOp::SaveBody {
                    id,
                    content: body.to_owned(),
                },
                source: OpSource::User,
            })
            .unwrap();
        common::pump(
            &mut h,
            &mut |app: &JdUi| {
                let idx = app.vault.index.read().unwrap();
                idx.get(id).is_some_and(|m| m.modified > before)
            },
            400,
            "SaveBody bumps modified in the index",
        );
    }

    to_drawer(&mut h);

    let order = {
        let idx = h.state().vault.index.read().unwrap();
        jd_app::surfaces::drawer::drawer_ids(
            &idx,
            &h.state().state.drawer_filters,
            &h.state().state.conflicts,
        )
    };
    assert_eq!(
        order,
        vec![ids[1], ids[0], ids[2]],
        "order must be newest-modified first: B (saved last), A, C (never saved)"
    );

    // The newest face renders in the grid (real card widget, a11y-labeled).
    assert!(
        h.query_by_label_contains("Card: 'Beta'").is_some(),
        "mini face for the newest note must render"
    );
}

/// Chips compose with AND: status=Cards AND tag=#zeta shows only permanent
/// notes tagged #zeta.
#[test]
fn chips_compose_status_and_tag() {
    let (_vault, mut h, ids) = app_with_seeds(vec![
        permanent("Perm X", "body with #zeta"),
        permanent("Perm Plain", "no tags here"),
        fleeting("scrap with #zeta"),
    ]);

    to_drawer(&mut h);
    // Scrap a11y labels quote the first body line — wait for the body fetch.
    let scrap_id = ids[2];
    common::pump(
        &mut h,
        &mut |a: &JdUi| a.state.bodies.get_cached(scrap_id).is_some(),
        200,
        "scrap body arrives",
    );
    h.run_ok();

    // All three faces visible before filtering.
    assert!(h.query_by_label_contains("Card: 'Perm X'").is_some());
    assert!(h.query_by_label_contains("Card: 'Perm Plain'").is_some());
    assert!(h.query_by_label_contains("Scrap: 'scrap with").is_some());

    // Toggle the Cards chip on.
    click_chip(&mut h, "Cards", false);

    // Open the tag picker and choose #zeta (listed with its count).
    h.get_by_label("Filter: Tag picker").click();
    h.run_ok();
    h.get_by_label_contains("Tag: #zeta").click();
    h.run_ok();

    // Only the permanent #zeta note remains.
    assert!(
        h.query_by_label_contains("Card: 'Perm X'").is_some(),
        "permanent note tagged #zeta must pass both chips"
    );
    assert!(
        h.query_by_label_contains("Card: 'Perm Plain'").is_none(),
        "untagged permanent note must be filtered out"
    );
    assert!(
        h.query_by_label_contains("Scrap: 'scrap with").is_none(),
        "fleeting scrap must be filtered out by the Cards chip"
    );
    // Chips announce their active state.
    h.get_by_label("Filter: Cards, active");
    h.get_by_label("Filter: #zeta, active");
}

/// The Unlinked chip keeps only notes with no outgoing links AND no backlinks:
/// A links to B → both are linked; C stands alone → only C remains.
#[test]
fn unlinked_chip_keeps_only_standalone_notes() {
    let (_vault, mut h, _ids) = app_with_seeds(vec![
        permanent("Alpha note", "see [[Beta note]]"),
        permanent("Beta note", "the target"),
        permanent("Gamma alone", "no links at all"),
    ]);

    to_drawer(&mut h);
    click_chip(&mut h, "Unlinked", false);

    assert!(
        h.query_by_label_contains("Card: 'Alpha note'").is_none(),
        "A has an outgoing link → not unlinked"
    );
    assert!(
        h.query_by_label_contains("Card: 'Beta note'").is_none(),
        "B has a backlink → not unlinked"
    );
    assert!(
        h.query_by_label_contains("Card: 'Gamma alone'").is_some(),
        "C has no links either way → unlinked"
    );
}

/// Needs Attention: a garbage-bytes file written pre-scan is quarantined and
/// renders as an inert row with the reason (no face, a11y-labeled).
#[test]
fn needs_attention_shows_quarantined_inert_row() {
    let vault = common::temp_vault();
    // Write an unreadable (invalid UTF-8) file BEFORE the scan.
    let notes_dir = vault.path().join("notes");
    std::fs::create_dir_all(&notes_dir).unwrap();
    std::fs::write(notes_dir.join("bad.md"), [0xff, 0xfe, 0xfd, 0x00, 0xc3]).unwrap();

    let app = JdUi::new(vault.path()).expect("JdUi::new");
    let mut h = Harness::builder()
        .with_size(egui::vec2(1200.0, 800.0))
        .build_ui_state(|ui, app: &mut JdUi| app.ui(ui), app);
    common::pump(
        &mut h,
        &mut |a: &JdUi| a.state.scan_done && !a.state.quarantined.is_empty(),
        400,
        "scan quarantines the garbage file",
    );

    to_drawer(&mut h);
    click_chip(&mut h, "Needs Attention", false);

    let reason = h.state().state.quarantined[0].error.clone();
    let expected = format!("Quarantined: 'bad.md' — {reason}");
    h.get_by_label(expected.as_str());

    // The row is inert: no card face rendered for it, and pressing Enter
    // (nothing focusable) opens no editor.
    h.key_press(egui::Key::Enter);
    h.run_ok();
    assert!(
        h.state().state.session.open_card.is_none(),
        "quarantined rows must not be openable"
    );
    assert!(h.state().state.editor.is_none());
}

/// Enter on a focused mini opens the editor in place (same open path as the
/// desk; the editor overlay is surface-agnostic).
#[test]
fn enter_on_focused_mini_opens_editor() {
    let (_vault, mut h, ids) = app_with_seeds(vec![permanent("Only note", "body text")]);
    let note_id = ids[0];

    to_drawer(&mut h);

    // ArrowDown focuses the first mini (linear focus, row-major).
    h.key_press(egui::Key::ArrowDown);
    h.run_ok();
    assert_eq!(
        h.state().state.focus,
        Some(note_id),
        "first mini must take linear focus"
    );

    h.key_press(egui::Key::Enter);
    h.run_ok();
    assert_eq!(
        h.state().state.session.open_card,
        Some(note_id),
        "Enter must engage the open path"
    );
    common::pump(
        &mut h,
        &mut |a: &JdUi| a.state.editor.is_some(),
        200,
        "editor opens once the body arrives",
    );
    // Still on the Drawer surface — the editor opened in place.
    assert_eq!(
        h.state().state.session.current_surface,
        Some(SurfaceId::Drawer)
    );
}

/// Dismissing a chip (clicking the active toggle) restores the full grid.
#[test]
fn chip_dismiss_restores_full_grid() {
    let (_vault, mut h, _ids) = app_with_seeds(vec![
        permanent("Perm note", "body"),
        fleeting("just a scrap"),
    ]);

    to_drawer(&mut h);

    // Scraps chip on: only the scrap remains.
    click_chip(&mut h, "Scraps", false);
    assert!(h.query_by_label_contains("Scrap: 'just a scrap'").is_some());
    assert!(
        h.query_by_label_contains("Card: 'Perm note'").is_none(),
        "permanent note must be filtered out while Scraps is active"
    );

    // Dismiss the chip: full grid restored.
    click_chip(&mut h, "Scraps", true);
    assert!(h.query_by_label_contains("Scrap: 'just a scrap'").is_some());
    assert!(
        h.query_by_label_contains("Card: 'Perm note'").is_some(),
        "dismissing the chip must restore the full grid"
    );
}
