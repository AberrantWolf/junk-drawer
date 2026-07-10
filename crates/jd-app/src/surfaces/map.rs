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
//! Gate matrix (Task 3, the desk's uniform matrix): EVERY mutation —
//! click-select (a mutation of focus/selection), keyboard traversal, Enter
//! open, Ctrl+D take-to-desk, Shift+F10 context menu — is gated on
//! palette_open + editor_open + confirm_pending + the map's own popups
//! (desk picker, node context menu). Camera pan/zoom is never journaled and
//! (matching the desk, where scroll pan/zoom is ungated) runs regardless.
//! The palette-dim highlight is a READ and is exempt from the gates.
//!
//! EQUIVALENCE RULE (hard): the map is a LENS, never the only path — every
//! interaction here mirrors a canonical one (click/Enter/Ctrl+D/Shift+F10 =
//! the Drawer's; the Ctrl+K dim = the palette's own result list). Nothing is
//! learnable or doable ONLY from the map.

use std::collections::{HashMap, HashSet};
use std::time::Instant;

use eframe::egui;
use jd_core::geom::Vec2;
use jd_core::id::NoteId;
use jd_core::index::Index;
use jd_core::maplayout::{ForceLayout, LayoutParams};
use jd_core::note::{Kind, Status};
use jd_core::session::SessionState;

use crate::card::shape::{CardStyle, RuledLines, card_size, shape_for};
// The Drawer's mini pattern: card_face at 0.6 scale (the corner panel is the
// same widget the drawer grid uses).
use crate::surfaces::drawer::MINI_SCALE;
// DeskCamera is pure world↔screen math with no desk state — shared, not moved,
// so the desk keeps its own module-local usage untouched.
pub use crate::surfaces::desk::DeskCamera;
use crate::surfaces::desk::{FaceMeta, ZOOM_MAX, ZOOM_MIN};

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

/// Non-matching nodes render at this alpha while the palette is open on the
/// map (palette hits stay full-opacity; clears when the palette closes).
pub const DIM_ALPHA: f32 = 0.25;

/// Corner margin for the selected-node mini card panel.
const MINI_PANEL_MARGIN: f32 = 16.0;

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
    /// Palette-on-map dim highlight (the testable seam): `Some(set)` while
    /// the palette is open on the map with a non-empty query — nodes in the
    /// set render at DIM_ALPHA, palette hits stay lit. `None` (no dimming)
    /// when the palette is closed or its query is still empty. Recomputed
    /// each frame in app.rs from `dimmed_node_ids` (pure); render-only,
    /// exempt from the mutation gates (it is a READ).
    pub dimmed: Option<HashSet<NoteId>>,
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
            dimmed: None,
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
// Palette-dim highlight — pure (the kittests' testable seam)
// ---------------------------------------------------------------------------

/// The dim set for the palette-on-map highlight: every map node NOT in the
/// palette's current results. Pure — ids in, dim flags out; the render just
/// applies it (DIM_ALPHA for members, full opacity otherwise). Result ids
/// that aren't map nodes are ignored. No results → everything dims (the
/// query matched nothing).
pub fn dimmed_node_ids(nodes: &[NoteId], palette_result_ids: &[NoteId]) -> HashSet<NoteId> {
    let lit: HashSet<NoteId> = palette_result_ids.iter().copied().collect();
    nodes
        .iter()
        .copied()
        .filter(|id| !lit.contains(id))
        .collect()
}

// ---------------------------------------------------------------------------
// egui memory keys (popup state, the drawer/desk idiom)
// ---------------------------------------------------------------------------

/// Desk-picker state key (shared component, one picker per surface).
fn map_picker_id() -> egui::Id {
    egui::Id::new("map_desk_picker")
}

/// Shift+F10 "open context menu on the selected node" one-shot flag.
pub fn map_context_menu_open_id() -> egui::Id {
    egui::Id::new("map_node_context_menu_open")
}

