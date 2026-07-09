//! Force-directed map layout (arch §2.15) + position cache.
//!
//! PURE math: no I/O (MapCache aside), no clocks — `dt` is passed in.
//! Determinism is a hard requirement (reproducible layouts):
//! - nodes are kept in a stable `Vec` order (sorted at construction,
//!   `add_node` appends) — the physics never iterates a HashMap;
//! - the only randomness is placement jitter, drawn from a per-node
//!   xorshift seeded by a FIXED constant mixed with the NoteId, so a
//!   node's jitter is a pure function of its id (order-independent);
//! - orphan/spiral placement is indexed by a deterministic counter.
//!
//! Settles then freezes: once `is_settled()`, `step` is a hard no-op
//! returning 0.0 until `add_node` unsettles the layout — a map, not a
//! lava lamp.
//!
//! The cache file is disposable machine state (missing/corrupt → empty),
//! mirroring session.jd's format and idioms.

use std::collections::HashMap;

use crate::error::IoError;
use crate::geom::Vec2;
use crate::id::NoteId;
use crate::rng::Xorshift128;
use crate::vault::Vault;
use crate::vault::io::atomic_save;

// ---------------------------------------------------------------------------
// Tuning constants (LayoutParams holds the pinned §2.15 knobs; these are the
// frozen structural choices around them)
// ---------------------------------------------------------------------------

/// Spring rest length (px). Pinned by the plan at ~120.
pub const REST_LENGTH: f32 = 120.0;

/// Repulsion grid cell ≈ 2× rest length; also the repulsion cutoff radius —
/// checking the 3×3 neighborhood therefore sees every pair within range.
const GRID_CELL: f32 = 2.0 * REST_LENGTH;

/// Hard speed clamp (px/s): bounds any single-step displacement to
/// `MAX_SPEED * dt` — pinned nodes drift smoothly, never teleport, and a
/// pathological force spike cannot fling a node across the map.
const MAX_SPEED: f32 = 4000.0;

/// Cooling schedule: initial per-step displacement cap and geometric decay.
/// The floor keeps a whisper of mobility until the settle check freezes the
/// layout; decay 0.995 gives ~1400 steps from 240 to 0.5 world units.
const INITIAL_TEMPERATURE: f32 = 240.0;
const COOLING: f32 = 0.995;

/// `add_node` reheat is LOCAL: only the newcomer's per-node temperature is
/// raised to this. EVERY existing node — direct neighbors included — stays
/// hard-frozen (temperature 0): the forces at the frozen positions still
/// saturate, so any nonzero cap lets nodes creep. Neighbors used to get the
/// temperature floor (`settle_eps / 2`) "to flex", but the floor never
/// decays to zero, so a direct neighbor accrued unbounded drift over the
/// newcomer's ease-in (measured 7.7 px on a 5-node graph, 9.2 px on the 1k
/// bench) — breaking the WP5 "stable across sessions" bound (< 2 px per
/// cached node). Frozen neighbors give exact 0 px drift; only the newcomer
/// moves. (Global reheat, for scale: mean 29.5 px / max 290 px of bulk
/// motion on the 1k/2k bench.)
const REHEAT_TEMPERATURE: f32 = 60.0;

/// Placement jitter half-range (px) around a neighbor centroid.
const JITTER: f32 = 60.0;

/// Spiral spacing (px): orphan k sits at radius `SPIRAL_SPACING * sqrt(k)`,
/// giving uniform areal density (~5 nodes per grid cell).
const SPIRAL_SPACING: f32 = 60.0;

/// Golden angle (rad) — successive spiral points never align.
const GOLDEN_ANGLE: f32 = 2.399_963;

/// FIXED seed for placement jitter; mixed with the NoteId per node.
const PLACEMENT_SEED: u64 = 0x4A44_4D41_5031; // "JDMAP1"

// ---------------------------------------------------------------------------
// LayoutParams
// ---------------------------------------------------------------------------

