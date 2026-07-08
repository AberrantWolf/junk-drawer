//! Map layout performance budget (arch §2.15) — the tripwire that legally
//! activates the §3 snapshot escape hatch for the map. Release-mode only:
//! debug runs skip via ignore.
//!
//! MEASUREMENT DISCIPLINE: timings are environment-sensitive (sandbox tax,
//! background churn); CI's clean runners are the authoritative venue for the
//! budget. Run serially:
//!   cargo test -p jd-core --release --test maplayout_bench -- --test-threads=1 --include-ignored
//! Do not weaken the assertion based on a noisy local run.

use std::collections::HashMap;
use std::time::Instant;

use jd_core::geom::Vec2;
use jd_core::id::NoteId;
use jd_core::maplayout::{ForceLayout, LayoutParams};
use jd_core::rng::Xorshift128;

fn nid(i: u64) -> NoteId {
    let mut b = [0u8; 16];
    b[..8].copy_from_slice(&i.to_le_bytes());
    NoteId(b)
}

/// Reproducible synthetic graph: `nodes` nodes, `edges` xorshift-drawn edges
/// (self-loops re-rolled; duplicates allowed — ForceLayout dedups).
fn synthetic_graph(nodes: u64, edges: u64, seed: u64) -> (Vec<NoteId>, Vec<(NoteId, NoteId)>) {
    let ids: Vec<NoteId> = (0..nodes).map(nid).collect();
    let mut rng = Xorshift128::new(seed);
    let mut es = Vec::with_capacity(edges as usize);
    while (es.len() as u64) < edges {
        let a = rng.gen_range(0..nodes);
        let b = rng.gen_range(0..nodes);
        if a != b {
            es.push((nid(a), nid(b)));
        }
    }
    (ids, es)
}

#[test]
#[cfg_attr(debug_assertions, ignore = "perf budgets are release-mode only")]
fn one_step_at_twenty_k_under_sixteen_ms() {
    let (ids, edges) = synthetic_graph(20_000, 40_000, 0x4D41_5042); // "MAPB"
    let mut layout = ForceLayout::new(&ids, &edges, &HashMap::new(), LayoutParams::default());
    let start = Instant::now();
    let _ = layout.step(1.0 / 60.0);
    let elapsed = start.elapsed();
    // §2.15 budget: one step at 20k nodes / 40k edges must fit a frame.
    assert!(
        elapsed.as_millis() < 16,
        "one step at 20k/40k took {elapsed:?} (budget 16ms)"
    );
}

/// Non-gated sanity: a 1k-node graph actually settles (the map freezes).
#[test]
fn one_k_graph_settles() {
    let (ids, edges) = synthetic_graph(1_000, 2_000, 0x4D41_5053); // "MAPS"
    let mut layout = ForceLayout::new(&ids, &edges, &HashMap::new(), LayoutParams::default());
    let mut pinned = HashMap::new();
    pinned.insert(nid(0), Vec2 { x: 0.0, y: 0.0 });
    const MAX_STEPS: usize = 20_000;
    for i in 0..MAX_STEPS {
        layout.step(1.0 / 60.0);
        if layout.is_settled() {
            assert!(i > 0, "non-trivial graph must not settle instantly");
            return;
        }
    }
    panic!("1k-node graph did not settle within {MAX_STEPS} steps");
}
