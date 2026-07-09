//! The Map surface (WP5): every card in the vault as a force-directed graph
//! of dots and lines — settles then freezes, stable across sessions.
//!
//! Follows the established surface conventions:
//! - events-out: `map_ui` renders and returns `MapEvent`s; app.rs is the one
//!   mutation site (camera lives on `MapState`, applied there).
//! - ONE index read-lock snapshot per frame: `MapNodeMeta`s are prefetched in
//!   app.rs (the FaceMeta idiom) and passed in.
//! - Edges are RESOLVED links only (both directions collapse to one edge).
//!
//! ORPHANS (degree 0) are kept OUT of the ForceLayout entirely: the physics
//! would just scatter them, so instead the surface renders them on a
//! deterministic ring (cluster bounding radius + margin, angle hashed from
//! the NoteId) recomputed each frame from the current positions. Findable,
//! not shamed — and their cache entries would be redundant (the ring is a
//! pure function of the cluster), so they are never cached.
//!
//! Camera: `DeskCamera` is desk-agnostic math (world↔screen + fit), so it is
//! re-exported here rather than duplicated. Pan/zoom bindings are identical
//! to the desk. The camera is NOT persisted: positions are the stable thing;
//! the map recenters via zoom-to-fit on first build each session (cheap and
//! always orienting).
//!
//! Gate matrix note for Task 3 (interactions): this task renders only — the
//! sole input is camera pan/zoom, which is never journaled and (matching the
//! desk's gate matrix, where scroll pan/zoom is ungated) runs regardless of
//! palette/editor/confirm. Any FUTURE mouse mutation (click-select, Ctrl+D
//! take-to-desk, context menus) must be gated on palette_open + editor_open +
//! confirm_pending from day one, exactly like the desk's card paths.

use std::collections::{HashMap, HashSet};
use std::time::Instant;

use eframe::egui;
use jd_core::geom::Vec2;
use jd_core::id::NoteId;
use jd_core::index::Index;
use jd_core::maplayout::{ForceLayout, LayoutParams};
use jd_core::note::{Kind, Status};

// DeskCamera is pure world↔screen math with no desk state — shared, not moved,
// so the desk keeps its own module-local usage untouched.
pub use crate::surfaces::desk::DeskCamera;
use crate::surfaces::desk::{ZOOM_MAX, ZOOM_MIN};

// ---------------------------------------------------------------------------
// Tuning constants
// ---------------------------------------------------------------------------

/// Orphan ring sits this far outside the settled cluster's bounding radius.
pub const ORPHAN_RING_MARGIN: f32 = 200.0;

/// New-node ease-in duration (opacity 0→1). Instant-based, render-only;
/// under reduced_motion nodes appear at full opacity immediately.
pub const FADE_IN_SECS: f32 = 0.4;

/// Physics budget per frame while unsettled: up to this many fixed-dt steps
/// OR `STEP_BUDGET_MS`, whichever is hit first. 4 × step(1/60) keeps the
/// settle visibly quick (~4× real time) while one 20k-node step is <16 ms
/// (the jd-core bench), so the wall-clock guard is what protects huge vaults.
pub const MAX_STEPS_PER_FRAME: usize = 4;
pub const STEP_BUDGET_MS: u64 = 8;
pub const STEP_DT: f32 = 1.0 / 60.0;

/// Dot radius law: 4 + 2·√degree, clamped 4..12 — gentle, sublinear growth
/// so hubs read as hubs without dwarfing the field. Dividers get +2 on top
/// (they are landmarks by design).
pub const NODE_R_MIN: f32 = 4.0;
pub const NODE_R_MAX: f32 = 12.0;
pub const DIVIDER_R_BONUS: f32 = 2.0;

/// Minimum on-screen radius so far-out zooms keep dots visible/hoverable.
const SCREEN_R_MIN: f32 = 2.0;

pub fn node_radius(degree: usize, is_divider: bool) -> f32 {
    let r = (4.0 + 2.0 * (degree as f32).sqrt()).clamp(NODE_R_MIN, NODE_R_MAX);
    if is_divider { r + DIVIDER_R_BONUS } else { r }
}