pub struct LayoutParams {
    pub spring_k: f32,
    pub repulsion: f32,
    pub damping: f32,
    pub settle_eps: f32,
}

impl Default for LayoutParams {
    /// Tuned here, then frozen (arch §2.15).
    fn default() -> Self {
        LayoutParams {
            spring_k: 40.0,
            repulsion: 800_000.0,
            damping: 0.85,
            settle_eps: 0.5,
        }
    }
}

// ---------------------------------------------------------------------------
// ForceLayout
// ---------------------------------------------------------------------------

pub struct ForceLayout {
    params: LayoutParams,
    /// Stable iteration order: sorted at construction; `add_node` appends.
    ids: Vec<NoteId>,
    index_of: HashMap<NoteId, usize>,
    /// Parallel to `ids`.
    pos: Vec<Vec2>,
    vel: Vec<(f32, f32)>,
    /// Normalized (lo, hi) index pairs, deduplicated.
    edges: Vec<(u32, u32)>,
    /// Mirror of `pos` keyed by id, kept in sync for `positions()`.
    positions: HashMap<NoteId, Vec2>,
    settled: bool,
    /// Fruchterman-Reingold-style cooling, PER NODE (parallel to `ids`):
    /// each node's per-step displacement is clamped to its temperature,
    /// which decays geometrically to a floor of `settle_eps / 2`. Guarantees
    /// termination even when spring forces permanently saturate the speed
    /// clamp (bang-bang oscillation around a minimum never settles
    /// otherwise). Note the temperature, not the speed clamp, governs the
    /// motion after ~256 steps (240 · 0.995^k < MAX_SPEED·dt ≈ 66.7 at
    /// k ≈ 256). Per-node (rather than global) so `add_node` can reheat
    /// just the newcomer while the settled bulk stays at the floor.
    temperature: Vec<f32>,
    /// Deterministic counter for spiral placement of orphans.
    spiral_count: u64,
}

impl ForceLayout {
    /// `pinned` = positions restored from the cache; they participate but keep
    /// spatial identity (they move like everyone else AFTER load; "pinned"
    /// means initial placement, not frozen).
    pub fn new(
        node_ids: &[NoteId],
        edges: &[(NoteId, NoteId)],
        pinned: &HashMap<NoteId, Vec2>,
        params: LayoutParams,
    ) -> Self {
        // Stable node order: sorted + deduped, independent of caller ordering.
        let mut ids = node_ids.to_vec();
        ids.sort_unstable();
        ids.dedup();
        let n = ids.len();
        let index_of: HashMap<NoteId, usize> =
            ids.iter().enumerate().map(|(i, id)| (*id, i)).collect();

        // Normalized (lo, hi) index pairs; self-loops and unknown ids dropped.
        let mut es: Vec<(u32, u32)> = edges
            .iter()
            .filter_map(|(a, b)| {
                let (i, j) = (*index_of.get(a)?, *index_of.get(b)?);
                (i != j).then(|| (i.min(j) as u32, i.max(j) as u32))
            })
            .collect();
        es.sort_unstable();
        es.dedup();

        // Adjacency for placement seeding.
        let mut adj: Vec<Vec<u32>> = vec![Vec::new(); n];
        for &(a, b) in &es {
            adj[a as usize].push(b);
            adj[b as usize].push(a);
        }

        // Placement. Pinned nodes take their cached position exactly and act
        // as anchors. Unpinned nodes (single pass, stable order): centroid of
        // already-ANCHORED neighbors + jitter; otherwise the spiral. Only
        // pinned/centroid-seeded nodes anchor — spiral nodes do not, so an
        // uncached graph spreads uniformly instead of recursively contracting
        // every centroid toward the origin (which would clump the whole graph
        // into a few repulsion-grid cells).
        let mut pos = vec![Vec2::default(); n];
        let mut anchored = vec![false; n];
        for (i, id) in ids.iter().enumerate() {
            if let Some(p) = pinned.get(id) {
                pos[i] = *p;
                anchored[i] = true;
            }
        }
        let mut spiral_count: u64 = 0;
        for i in 0..n {
            if anchored[i] {
                continue;
            }
            let (mut cx, mut cy, mut count) = (0.0f32, 0.0f32, 0u32);
            for &j in &adj[i] {
                if anchored[j as usize] {
                    cx += pos[j as usize].x;
                    cy += pos[j as usize].y;
                    count += 1;
                }
            }
            if count > 0 {
                let (jx, jy) = jitter(&ids[i]);
                pos[i] = Vec2 {
                    x: cx / count as f32 + jx,
                    y: cy / count as f32 + jy,
                };
                anchored[i] = true;
            } else {
                pos[i] = spiral(spiral_count);
                spiral_count += 1;
            }
        }

        let positions = ids.iter().copied().zip(pos.iter().copied()).collect();
        ForceLayout {
            params,
            ids,
            index_of,
            pos,
            vel: vec![(0.0, 0.0); n],
            edges: es,
            positions,
            settled: n == 0,
            temperature: vec![INITIAL_TEMPERATURE; n],
            spiral_count,
        }
    }

