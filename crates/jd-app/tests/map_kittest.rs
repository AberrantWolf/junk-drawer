//! WP5 Task 2: the Map surface — nodes, edges, settle-freeze, position cache.
//!
//! Everything drives the real UI through egui_kittest/AccessKit, mirroring
//! the drawer_kittest patterns (pump for worker events, a11y-label queries).

mod common;

use eframe::egui;
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use jd_app::app::JdUi;
use jd_core::command::{Dest, OpSource, VaultOp};
use jd_core::geom::Vec2;
use jd_core::id::NoteId;
use jd_core::maplayout::MapCache;
use jd_core::note::NewNote;
use jd_core::session::{SessionOp, SurfaceId};
use jd_core::vault::Vault;
use jd_core::worker::{VaultCommand, VaultEvent};

/// Create an app with the given note seeds already in the vault.
/// Returns (vault_dir, harness, note ids in creation order).
fn app_with_seeds(
    seeds: Vec<(NewNote, Dest)>,
) -> (common::TempDir, Harness<'static, JdUi>, Vec<NoteId>) {
    let vault = common::temp_vault();
    let (h, ids) = harness_over(&vault, seeds);
    (vault, h, ids)
}

/// Build a JdUi + harness over an EXISTING vault dir, seeding `seeds` first
/// (pass an empty vec for a plain restart).
fn harness_over(
    vault: &common::TempDir,
    seeds: Vec<(NewNote, Dest)>,
) -> (Harness<'static, JdUi>, Vec<NoteId>) {
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

    (h, ids)
}

fn permanent(title: &str, body: &str) -> (NewNote, Dest) {
    (common::new_note(title, body), Dest::Notes)
}

/// 5 notes, 2 resolved links: Alpha → Beta, Alpha → Gamma.
/// Delta and Epsilon are orphans (degree 0).
fn five_note_fixture() -> Vec<(NewNote, Dest)> {
    vec![
        permanent("Beta", "linked from alpha"),
        permanent("Gamma", "also linked from alpha"),
        permanent("Alpha", "see [[Beta]] and [[Gamma]]"),
        permanent("Delta", "an orphan"),
        permanent("Epsilon", "another orphan"),
    ]
}

/// Switch the app to the Map surface (navigation, direct set — the rail
/// Switch idiom) and render a few frames so the map builds.
fn to_map(h: &mut Harness<'_, JdUi>) {
    h.state_mut().state.session.current_surface = Some(SurfaceId::Map);
    h.run_ok();
}

/// Pump frames until the map layout is settled.
fn pump_settled(h: &mut Harness<'_, JdUi>) {
    common::pump(
        &mut *h,
        &mut |a: &JdUi| a.map.as_ref().is_some_and(|m| m.layout.is_settled()),
        3000,
        "map layout to settle",
    );
}