// ---------------------------------------------------------------------------
// MapState — owned by JdUi (app-side; kittests read it via harness.state())
// ---------------------------------------------------------------------------

pub struct MapState {
    /// Physics for LINKED nodes only (orphans are ring-rendered, see module docs).
    pub layout: ForceLayout,
    /// Normalized (lo, hi) resolved-link edges between linked nodes.
    pub edges: Vec<(NoteId, NoteId)>,
    /// Degree-0 notes, rendered on the deterministic ring.
    pub orphans: Vec<NoteId>,
    /// Distinct-neighbor counts (drives dot radius).
    pub degrees: HashMap<NoteId, usize>,
    /// Stable render/a11y order: sorted at build, appended by `add_note`.
    pub nodes: Vec<NoteId>,
    /// O(1) membership for `nodes`.
    known: HashSet<NoteId>,
    /// Last observed settle state; the false→true transition arms the
    /// debounced cache save (`cache_dirty_at`) exactly once per settle.
    pub settled: bool,
    /// Debounced-save timestamp (the session_dirty_at pattern): set on
    /// settle, cleared by the save in app.rs (1s debounce / surface-leave /
    /// Drop).
    pub cache_dirty_at: Option<Instant>,
    /// Not persisted — zoom-to-fit on build orients each session.
    pub camera: DeskCamera,
    /// Ease-in clocks for nodes added while the map exists (render-only).
    pub appeared: HashMap<NoteId, Instant>,
}

impl MapState {
    /// Build from the whole index: nodes = all indexed notes, edges =
    /// resolved links only, pinned = the cached positions. Orphans are
    /// partitioned OUT of the layout. Camera zoom-to-fits the initial
    /// placement (pinned/seeded positions + orphan ring).
    pub fn build(idx: &Index, pinned: HashMap<NoteId, Vec2>, panel: egui::Rect) -> MapState {
        let mut nodes: Vec<NoteId> = idx.iter_meta().map(|m| m.id).collect();
        nodes.sort_unstable();

        // Resolved links only; both directions collapse via the normalized
        // (min, max) pair (the selected_edges dedup idiom).
        let mut edge_set: std::collections::BTreeSet<(NoteId, NoteId)> =
            std::collections::BTreeSet::new();
        for &id in &nodes {
            for (_, resolved) in idx.outlinks(id) {
                if let Some(t) = resolved
                    && t != id
                {
                    edge_set.insert((id.min(t), id.max(t)));
                }
            }
        }
        let edges: Vec<(NoteId, NoteId)> = edge_set.into_iter().collect();

        let mut degrees: HashMap<NoteId, usize> = HashMap::new();
        for (a, b) in &edges {
            *degrees.entry(*a).or_default() += 1;
            *degrees.entry(*b).or_default() += 1;
        }

        let (linked, orphans): (Vec<NoteId>, Vec<NoteId>) = nodes
            .iter()
            .copied()
            .partition(|id| degrees.get(id).copied().unwrap_or(0) > 0);

        // Stable across sessions: whenever ANY linked node has a cached
        // position, build the layout from the CACHED nodes (+ the edges among
        // them) only, freeze it with one zero-dt step (max displacement 0 <
        // settle_eps trips the layout's settle path), then `add_node` each
        // uncached newcomer in sorted order. add_node's reheat is LOCAL, so
        // the cached bulk stays hard-frozen while newcomers ease in near
        // their neighbor centroids — versus the old whole-graph rebuild,
        // where ONE newcomer gave EVERY node INITIAL_TEMPERATURE (measured
        // bulk drift mean 22.1 px / max 77.6 px on a 60-node graph, vs
        // 0.04 px mean with this path). add_node silently drops edges to
        // not-yet-present ids, but sorted insertion covers every
        // newcomer↔newcomer edge: each pair (A, B) is wired when the LATER
        // of the two is added, since its neighbor list includes the earlier.
        let cached: Vec<NoteId> = linked
            .iter()
            .copied()
            .filter(|id| pinned.contains_key(id))
            .collect();
        let layout = if cached.is_empty() {
            // No usable cache (first session / fully stale): cold build,
            // everything at INITIAL_TEMPERATURE.
            ForceLayout::new(&linked, &edges, &pinned, LayoutParams::default())
        } else {
            let cached_set: HashSet<NoteId> = cached.iter().copied().collect();
            let cached_edges: Vec<(NoteId, NoteId)> = edges
                .iter()
                .copied()
                .filter(|(a, b)| cached_set.contains(a) && cached_set.contains(b))
                .collect();
            let mut layout =
                ForceLayout::new(&cached, &cached_edges, &pinned, LayoutParams::default());
            layout.step(0.0); // freeze the cached bulk (born settled)
            let mut adj: HashMap<NoteId, Vec<NoteId>> = HashMap::new();
            for (a, b) in &edges {
                adj.entry(*a).or_default().push(*b);
                adj.entry(*b).or_default().push(*a);
            }
            // Sorted order (`linked` is sorted; filter preserves order).
            for id in linked.iter().filter(|id| !cached_set.contains(*id)) {
                let ns = adj.get(id).map(Vec::as_slice).unwrap_or(&[]);
                layout.add_node(*id, ns);
            }
            layout
        };
        let settled = layout.is_settled();

        // Zoom-to-fit on first build (per-session orientation; camera is not
        // persisted). zoom_to_fit pads by a nominal card extent — generous
        // for dots, which just means a slightly wider margin. Fine.
        let mut camera = DeskCamera {
            center: egui::Vec2::ZERO,
            zoom: 1.0,
        };
        let mut fit: Vec<(NoteId, Vec2)> =
            layout.positions().iter().map(|(id, p)| (*id, *p)).collect();
        fit.extend(orphan_ring_positions(layout.positions(), &orphans));
        camera.zoom_to_fit(&fit, panel);

        let known: HashSet<NoteId> = nodes.iter().copied().collect();
        MapState {
            layout,
            edges,
            orphans,
            degrees,
            nodes,
            known,
            settled,
            cache_dirty_at: None,
            camera,
            appeared: HashMap::new(),
        }
    }