/// The node whose context popup is open — ONE slot holding the NoteId, so a
/// focus move implicitly closes it (no per-node stale flags to sweep, which
/// matters at map scale).
fn map_popup_for_id() -> egui::Id {
    egui::Id::new("map_node_popup_for")
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
    /// Selection = the app-wide focus (view state, like the drawer's).
    pub focus: &'a mut Option<NoteId>,
    /// Keyboard traversal order: newest-modified first (Drawer parity),
    /// prefetched in app.rs under the same index read lock as `metas`.
    pub ordered_ids: &'a [NoteId],
    /// FaceMeta for the selected node (mini panel + context-menu enablement),
    /// prefetched under the same lock. None → no node selected.
    pub selected_meta: Option<&'a FaceMeta>,
    /// Top tags (≤2, "#tag") of the selected node, for the mini panel.
    pub selected_tags: &'a [String],
    /// Body cache for the mini panel's card_face (the drawer idiom).
    pub bodies: &'a mut crate::state::BodyCache,
    pub commands: &'a std::sync::mpsc::Sender<jd_core::worker::VaultCommand>,
    pub line_cache: &'a mut crate::editor::LineCache,
    /// Current session — desk picker rows + the context menu's desk submenu.
    pub session: &'a SessionState,
    pub editor_open: bool,
    pub confirm_pending: bool,
    pub palette_open: bool,
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
    /// Enter on the focused node — the existing surface-agnostic open path.
    OpenCard(NoteId),
    /// Ctrl+D → desk picker pick: place at that desk's viewport center (the
    /// journaled "Place card", same as the Drawer).
    PlaceOnDesk {
        id: NoteId,
        desk: jd_core::session::DeskId,
    },
    /// Shift+F10 context menu action on the selected node (card_menu_items
    /// is surface-agnostic; on_desk = false here).
    CardMenu(crate::menus::CardMenuEvent),
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
pub fn map_ui(ui: &mut egui::Ui, state: &MapState, deps: &mut MapUiDeps<'_>) -> Vec<MapEvent> {
    let mut events: Vec<MapEvent> = Vec::new();
    let panel = ui.max_rect();
    let mut cam = state.camera;

    // ------------------------------------------------------------------
    // Gate matrix (the desk's uniform matrix, day one): every mutation —
    // keyboard AND mouse (click-select mutates focus) — is blocked while
    // the palette / editor / confirm modal / our own popups are up. Camera
    // pan/zoom below stays ungated (desk parity); the palette-dim highlight
    // is a READ and renders regardless.
    // ------------------------------------------------------------------
    let picker_open = crate::surfaces::desk_picker::is_open(ui, map_picker_id());
    // Stale-slot sweep: the popup slot is only meaningful while its node IS
    // the current focus. Clear it the moment they diverge (focus moved by
    // the palette, a delete, anything app-side) so a LATER return of focus
    // to that node cannot resurrect a popup nobody asked for.
    if let Some(pid) = ui.memory(|m| m.data.get_temp::<NoteId>(map_popup_for_id()))
        && Some(pid) != *deps.focus
    {
        ui.memory_mut(|m| m.data.remove::<NoteId>(map_popup_for_id()));
    }
    let node_popup_open = ui
        .memory(|m| m.data.get_temp::<NoteId>(map_popup_for_id()))
        .is_some();
    let inputs_blocked = deps.editor_open
        || deps.confirm_pending
        || deps.palette_open
        || picker_open
        || node_popup_open;

    // ------------------------------------------------------------------
    // Keyboard (Drawer parity — the map is a lens over the same cards):
    // linear focus traversal newest-modified first, Enter opens in place,
    // Ctrl+D → shared desk picker, Shift+F10 → context menu.
    // ------------------------------------------------------------------
    let ids = deps.ordered_ids;
    if !inputs_blocked {
        let go_prev =
            ui.input(|i| i.key_pressed(egui::Key::ArrowUp) || i.key_pressed(egui::Key::ArrowLeft));
        let go_next = ui
            .input(|i| i.key_pressed(egui::Key::ArrowDown) || i.key_pressed(egui::Key::ArrowRight));
        if go_prev || go_next {
            let current_idx = deps.focus.and_then(|f| ids.iter().position(|id| *id == f));
            let next_idx = if go_next {
                match current_idx {
                    None if !ids.is_empty() => Some(0),
                    Some(i) if i + 1 < ids.len() => Some(i + 1),
                    _ => None,
                }
            } else {
                match current_idx {
                    None if !ids.is_empty() => Some(ids.len() - 1),
                    Some(i) if i > 0 => Some(i - 1),
                    _ => None,
                }
            };
            if let Some(idx) = next_idx {
                *deps.focus = Some(ids[idx]);
            }
        }

        // Enter → open editor in place (the surface-agnostic overlay).
        if ui.input(|i| i.key_pressed(egui::Key::Enter) && !i.modifiers.command)
            && let Some(id) = *deps.focus
            && ids.contains(&id)
        {
            events.push(MapEvent::OpenCard(id));
        }

        // Ctrl+D → desk picker for the focused node (shared component).
        let ctrl_d = ui.input(|i| {
            i.events.iter().any(|e| {
                matches!(
                    e,
                    egui::Event::Key {
                        key: egui::Key::D,
                        pressed: true,
                        modifiers,
                        ..
                    } if modifiers.command
                )
            })
        });
        if ctrl_d
            && let Some(id) = *deps.focus
            && ids.contains(&id)
            && !deps.session.desks.is_empty()
        {
            crate::surfaces::desk_picker::open_for(ui, map_picker_id(), id);
        }

        // Shift+F10 → context menu on the selected node (anchored Popup —
        // egui 0.35 cannot open a context_menu programmatically; the desk's
        // memory-flag pattern).
        let shift_f10 = ui.input(|i| {
            i.events.iter().any(|e| {
                matches!(
                    e,
                    egui::Event::Key {
                        key: egui::Key::F10,
                        pressed: true,
                        modifiers,
                        ..
                    } if modifiers.shift
                )
            })
        });
        // Arm only when the focused node still EXISTS (is in this frame's
        // metas): a focus id pointing at a deleted note would otherwise
        // leave the one-shot flag armed with no node to consume it.
        if shift_f10
            && let Some(f) = *deps.focus
            && deps.metas.iter().any(|m| m.id == f)
        {
            ui.memory_mut(|m| m.data.insert_temp(map_context_menu_open_id(), true));
        }
    }

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
    // Trackpad pinch arrives as Event::Zoom factors (desk parity — see
    // desk.rs: the raw-MouseWheel Ctrl+scroll path bypasses egui's
    // zoom_delta(), so pinch must be read explicitly).
    let pinch_factor: f32 = ui.input(|i| {
        i.events
            .iter()
            .filter_map(|ev| match ev {
                egui::Event::Zoom(z) => Some(*z),
                _ => None,
            })
            .product()
    });
    let mut viewport_changed = false;
    let zoom_factor = 1.0015_f32.powf(ctrl_scroll_delta) * pinch_factor;
    if (zoom_factor - 1.0).abs() > 1e-6 {
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
    let mut selected_resp: Option<egui::Response> = None;
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

        // Ease-in opacity (render-only; entries expire in app.rs), then the
        // palette-dim highlight (a READ — applied regardless of the gates):
        // palette hits stay lit, everything else drops to DIM_ALPHA.
        let mut opacity = state
            .appeared
            .get(&meta.id)
            .map(|t0| (t0.elapsed().as_secs_f32() / FADE_IN_SECS).clamp(0.0, 1.0))
            .unwrap_or(1.0);
        if state
            .dimmed
            .as_ref()
            .is_some_and(|dim| dim.contains(&meta.id))
        {
            opacity *= DIM_ALPHA;
        }

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

        // Click-select (a MUTATION of focus — gated, unlike the dim read).
        if resp.clicked() && !inputs_blocked && *deps.focus != Some(meta.id) {
            *deps.focus = Some(meta.id);
        }

        let is_focused = *deps.focus == Some(meta.id);
        if is_focused {
            // Selection ring (focus_ring, the card discipline) + keyboard
            // focus for a11y, held only while no overlay owns the keyboard.
            ui.painter().circle_stroke(
                center,
                r_screen + 3.0,
                egui::Stroke::new(2.0, deps.theme.focus_ring),
            );
            if !inputs_blocked {
                resp.request_focus();
            }
            selected_resp = Some(resp.clone());
        }

        resp.on_hover_text(meta.label.as_str());
    }

    // ------------------------------------------------------------------
    // Shift+F10 context menu on the selected node — the desk's anchored
    // Popup pattern; ONE memory slot holds the open node's id, so a focus
    // move implicitly closes it. card_menu_items is surface-agnostic
    // (on_desk = false: a map node is not a desk placement).
    // ------------------------------------------------------------------
    if let (Some(sel_id), Some(sel_resp)) = (*deps.focus, &selected_resp) {
        let wants_open: bool =
            ui.memory(|m| m.data.get_temp(map_context_menu_open_id()).unwrap_or(false));
        if wants_open {
            ui.memory_mut(|m| {
                m.data.insert_temp(map_context_menu_open_id(), false);
                m.data.insert_temp(map_popup_for_id(), sel_id);
            });
        }
        let popup_open = ui
            .memory(|m| m.data.get_temp::<NoteId>(map_popup_for_id()))
            .is_some_and(|pid| pid == sel_id);
        if popup_open {
            let desk_refs: Vec<(jd_core::session::DeskId, &str)> = deps
                .session
                .desks
                .iter()
                .map(|d| (d.id, d.name.as_str()))
                .collect();
            let menu_ctx = crate::menus::CardMenuCtx {
                id: sel_id,
                status: deps
                    .selected_meta
                    .map(|m| m.status)
                    .unwrap_or(Status::Fleeting),
                kind: deps.selected_meta.map(|m| m.kind).unwrap_or(Kind::Note),
                title: deps.selected_meta.map(|m| m.title.as_str()).unwrap_or(""),
                desks: &desk_refs,
                on_desk: false,
                editor_open: deps.editor_open,
                confirm_pending: deps.confirm_pending,
                palette_open: deps.palette_open,
            };
            let popup_id = egui::Id::new("map_node_context_popup").with(sel_id);
            egui::Popup::from_response(sel_resp)
                .id(popup_id)
                .open(true)
                .at_position(sel_resp.rect.left_bottom())
                .show(|ui| {
                    if let Some(ev) = crate::menus::card_menu_items(ui, &menu_ctx) {
                        events.push(MapEvent::CardMenu(ev));
                        ui.memory_mut(|m| m.data.remove::<NoteId>(map_popup_for_id()));
                    }
                });
            // Close on click-elsewhere and on Esc (consumed — defense in
            // depth against the same-frame surface handlers).
            if sel_resp.clicked_elsewhere()
                || ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape))
            {
                ui.memory_mut(|m| m.data.remove::<NoteId>(map_popup_for_id()));
            }
        }
    }

    // ------------------------------------------------------------------
    // Mini card panel (bottom-right corner) for the selected node — the
    // Drawer's mini pattern: the SAME card_face widget at MINI_SCALE, plus
    // its top tags. Read-only company for the dots; opening still goes
    // through Enter / the canonical paths.
    // ------------------------------------------------------------------
    if let Some(meta) = deps.selected_meta
        && *deps.focus == Some(meta.id)
    {
        let shape = shape_for(meta.status, meta.kind);
        let size = card_size(shape) * MINI_SCALE;
        let rect = egui::Rect::from_min_size(
            egui::pos2(
                panel.max.x - size.x - MINI_PANEL_MARGIN,
                panel.max.y - size.y - MINI_PANEL_MARGIN,
            ),
            size,
        );
        let body_str = deps
            .bodies
            .get_or_request(meta.id, deps.commands)
            .map(|b| b.text.as_str());
        let face = crate::card::CardFace {
            id: meta.id,
            title: meta.title.as_str(),
            body: body_str,
            shape,
            style: CardStyle::Paper,
            lines: RuledLines::Natural,
            source: meta.source.as_deref(),
            links: meta.links,
            tags: meta.tags,
            focused: false,
        };
        let _ = crate::card::card_face(ui, rect, &face, deps.theme, deps.line_cache);
        if !deps.selected_tags.is_empty() {
            ui.painter().text(
                egui::pos2(rect.min.x, rect.min.y - 6.0),
                egui::Align2::LEFT_BOTTOM,
                deps.selected_tags.join(" "),
                egui::FontId::new(12.0, egui::FontFamily::Proportional),
                deps.theme.text_weak,
            );
        }
    }

    // ------------------------------------------------------------------
    // Desk picker (shared component) — rendered above everything; a pick
    // becomes the journaled place-on-desk in app.rs.
    // ------------------------------------------------------------------
    if picker_open
        && let Some((id, desk)) = crate::surfaces::desk_picker::desk_picker_ui(
            ui,
            map_picker_id(),
            panel,
            deps.session,
            deps.theme,
        )
    {
        events.push(MapEvent::PlaceOnDesk { id, desk });
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
    fn dimmed_set_is_nodes_minus_palette_results() {
        let nodes = [nid(1), nid(2), nid(3)];
        // Result ids not on the map (nid(9)) are ignored.
        let dimmed = dimmed_node_ids(&nodes, &[nid(2), nid(9)]);
        assert!(dimmed.contains(&nid(1)));
        assert!(!dimmed.contains(&nid(2)), "palette hits stay lit");
        assert!(dimmed.contains(&nid(3)));
        assert_eq!(dimmed.len(), 2);
    }

    #[test]
    fn dimmed_set_with_no_results_dims_everything() {
        let nodes = [nid(1), nid(2)];
        let dimmed = dimmed_node_ids(&nodes, &[]);
        assert_eq!(dimmed.len(), 2, "no hits → the whole field dims");
    }

    #[test]
    fn dimmed_set_with_all_matching_dims_nothing() {
        let nodes = [nid(1), nid(2)];
        let dimmed = dimmed_node_ids(&nodes, &[nid(1), nid(2)]);
        assert!(dimmed.is_empty());
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