    /// One physics step; returns max node displacement (px). Hard no-op (0.0)
    /// once settled — the map freezes until `add_node` wakes it.
    pub fn step(&mut self, dt: f32) -> f32 {
        if self.settled {
            return 0.0;
        }
        let n = self.ids.len();
        let mut force = vec![(0.0f32, 0.0f32); n];

        // Springs along edges (stable edge order).
        for &(a, b) in &self.edges {
            let (a, b) = (a as usize, b as usize);
            let dx = self.pos[b].x - self.pos[a].x;
            let dy = self.pos[b].y - self.pos[a].y;
            let d = (dx * dx + dy * dy).sqrt().max(1e-3);
            let f = self.params.spring_k * (d - REST_LENGTH) / d;
            force[a].0 += f * dx;
            force[a].1 += f * dy;
            force[b].0 -= f * dx;
            force[b].1 -= f * dy;
        }

        // Grid-bucketed repulsion: nodes bucketed by cell; each node scans its
        // 3×3 neighborhood (cell == cutoff, so no pair in range is missed).
        // Buckets fill in stable node order and cells are looked up (never
        // iterated), so accumulation order is deterministic.
        let inv_cell = 1.0 / GRID_CELL;
        let cell_of = |p: Vec2| {
            (
                (p.x * inv_cell).floor() as i32,
                (p.y * inv_cell).floor() as i32,
            )
        };
        let mut grid: HashMap<(i32, i32), Vec<u32>> = HashMap::new();
        for (i, p) in self.pos.iter().enumerate() {
            grid.entry(cell_of(*p)).or_default().push(i as u32);
        }
        let cutoff2 = GRID_CELL * GRID_CELL;
        // Index loop: `i` addresses two parallel arrays (`pos`, `force`) and
        // is compared against bucket entries — iterator form is noisier.
        #[allow(clippy::needless_range_loop)]
        for i in 0..n {
            let p = self.pos[i];
            let (cx, cy) = cell_of(p);
            let (mut fx, mut fy) = (0.0f32, 0.0f32);
            for dcy in -1..=1 {
                for dcx in -1..=1 {
                    let Some(bucket) = grid.get(&(cx + dcx, cy + dcy)) else {
                        continue;
                    };
                    for &j in bucket {
                        let j = j as usize;
                        if j == i {
                            continue;
                        }
                        let dx = p.x - self.pos[j].x;
                        let dy = p.y - self.pos[j].y;
                        let d2 = dx * dx + dy * dy;
                        if d2 > cutoff2 {
                            continue;
                        }
                        if d2 < 1e-6 {
                            // Coincident: deterministic push, angle from index.
                            let a = i as f32 * GOLDEN_ANGLE;
                            fx += self.params.repulsion * a.cos();
                            fy += self.params.repulsion * a.sin();
                        } else {
                            let d2 = d2.max(1.0);
                            let f = self.params.repulsion / (d2 * d2.sqrt());
                            fx += f * dx;
                            fy += f * dy;
                        }
                    }
                }
            }
            force[i].0 += fx;
            force[i].1 += fy;
        }

        // Integrate (semi-implicit Euler, damped, speed-clamped).
        let mut max_disp = 0.0f32;
        // Index loop: `i` addresses three parallel arrays (`vel`, `force`, `pos`).
        #[allow(clippy::needless_range_loop)]
        for i in 0..n {
            let mut vx = (self.vel[i].0 + force[i].0 * dt) * self.params.damping;
            let mut vy = (self.vel[i].1 + force[i].1 * dt) * self.params.damping;
            let speed2 = vx * vx + vy * vy;
            if speed2 > MAX_SPEED * MAX_SPEED {
                let s = MAX_SPEED / speed2.sqrt();
                vx *= s;
                vy *= s;
            }
            self.vel[i] = (vx, vy);
            let (mut sx, mut sy) = (vx * dt, vy * dt);
            // Per-node temperature clamp (see field docs): bounds this
            // node's step, then decays toward the floor.
            let t = self.temperature[i];
            let step2 = sx * sx + sy * sy;
            if step2 > t * t {
                let k = t / step2.sqrt();
                sx *= k;
                sy *= k;
            }
            // Decay toward the floor; a hard-frozen node (t == 0, set by the
            // local reheat in `add_node`) stays frozen — max() must not lift
            // it back to the floor.
            if t > 0.0 {
                self.temperature[i] = (t * COOLING).max(self.params.settle_eps * 0.5);
            }
            self.pos[i].x += sx;
            self.pos[i].y += sy;
            max_disp = max_disp.max((sx * sx + sy * sy).sqrt());
        }
        if max_disp < self.params.settle_eps {
            self.settled = true;
            // A frozen layout carries no energy: velocities integrated
            // forces UNCLAMPED while the temperature capped visible motion,
            // so without this a later reheat would release the stored
            // momentum as a bulk lurch.
            self.vel.fill((0.0, 0.0));
        }
        for (id, p) in self.ids.iter().zip(self.pos.iter()) {
            self.positions.insert(*id, *p);
        }
        max_disp
    }