    /// Track a note that appeared while the map exists (OpDone created /
    /// External changed with an id the layout doesn't know). Linked notes
    /// join the physics via `add_node` (local reheat, neighbor-centroid
    /// seed); degree-0 notes join the orphan ring. Either way the node
    /// eases in (opacity 0→1 over ~400ms) unless reduced_motion, in which
    /// case it appears at full opacity immediately ("appear settled").
    pub fn add_note(&mut self, id: NoteId, idx: &Index, reduced_motion: bool) {
        if self.known.contains(&id) || idx.get(id).is_none() {
            return;
        }
        // Resolved neighbors in BOTH directions, restricted to nodes already
        // in the layout (add_node ignores unknown ids). KNOWN GAP (Task 3+
        // scope): a new note linked only to orphans is classified an orphan
        // here and the edge is LOST for this session — when the orphan
        // neighbor later re-indexes, its event hits the known-id guard above
        // and returns, so nothing re-wires. The next full rebuild
        // (MapState::build) heals it.
        let mut neighbors: std::collections::BTreeSet<NoteId> = std::collections::BTreeSet::new();
        for (_, resolved) in idx.outlinks(id) {
            if let Some(t) = resolved
                && t != id
                && self.layout.positions().contains_key(&t)
            {
                neighbors.insert(t);
            }
        }
        for b in idx.backlinks(id) {
            if b != id && self.layout.positions().contains_key(&b) {
                neighbors.insert(b);
            }
        }
        if neighbors.is_empty() {
            self.orphans.push(id);
        } else {
            let ns: Vec<NoteId> = neighbors.iter().copied().collect();
            self.layout.add_node(id, &ns);
            for n in &ns {
                self.edges.push((id.min(*n), id.max(*n)));
                *self.degrees.entry(*n).or_default() += 1;
            }
            self.degrees.insert(id, ns.len());
            self.settled = false;
            // Disarm any pending debounced save: positions are about to
            // ease, and flushing a mid-ease snapshot would cache transient
            // positions. Re-armed by the next settle transition in app.rs.
            self.cache_dirty_at = None;
        }
        self.nodes.push(id);
        self.known.insert(id);
        if !reduced_motion {
            self.appeared.insert(id, Instant::now());
        }
    }
}