fn dist(a: Vec2, b: Vec2) -> f32 {
    let (dx, dy) = (a.x - b.x, a.y - b.y);
    (dx * dx + dy * dy).sqrt()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// Every indexed note shows up as an AccessKit-labeled map node.
#[test]
fn map_builds_five_labeled_nodes() {
    let (_vault, mut h, _ids) = app_with_seeds(five_note_fixture());
    to_map(&mut h);
    for title in ["Alpha", "Beta", "Gamma", "Delta", "Epsilon"] {
        // get_by_label panics (failing the test) when absent or ambiguous.
        let _ = h.get_by_label(format!("Map node: '{title}'").as_str());
    }
}

/// The layout settles then FREEZES: positions are bit-stable across further
/// frames (a map, not a lava lamp).
#[test]
fn map_settles_and_freezes() {
    let (_vault, mut h, _ids) = app_with_seeds(five_note_fixture());
    to_map(&mut h);
    pump_settled(&mut h);
    let before = h.state().map.as_ref().unwrap().layout.positions().clone();
    for _ in 0..10 {
        h.step();
    }
    let after = h.state().map.as_ref().unwrap().layout.positions();
    assert_eq!(&before, after, "frozen map must not move");
    assert!(h.state().map.as_ref().unwrap().layout.is_settled());
}

/// After settle + the 1s debounce, .junkdrawer/map.jd exists on disk.
#[test]
fn map_cache_file_written_after_settle() {
    let (vault, mut h, _ids) = app_with_seeds(five_note_fixture());
    let cache_path = vault.path().join(".junkdrawer").join("map.jd");
    to_map(&mut h);
    pump_settled(&mut h);
    common::pump(
        &mut h,
        &mut |_: &JdUi| cache_path.exists(),
        1000,
        "map cache debounce save",
    );
    assert!(cache_path.exists());
}

/// Restart (new JdUi over the same vault): the map loads the cached
/// positions and is born settled — loaded positions equal saved ones.
#[test]
fn map_restart_loads_saved_positions() {
    let vault = common::temp_vault();
    let cache_path = vault.path().join(".junkdrawer").join("map.jd");
    {
        let (mut h, _ids) = harness_over(&vault, five_note_fixture());
        to_map(&mut h);
        pump_settled(&mut h);
        common::pump(
            &mut h,
            &mut |_: &JdUi| cache_path.exists(),
            1000,
            "map cache debounce save",
        );
        // h (and its JdUi) drop here — Drop also flushes, but the file exists already.
    }
    let saved = MapCache::load(&Vault::open(vault.path()).unwrap());
    assert_eq!(
        saved.len(),
        3,
        "cache holds the 3 linked nodes only (orphans are ring-placed, not cached)"
    );

    // Restart: fresh JdUi over the same vault.
    let (mut h, _no_ids) = harness_over(&vault, vec![]);
    to_map(&mut h);
    common::pump(
        &mut h,
        &mut |a: &JdUi| a.map.is_some(),
        200,
        "map build on restart",
    );
    h.run_ok();
    let map = h.state().map.as_ref().unwrap();
    assert!(
        map.layout.is_settled(),
        "fully-cached map must be born settled (stable across sessions)"
    );
    for (id, saved_pos) in &saved {
        let loaded = map.layout.positions().get(id).copied().unwrap_or_else(|| {
            panic!("cached node {id} missing from restarted layout");
        });
        assert_eq!(
            loaded, *saved_pos,
            "restarted position must equal the saved one for {id}"
        );
    }
}

/// Restart with ONE extra linked note in the vault: the cached nodes stay
/// put (< 2px each, loaded vs post-build) while the newcomer joins the
/// layout locally — a partial cache must NOT re-cook the whole map.
#[test]
fn map_restart_with_newcomer_keeps_cached_nodes_put() {
    let vault = common::temp_vault();
    let cache_path = vault.path().join(".junkdrawer").join("map.jd");
    {
        let (mut h, _ids) = harness_over(&vault, five_note_fixture());
        to_map(&mut h);
        pump_settled(&mut h);
        common::pump(
            &mut h,
            &mut |_: &JdUi| cache_path.exists(),
            1000,
            "map cache debounce save",
        );
    }
    let saved = MapCache::load(&Vault::open(vault.path()).unwrap());
    assert_eq!(saved.len(), 3, "cache holds the 3 linked nodes");

    // Restart: fresh JdUi over the same vault, with ONE extra linked note.
    let (mut h, new_ids) = harness_over(&vault, vec![permanent("Zeta", "links to [[Alpha]]")]);
    let zeta = new_ids[0];
    to_map(&mut h);
    common::pump(
        &mut h,
        &mut |a: &JdUi| a.map.is_some(),
        200,
        "map build on restart",
    );
    pump_settled(&mut h);
    let _ = h.get_by_label("Map node: 'Zeta'"); // newcomer rendered
    let map = h.state().map.as_ref().unwrap();
    assert!(
        map.layout.positions().contains_key(&zeta),
        "newcomer must join the layout (it is linked, not an orphan)"
    );
    let (mut sum, mut max_drift) = (0.0f32, 0.0f32);
    for (id, saved_pos) in &saved {
        let now = map.layout.positions().get(id).copied().unwrap_or_else(|| {
            panic!("cached node {id} missing from restarted layout");
        });
        let d = dist(*saved_pos, now);
        sum += d;
        max_drift = max_drift.max(d);
        assert!(
            d < 2.0,
            "cached node {id} drifted {d} px (>= 2 px) after partial-cache rebuild"
        );
    }
    println!(
        "cached-node drift (loaded vs post-build+settle): mean {:.4} px, max {:.4} px",
        sum / saved.len() as f32,
        max_drift
    );
}

/// Orphans (degree 0) sit on a ring OUTSIDE the settled cluster: farther
/// from the cluster centroid than any linked node.
#[test]
fn map_orphans_ringed_outside_cluster() {
    let (_vault, mut h, ids) = app_with_seeds(five_note_fixture());
    to_map(&mut h);
    pump_settled(&mut h);
    let map = h.state().map.as_ref().unwrap();
    let linked = [ids[0], ids[1], ids[2]]; // Beta, Gamma, Alpha
    let orphans = [ids[3], ids[4]]; // Delta, Epsilon
    assert_eq!(
        {
            let mut o = map.orphans.clone();
            o.sort_unstable();
            o
        },
        {
            let mut o = orphans.to_vec();
            o.sort_unstable();
            o
        },
        "exactly Delta and Epsilon are orphans"
    );

    let positions = map.layout.positions();
    let centroid = {
        let mut c = Vec2::default();
        for id in &linked {
            let p = positions[id];
            c.x += p.x / linked.len() as f32;
            c.y += p.y / linked.len() as f32;
        }
        c
    };
    let max_linked = linked
        .iter()
        .map(|id| dist(positions[id], centroid))
        .fold(0.0f32, f32::max);
    let ring = jd_app::surfaces::map::orphan_ring_positions(positions, &map.orphans);
    assert_eq!(ring.len(), 2);
    for (id, p) in &ring {
        let d = dist(*p, centroid);
        assert!(
            d > max_linked,
            "orphan {id} at ring distance {d} must exceed the cluster's max radius {max_linked}"
        );
    }
}