    /// Max displacement < settle_eps → freeze.
    pub fn is_settled(&self) -> bool {
        self.settled
    }

    pub fn positions(&self) -> &HashMap<NoteId, Vec2> {
        &self.positions
    }

    /// Seed near the centroid of its already-present neighbors (+ jitter);
    /// orphans continue the spiral. Marks the layout unsettled.
    pub fn add_node(&mut self, id: NoteId, edges: &[NoteId]) {
        if !self.index_of.contains_key(&id) {
            let (mut cx, mut cy, mut count) = (0.0f32, 0.0f32, 0u32);
            for e in edges {
                if let Some(&j) = self.index_of.get(e) {
                    cx += self.pos[j].x;
                    cy += self.pos[j].y;
                    count += 1;
                }
            }
            let p = if count > 0 {
                let (jx, jy) = jitter(&id);
                Vec2 {
                    x: cx / count as f32 + jx,
                    y: cy / count as f32 + jy,
                }
            } else {
                let p = spiral(self.spiral_count);
                self.spiral_count += 1;
                p
            };
            self.index_of.insert(id, self.ids.len());
            self.ids.push(id);
            self.pos.push(p);
            self.vel.push((0.0, 0.0));
            self.temperature.push(REHEAT_TEMPERATURE);
            self.positions.insert(id, p);
        }
        let i = self.index_of[&id];
        for e in edges {
            if let Some(&j) = self.index_of.get(e)
                && i != j
            {
                let pair = (i.min(j) as u32, i.max(j) as u32);
                if !self.edges.contains(&pair) {
                    self.edges.push(pair);
                }
            }
        }
        // LOCAL reheat (see REHEAT docs). Waking a FROZEN layout: every
        // existing node — direct neighbors included — stays hard-frozen
        // (temperature 0; a floored neighbor would creep unboundedly, see
        // REHEAT docs), and everything starts cold (defense in depth
        // alongside the zeroing at settle: a reheat must release no stored
        // energy). A still-running layout keeps its temperatures and
        // momentum. Either way the newcomer gets at least
        // REHEAT_TEMPERATURE to ease in.
        if self.settled {
            self.temperature.fill(0.0);
            self.vel.fill((0.0, 0.0));
        }
        self.temperature[i] = self.temperature[i].max(REHEAT_TEMPERATURE);
        self.settled = false;
    }
}