// ---------------------------------------------------------------------------
// Orphan ring — pure and deterministic (the kittests' testable seam)
// ---------------------------------------------------------------------------

/// Ring positions for the orphans: radius = the linked cluster's bounding
/// radius (max distance from its centroid) + `ORPHAN_RING_MARGIN`; angle a
/// pure hash of the NoteId, so a given orphan always sits at the same
/// bearing. Recomputed each frame from the live positions (cheap: O(n)),
/// which keeps the ring hugging the cluster while it settles. With no
/// linked nodes at all, the ring circles the origin.
pub fn orphan_ring_positions(
    positions: &HashMap<NoteId, Vec2>,
    orphans: &[NoteId],
) -> Vec<(NoteId, Vec2)> {
    let mut centroid = Vec2::default();
    if !positions.is_empty() {
        // Sum in sorted-id order: float addition is not associative, so
        // iterating the HashMap directly would make the centroid (and thus
        // the ring) sub-pixel nondeterministic across runs.
        let mut sorted: Vec<(&NoteId, &Vec2)> = positions.iter().collect();
        sorted.sort_unstable_by_key(|(id, _)| **id);
        let n = positions.len() as f32;
        for (_, p) in sorted {
            centroid.x += p.x / n;
            centroid.y += p.y / n;
        }
    }
    let bound = positions
        .values()
        .map(|p| {
            let (dx, dy) = (p.x - centroid.x, p.y - centroid.y);
            (dx * dx + dy * dy).sqrt()
        })
        .fold(0.0f32, f32::max);
    let r = bound + ORPHAN_RING_MARGIN;
    orphans
        .iter()
        .map(|id| {
            let a = orphan_angle(id);
            (
                *id,
                Vec2 {
                    x: centroid.x + r * a.cos(),
                    y: centroid.y + r * a.sin(),
                },
            )
        })
        .collect()
}

/// Deterministic bearing from the NoteId bytes (splitmix64-style mix so
/// consecutive ULIDs don't bunch up on the ring).
fn orphan_angle(id: &NoteId) -> f32 {
    let hi = u64::from_le_bytes(id.0[0..8].try_into().unwrap());
    let lo = u64::from_le_bytes(id.0[8..16].try_into().unwrap());
    // Fold hi and lo SEQUENTIALLY (mix, xor, mix) rather than xor-first:
    // a plain `hi ^ lo` cancels to a constant for symmetric byte patterns.
    let mix = |mut z: u64| {
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    };
    let z = mix(mix(hi) ^ lo);
    (z as f64 / u64::MAX as f64) as f32 * std::f32::consts::TAU
}

// ---------------------------------------------------------------------------
// Per-node metadata (prefetched in app.rs under the frame's ONE read lock)
// ---------------------------------------------------------------------------

pub struct MapNodeMeta {
    pub id: NoteId,
    /// Title, or first line for untitled scraps (a11y label + tooltip).
    pub label: String,
    pub status: Status,
    pub kind: Kind,
}

pub struct MapUiDeps<'a> {
    pub theme: &'a crate::theme::Theme,
    /// Prefetched in `nodes` order (notes deleted since build simply drop out).
    pub metas: &'a [MapNodeMeta],
}

// ---------------------------------------------------------------------------
// MapEvent
// ---------------------------------------------------------------------------

/// Events emitted by `map_ui` for app.rs to apply (the events-out pattern;
/// map_ui never mutates MapState).
#[derive(Debug)]
pub enum MapEvent {
    /// Camera moved/zoomed — NOT journaled, NOT persisted; app.rs writes it
    /// back onto MapState.
    ViewportMoved { cam: DeskCamera },
}

// ---------------------------------------------------------------------------
// map_ui
// ---------------------------------------------------------------------------