/// Per-node placement jitter: a pure function of the NoteId (fixed seed mixed
/// with the id bytes), so placement is order-independent and reproducible.
fn jitter(id: &NoteId) -> (f32, f32) {
    let hi = u64::from_le_bytes(id.0[0..8].try_into().unwrap());
    let lo = u64::from_le_bytes(id.0[8..16].try_into().unwrap());
    let mut rng = Xorshift128::new(PLACEMENT_SEED ^ hi ^ lo.rotate_left(32));
    let draw = |rng: &mut Xorshift128| (rng.gen_range(0..2001) as f32 / 1000.0 - 1.0) * JITTER;
    (draw(&mut rng), draw(&mut rng))
}

/// Deterministic sunflower spiral: point k at radius `SPIRAL_SPACING·√k`,
/// angle `k·golden` — uniform areal density, no alignments.
fn spiral(k: u64) -> Vec2 {
    let r = SPIRAL_SPACING * (k as f32).sqrt();
    let a = k as f32 * GOLDEN_ANGLE;
    Vec2 {
        x: r * a.cos(),
        y: r * a.sin(),
    }
}

// ---------------------------------------------------------------------------
// MapCache — .junkdrawer/map.jd (mirrors session.rs: disposable line format)
// ---------------------------------------------------------------------------

pub struct MapCache;

fn map_path(vault: &Vault) -> std::path::PathBuf {
    vault.abs(std::path::Path::new(".junkdrawer/map.jd"))
}

/// Entries sorted by NoteId so the file is byte-stable for identical inputs.
fn serialise(positions: &HashMap<NoteId, Vec2>) -> String {
    let mut out = String::new();
    out.push_str("jd-map 1\n");
    let mut entries: Vec<(&NoteId, &Vec2)> = positions.iter().collect();
    entries.sort_unstable_by_key(|(id, _)| **id);
    for (id, pos) in entries {
        out.push_str(&format!("node = {} {} {}\n", id, pos.x, pos.y));
    }
    out
}

/// Lenient-all-or-nothing: any malformed line → return None (→ empty).
fn parse(text: &str) -> Option<HashMap<NoteId, Vec2>> {
    let mut lines = text.lines();

    // Header check
    let header = lines.next()?;
    if header.trim() != "jd-map 1" {
        return None;
    }

    let mut positions = HashMap::new();
    for line in lines {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        // Key = value
        let (key, value) = if let Some(pos) = line.find('=') {
            (line[..pos].trim(), line[pos + 1..].trim())
        } else {
            return None; // malformed line
        };

        match key {
            "node" => {
                let mut parts = value.split_ascii_whitespace();
                let note_id = NoteId::parse(parts.next()?).ok()?;
                let x: f32 = parts.next()?.parse().ok()?;
                let y: f32 = parts.next()?.parse().ok()?;
                positions.insert(note_id, Vec2 { x, y });
            }
            _ => return None, // unknown key = malformed
        }
    }
    Some(positions)
}

impl MapCache {
    /// Load from `.junkdrawer/map.jd`.
    /// Missing or corrupt file → empty map, never an error (disposable).
    pub fn load(vault: &Vault) -> HashMap<NoteId, Vec2> {
        let path = map_path(vault);
        let text = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            Err(_) => return HashMap::new(),
        };
        parse(&text).unwrap_or_default()
    }

    /// Atomically save to `.junkdrawer/map.jd`.
    pub fn save(vault: &Vault, positions: &HashMap<NoteId, Vec2>) -> Result<(), IoError> {
        atomic_save(&map_path(vault), &serialise(positions))
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn nid(n: u8) -> NoteId {
        NoteId([n; 16])
    }

    fn dist(a: Vec2, b: Vec2) -> f32 {
        let (dx, dy) = (a.x - b.x, a.y - b.y);
        (dx * dx + dy * dy).sqrt()
    }

    const DT: f32 = 1.0 / 60.0;

    fn settle(l: &mut ForceLayout, max_steps: usize) -> usize {
        for i in 0..max_steps {
            l.step(DT);
            if l.is_settled() {
                return i + 1;
            }
        }
        panic!("layout did not settle within {max_steps} steps");
    }

    #[test]
    fn linked_pair_ends_closer_than_unlinked() {
        let ids = [nid(1), nid(2), nid(3), nid(4)];
        let edges = [(nid(1), nid(2))];
        let mut l = ForceLayout::new(&ids, &edges, &HashMap::new(), LayoutParams::default());
        settle(&mut l, 5000);
        let p = l.positions();
        let linked = dist(p[&nid(1)], p[&nid(2)]);
        let unlinked = dist(p[&nid(3)], p[&nid(4)]);
        assert!(
            linked < unlinked,
            "linked pair ({linked}) must end closer than unlinked ({unlinked})"
        );
    }

    #[test]
    fn triangle_settles_and_freezes() {
        let ids = [nid(1), nid(2), nid(3)];
        let edges = [(nid(1), nid(2)), (nid(2), nid(3)), (nid(3), nid(1))];
        let mut l = ForceLayout::new(&ids, &edges, &HashMap::new(), LayoutParams::default());
        assert!(!l.is_settled(), "non-trivial graph starts unsettled");
        settle(&mut l, 5000);
        assert!(l.is_settled());
        // Frozen: further steps move nothing and stay settled.
        let before = l.positions().clone();
        for _ in 0..10 {
            let d = l.step(DT);
            assert!(d.abs() < 1e-6, "step after settle must return ~0, got {d}");
            assert!(l.is_settled(), "settled must STAY true");
        }
        assert_eq!(&before, l.positions(), "frozen positions must not move");
    }

    #[test]
    fn pinned_nodes_start_exact_and_never_teleport() {
        let ids = [nid(1), nid(2), nid(3)];
        let edges = [(nid(1), nid(2)), (nid(2), nid(3))];
        let mut pinned = HashMap::new();
        pinned.insert(
            nid(1),
            Vec2 {
                x: 500.0,
                y: -250.0,
            },
        );
        pinned.insert(
            nid(2),
            Vec2 {
                x: 610.0,
                y: -250.0,
            },
        );
        let mut l = ForceLayout::new(&ids, &edges, &pinned, LayoutParams::default());
        // Step 0: pinned nodes at their exact cached positions.
        assert_eq!(
            l.positions()[&nid(1)],
            Vec2 {
                x: 500.0,
                y: -250.0
            }
        );
        assert_eq!(
            l.positions()[&nid(2)],
            Vec2 {
                x: 610.0,
                y: -250.0
            }
        );
        // First step: displacement bounded by the speed clamp (no teleport).
        let before = l.positions().clone();
        l.step(DT);
        let bound = MAX_SPEED * DT + 1e-3;
        for id in [nid(1), nid(2)] {
            let d = dist(before[&id], l.positions()[&id]);
            assert!(d <= bound, "pinned node moved {d} > bound {bound}");
        }
    }

    #[test]
    fn identical_inputs_give_identical_layouts() {
        let ids: Vec<NoteId> = (1..=20).map(nid).collect();
        let edges: Vec<(NoteId, NoteId)> = (1..20).map(|i| (nid(i), nid(i + 1))).collect();
        let mut pinned = HashMap::new();
        pinned.insert(nid(5), Vec2 { x: 42.0, y: 7.0 });
        let mut a = ForceLayout::new(&ids, &edges, &pinned, LayoutParams::default());
        let mut b = ForceLayout::new(&ids, &edges, &pinned, LayoutParams::default());
        for _ in 0..50 {
            let da = a.step(DT);
            let db = b.step(DT);
            assert_eq!(da, db, "step displacements must match bitwise");
        }
        assert_eq!(a.positions(), b.positions(), "positions must match bitwise");
    }

    #[test]
    fn add_node_seeds_near_neighbor_centroid() {
        let ids = [nid(1), nid(2)];
        let mut pinned = HashMap::new();
        pinned.insert(nid(1), Vec2 { x: 0.0, y: 0.0 });
        pinned.insert(nid(2), Vec2 { x: 200.0, y: 0.0 });
        let mut l = ForceLayout::new(&ids, &[], &pinned, LayoutParams::default());
        l.step(DT); // let it settle or not; we only care about the seed below
        let centroid = {
            let p = l.positions();
            Vec2 {
                x: (p[&nid(1)].x + p[&nid(2)].x) / 2.0,
                y: (p[&nid(1)].y + p[&nid(2)].y) / 2.0,
            }
        };
        l.add_node(nid(3), &[nid(1), nid(2)]);
        let seeded = l.positions()[&nid(3)];
        let r = dist(seeded, centroid);
        // Jitter is ±JITTER per axis → max radius JITTER·√2.
        assert!(
            r <= JITTER * std::f32::consts::SQRT_2 + 1e-3,
            "seed {r} px from centroid (jitter bound {})",
            JITTER * std::f32::consts::SQRT_2
        );
        assert!(!l.is_settled(), "add_node must unsettle the layout");
    }

    #[test]
    fn cache_round_trips_bytes_and_positions() {
        let t = crate::vault::testutil::TempDir::new();
        let vault = Vault::open(t.path()).unwrap();
        let mut positions = HashMap::new();
        positions.insert(nid(1), Vec2 { x: 10.5, y: -20.25 });
        positions.insert(nid(2), Vec2 { x: 0.0, y: 300.0 });
        positions.insert(nid(9), Vec2 { x: -1.75, y: 2.5 });
        MapCache::save(&vault, &positions).unwrap();
        let bytes_a = std::fs::read(map_path(&vault)).unwrap();
        assert!(bytes_a.starts_with(b"jd-map 1\n"), "header line");
        let loaded = MapCache::load(&vault);
        assert_eq!(loaded, positions);
        // Byte-stable: saving the loaded map reproduces the same file.
        MapCache::save(&vault, &loaded).unwrap();
        let bytes_b = std::fs::read(map_path(&vault)).unwrap();
        assert_eq!(bytes_a, bytes_b, "cache serialisation must be byte-stable");
    }

    #[test]
    fn cache_missing_or_corrupt_loads_empty() {
        let t = crate::vault::testutil::TempDir::new();
        let vault = Vault::open(t.path()).unwrap();
        assert!(MapCache::load(&vault).is_empty(), "missing → empty");
        std::fs::write(map_path(&vault), "garbage ]]] \0\n").unwrap();
        assert!(MapCache::load(&vault).is_empty(), "corrupt → empty");
        std::fs::write(map_path(&vault), "jd-map 1\nnode = not-a-ulid 1 2\n").unwrap();
        assert!(MapCache::load(&vault).is_empty(), "bad line → empty");
    }
}