/// Render the map; returns events for app.rs to apply.
///
/// Dot tints are EXISTING theme fields only (visual-language mapping, no new
/// colors): divider (Kind::Structure) = divider_tab_bg fill (a landmark, +2
/// radius); literature = rule_ink (slate, reads "referenced material");
/// scrap (Fleeting) = text_weak (muted — not yet part of the web); permanent
/// note = accent. Every dot carries a 1px card_border stroke: card_border is
/// already WCAG-checked ≥3:1 against desk_bg (see theme.rs), so even the
/// light fills (divider_tab_bg) keep a compliant boundary — same discipline
/// as card outlines. Edges are thin card_border lines beneath the nodes.
pub fn map_ui(ui: &mut egui::Ui, state: &MapState, deps: &MapUiDeps<'_>) -> Vec<MapEvent> {
    let mut events: Vec<MapEvent> = Vec::new();
    let panel = ui.max_rect();
    let mut cam = state.camera;

    // ------------------------------------------------------------------
    // Pan and zoom — identical bindings to the desk (scroll pan, Shift for
    // horizontal, Ctrl+scroll zoom with the spec 1.0015^delta formula,
    // middle-drag pan). Camera-only: never journaled (see module docs).
    // ------------------------------------------------------------------
    let scroll = ui.input(|i| i.smooth_scroll_delta);
    let panel_h = panel.height();
    let ctrl_scroll_delta = ui.input(|i| {
        i.events
            .iter()
            .filter_map(|ev| {
                if let egui::Event::MouseWheel {
                    delta,
                    modifiers,
                    unit,
                    ..
                } = ev
                    && modifiers.command
                {
                    let y = match unit {
                        egui::MouseWheelUnit::Point => delta.y,
                        egui::MouseWheelUnit::Line => delta.y * 40.0,
                        egui::MouseWheelUnit::Page => delta.y * panel_h,
                    };
                    return Some(y);
                }
                None
            })
            .sum::<f32>()
    });
    let mut viewport_changed = false;
    if ctrl_scroll_delta.abs() > 1e-6 {
        let zoom_factor = 1.0015_f32.powf(ctrl_scroll_delta);
        let ptr_screen = ui
            .input(|i| i.pointer.latest_pos())
            .unwrap_or(panel.center());
        let ptr_world = cam.to_world(panel, ptr_screen);
        let new_zoom = (cam.zoom * zoom_factor).clamp(ZOOM_MIN, ZOOM_MAX);
        let new_center =
            egui::vec2(ptr_world.x, ptr_world.y) - (ptr_screen - panel.center()) / new_zoom;
        cam.zoom = new_zoom;
        cam.center = new_center;
        viewport_changed = true;
    }
    if scroll != egui::Vec2::ZERO {
        cam.center -= scroll / cam.zoom;
        viewport_changed = true;
    }
    let mid_delta = ui.input(|i| {
        if i.pointer.button_down(egui::PointerButton::Middle) {
            i.pointer.delta()
        } else {
            egui::Vec2::ZERO
        }
    });
    if mid_delta != egui::Vec2::ZERO {
        cam.center -= mid_delta / cam.zoom;
        viewport_changed = true;
    }
    if viewport_changed {
        events.push(MapEvent::ViewportMoved { cam });
    }

    // ------------------------------------------------------------------
    // World positions: physics for linked nodes + the orphan ring.
    // ------------------------------------------------------------------
    let mut world: HashMap<NoteId, Vec2> = state.layout.positions().clone();
    world.extend(orphan_ring_positions(
        state.layout.positions(),
        &state.orphans,
    ));

    let painter = ui.painter().with_clip_rect(panel);

    // Edges FIRST (thin theme stroke beneath the nodes).
    let edge_stroke = egui::Stroke::new(1.0, deps.theme.card_border);
    for (a, b) in &state.edges {
        if let (Some(pa), Some(pb)) = (world.get(a), world.get(b)) {
            painter.line_segment(
                [
                    cam.to_screen(panel, egui::pos2(pa.x, pa.y)),
                    cam.to_screen(panel, egui::pos2(pb.x, pb.y)),
                ],
                edge_stroke,
            );
        }
    }

    // Nodes: sized by degree, tinted per visual language (see fn docs),
    // hover tooltip, AccessKit label — the allocate_rect + widget_info
    // pattern proven by the desk's ghost minis.
    for meta in deps.metas {
        let Some(p) = world.get(&meta.id) else {
            continue;
        };
        let is_divider = meta.kind == Kind::Structure;
        let r_world = node_radius(
            state.degrees.get(&meta.id).copied().unwrap_or(0),
            is_divider,
        );
        let r_screen = (r_world * cam.zoom).max(SCREEN_R_MIN);
        let center = cam.to_screen(panel, egui::pos2(p.x, p.y));

        // Ease-in opacity (render-only; entries expire in app.rs).
        let opacity = state
            .appeared
            .get(&meta.id)
            .map(|t0| (t0.elapsed().as_secs_f32() / FADE_IN_SECS).clamp(0.0, 1.0))
            .unwrap_or(1.0);

        let fill = if is_divider {
            deps.theme.divider_tab_bg
        } else if meta.kind == Kind::Literature {
            deps.theme.rule_ink
        } else if meta.status == Status::Fleeting {
            deps.theme.text_weak
        } else {
            deps.theme.accent
        };

        // Hit/a11y rect: at least 8px square so tiny dots stay hoverable.
        let hit = r_screen.max(4.0);
        let rect = egui::Rect::from_center_size(center, egui::vec2(hit * 2.0, hit * 2.0));
        let resp = ui.allocate_rect(rect, egui::Sense::click());
        let label = format!("Map node: '{}'", meta.label);
        resp.widget_info(|| {
            egui::WidgetInfo::labeled(egui::WidgetType::Button, true, label.as_str())
        });

        painter.circle(
            center,
            r_screen,
            fill.gamma_multiply(opacity),
            egui::Stroke::new(1.0, deps.theme.card_border.gamma_multiply(opacity)),
        );

        resp.on_hover_text(meta.label.as_str());
    }

    events
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

    #[test]
    fn node_radius_law() {
        // 4 + 2√degree clamped 4..12; dividers +2.
        assert_eq!(node_radius(0, false), 4.0);
        assert_eq!(node_radius(1, false), 6.0);
        assert_eq!(node_radius(4, false), 8.0);
        assert_eq!(node_radius(16, false), 12.0);
        assert_eq!(node_radius(100, false), 12.0, "clamped at 12");
        assert_eq!(node_radius(0, true), 6.0, "divider bonus");
        assert_eq!(node_radius(100, true), 14.0, "bonus applies after clamp");
    }

    #[test]
    fn orphan_ring_outside_cluster_and_deterministic() {
        let mut positions = HashMap::new();
        positions.insert(nid(1), Vec2 { x: 0.0, y: 0.0 });
        positions.insert(nid(2), Vec2 { x: 100.0, y: 0.0 });
        let orphans = [nid(7), nid(8)];
        let a = orphan_ring_positions(&positions, &orphans);
        let b = orphan_ring_positions(&positions, &orphans);
        assert_eq!(a.len(), 2);
        // Deterministic: identical inputs, identical ring.
        for ((ia, pa), (ib, pb)) in a.iter().zip(&b) {
            assert_eq!(ia, ib);
            assert_eq!(pa, pb);
        }
        // Outside the cluster: distance from centroid (50,0) is exactly
        // bound (50) + margin, > every cluster node's distance.
        for (_, p) in &a {
            let d = ((p.x - 50.0).powi(2) + p.y.powi(2)).sqrt();
            assert!((d - (50.0 + ORPHAN_RING_MARGIN)).abs() < 1e-3);
        }
        // Distinct ids land at distinct bearings.
        assert_ne!(a[0].1, a[1].1);
    }

    #[test]
    fn orphan_ring_with_empty_cluster_circles_origin() {
        let positions = HashMap::new();
        let ring = orphan_ring_positions(&positions, &[nid(3)]);
        let p = ring[0].1;
        let d = (p.x * p.x + p.y * p.y).sqrt();
        assert!((d - ORPHAN_RING_MARGIN).abs() < 1e-3);
    }
}
