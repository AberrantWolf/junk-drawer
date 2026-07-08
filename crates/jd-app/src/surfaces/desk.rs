//! The desk surface. This file owns spatial focus order (Spike B) and,
//! from Task 8, the pannable canvas itself.

use eframe::egui;
use jd_core::geom::Vec2;
use jd_core::id::NoteId;
use jd_core::note::NoteMeta;
use jd_core::session::{Desk, DeskId, SessionOp};

/// 0.6 × index-card height (200.0). Rounded y-bands make reading order
/// stable under small drags (architecture §3, spec §12).
pub const BAND_HEIGHT: f32 = 120.0;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FocusDir {
    Left,
    Right,
    Up,
    Down,
}

fn band(y: f32) -> i64 {
    (y / BAND_HEIGHT).round() as i64
}

pub fn reading_order(cards: &[(NoteId, Vec2)]) -> Vec<NoteId> {
    let mut v: Vec<&(NoteId, Vec2)> = cards.iter().collect();
    v.sort_by(|a, b| {
        band(a.1.y)
            .cmp(&band(b.1.y))
            .then(a.1.x.total_cmp(&b.1.x))
            .then(a.0.cmp(&b.0))
    });
    v.into_iter().map(|(id, _)| *id).collect()
}

pub fn next_focus(
    cards: &[(NoteId, Vec2)],
    current: Option<NoteId>,
    dir: FocusDir,
) -> Option<NoteId> {
    if cards.is_empty() {
        return None;
    }
    let order = reading_order(cards);
    let Some(cur) = current else {
        return order.first().copied();
    };
    let Some(idx) = order.iter().position(|id| *id == cur) else {
        return order.first().copied();
    };
    match dir {
        FocusDir::Left => idx.checked_sub(1).map(|i| order[i]),
        FocusDir::Right => order.get(idx + 1).copied(),
        FocusDir::Up | FocusDir::Down => {
            let pos = cards.iter().find(|(id, _)| *id == cur)?.1;
            let cur_band = band(pos.y);
            let step: i64 = if dir == FocusDir::Down { 1 } else { -1 };
            // Search outward band by band for the nearest card by |Δx|.
            let bands: std::collections::BTreeSet<i64> =
                cards.iter().map(|(_, p)| band(p.y)).collect();
            let mut target = cur_band + step;
            let (min_b, max_b) = (*bands.iter().next()?, *bands.iter().last()?);
            while target >= min_b && target <= max_b {
                let mut best: Option<(f32, NoteId)> = None;
                for (id, p) in cards {
                    if band(p.y) == target {
                        let dx = (p.x - pos.x).abs();
                        if best.is_none_or(|(bd, bid)| dx < bd || (dx == bd && *id < bid)) {
                            best = Some((dx, *id));
                        }
                    }
                }
                if let Some((_, id)) = best {
                    return Some(id);
                }
                target += step;
            }
            None
        }
    }
}

pub fn card_a11y_label(
    title: &str,
    first_line: &str,
    is_scrap: bool,
    links: usize,
    tags: usize,
) -> String {
    if is_scrap {
        return format!("Scrap: '{first_line}'");
    }
    let l = if links == 1 { "link" } else { "links" };
    let t = if tags == 1 { "tag" } else { "tags" };
    format!("Card: '{title}', {links} {l}, {tags} {t}")
}

// ===========================================================================
// Task 8: pannable desk canvas
// ===========================================================================

pub const ZOOM_MIN: f32 = 0.5;
pub const ZOOM_MAX: f32 = 2.0;

/// Desk-space → screen-space: screen = (world - viewport.center) * zoom + panel_center.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DeskCamera {
    pub center: egui::Vec2,
    pub zoom: f32,
}

impl DeskCamera {
    pub fn to_screen(&self, panel: egui::Rect, world: egui::Pos2) -> egui::Pos2 {
        let panel_center = panel.center();
        let world_v = egui::vec2(world.x, world.y);
        let offset = (world_v - self.center) * self.zoom;
        panel_center + offset
    }

    pub fn to_world(&self, panel: egui::Rect, screen: egui::Pos2) -> egui::Pos2 {
        let panel_center = panel.center();
        let offset = screen - panel_center;
        let world_v = self.center + offset / self.zoom;
        egui::pos2(world_v.x, world_v.y)
    }

    /// Zoom and pan so all placed cards fit within `panel` with some padding.
    pub fn zoom_to_fit(&mut self, cards: &[(NoteId, Vec2)], panel: egui::Rect) {
        if cards.is_empty() {
            self.center = egui::Vec2::ZERO;
            self.zoom = 1.0;
            return;
        }
        let mut min_x = f32::MAX;
        let mut min_y = f32::MAX;
        let mut max_x = f32::MIN;
        let mut max_y = f32::MIN;
        // Include card extent (use a nominal 300×200 size for all shapes)
        let card_w = 300.0_f32;
        let card_h = 200.0_f32;
        for (_, p) in cards {
            min_x = min_x.min(p.x);
            min_y = min_y.min(p.y);
            max_x = max_x.max(p.x + card_w);
            max_y = max_y.max(p.y + card_h);
        }
        let content_w = (max_x - min_x).max(1.0);
        let content_h = (max_y - min_y).max(1.0);
        let pad = 60.0;
        let zoom_x = (panel.width() - pad * 2.0) / content_w;
        let zoom_y = (panel.height() - pad * 2.0) / content_h;
        // zoom_to_fit uses a wider range than interactive zoom — we want to see all cards
        // even if they span a large area. Minimum is 0.01 (very far out), max is ZOOM_MAX.
        self.zoom = zoom_x.min(zoom_y).clamp(0.01, ZOOM_MAX);
        // Center is the world-space midpoint of all content.
        self.center = egui::vec2((min_x + max_x) / 2.0, (min_y + max_y) / 2.0);
    }
}

// ---------------------------------------------------------------------------
// WP4 Task 5: ghost fan ranking + edges-on-select (architecture §3, §6.16)
// ---------------------------------------------------------------------------

/// The fan shows at most this many ghosts.
pub const GHOST_K: usize = 5;
/// Ghost minis render at this fraction of the real card size.
pub const GHOST_SCALE: f32 = 0.4;
/// Gap between the anchor card and the fan, and between fan minis (world px).
const GHOST_GAP: f32 = 24.0;
/// How many cosine neighbours `ghost_candidates` blends in. `Index::similar`
/// returns only its top-k, so structural candidates outside this top-N get a
/// cosine contribution of 0 (a deliberate approximation — see below).
const GHOST_SIMILAR_N: usize = 32;

/// Pinned ranking weights (decision §6.16). A unit test enforces the law:
/// direct link 3.0 > backlink 2.5 > shared tag 1.0 each; relations stack and
/// the cosine similarity (clamped 0–1) is added on top.
pub const W_DIRECT_LINK: f32 = 3.0;
pub const W_BACKLINK: f32 = 2.5;
pub const W_SHARED_TAG: f32 = 1.0;

/// Pure scoring law for one candidate. Kept free of the Index so the weights
/// are pinned by a table-style unit test.
pub fn ghost_score(direct_link: bool, backlink: bool, shared_tags: usize, cosine: f32) -> f32 {
    (if direct_link { W_DIRECT_LINK } else { 0.0 })
        + (if backlink { W_BACKLINK } else { 0.0 })
        + shared_tags as f32 * W_SHARED_TAG
        + cosine.clamp(0.0, 1.0)
}

/// Rank every OFF-desk note as a ghost candidate for `id` (the selected/open
/// card): top `GHOST_K` by `ghost_score`, ties broken by id for determinism.
///
/// Blend note: links, backlinks and shared tags are scored exhaustively from
/// the index adjacency (links_fwd/links_rev/tags). Cosine comes from
/// `Index::similar(id, GHOST_SIMILAR_N)`, which only returns its top-N
/// neighbours — candidates outside that top-N simply get cosine 0, and
/// cosine-only neighbours (no structural relation) still enter the pool.
/// Call under the caller's index read lock (the ONE-lock-per-frame idiom).
pub fn ghost_candidates(
    idx: &jd_core::index::Index,
    id: NoteId,
    on_desk: &std::collections::HashSet<NoteId>,
) -> Vec<(NoteId, f32)> {
    use std::collections::{HashMap, HashSet};
    let Some(meta) = idx.get(id) else {
        return Vec::new();
    };

    // Structural relations (deduped: two [[X]] refs are still ONE direct link).
    let direct: HashSet<NoteId> = idx.outlinks(id).iter().filter_map(|(_, r)| *r).collect();
    let back: HashSet<NoteId> = idx.backlinks(id).into_iter().collect();
    let mut shared_tags: HashMap<NoteId, usize> = HashMap::new();
    for tag in &meta.tags {
        for other in idx.notes_with_tag(tag) {
            *shared_tags.entry(other).or_default() += 1;
        }
    }
    let cosine: HashMap<NoteId, f32> = idx.similar(id, GHOST_SIMILAR_N).into_iter().collect();

    let pool: HashSet<NoteId> = direct
        .iter()
        .chain(back.iter())
        .chain(shared_tags.keys())
        .chain(cosine.keys())
        .copied()
        .collect();

    let mut scored: Vec<(NoteId, f32)> = pool
        .into_iter()
        .filter(|c| *c != id && !on_desk.contains(c) && idx.get(*c).is_some())
        .map(|c| {
            let s = ghost_score(
                direct.contains(&c),
                back.contains(&c),
                shared_tags.get(&c).copied().unwrap_or(0),
                cosine.get(&c).copied().unwrap_or(0.0),
            );
            (c, s)
        })
        .filter(|(_, s)| *s > 0.0)
        .collect();
    scored.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
    scored.truncate(GHOST_K);
    scored
}

/// Edges to draw when `selected` is the focused/open card: one edge per
/// distinct linked-or-linking note that is ON the desk (both directions,
/// deduped — a mutual link is one edge). Sorted by the far endpoint for
/// determinism. This pure fn is the testable seam: kitests assert counts
/// here instead of reading pixels. Call under the caller's index read lock.
pub fn selected_edges(
    idx: &jd_core::index::Index,
    selected: NoteId,
    on_desk: &std::collections::HashSet<NoteId>,
) -> Vec<(NoteId, NoteId)> {
    let mut others: std::collections::BTreeSet<NoteId> = std::collections::BTreeSet::new();
    for (_, resolved) in idx.outlinks(selected) {
        if let Some(t) = resolved
            && t != selected
            && on_desk.contains(&t)
        {
            others.insert(t);
        }
    }
    for b in idx.backlinks(selected) {
        if b != selected && on_desk.contains(&b) {
            others.insert(b);
        }
    }
    others.into_iter().map(|o| (selected, o)).collect()
}

/// One ghost mini, prefetched in app.rs under the frame's index read lock.
pub struct GhostSpec {
    pub id: NoteId,
    /// Title, or first line for untitled scraps (a11y label + mini face text).
    pub title: String,
    /// The candidate's REAL card size in world units (fan scales it by
    /// `GHOST_SCALE`; a click places the real card at the ghost's position).
    pub size: egui::Vec2,
}

/// World-space top-left positions for the fan minis.
///
/// Edge heuristic (documented per §6.16): measure the free space between the
/// anchor card and the visible panel's world bounds on each of N/E/S/W and
/// fan along the side with the MOST free space — a simple, stable choice that
/// keeps ghosts on-screen without any packing logic. E/W fans stack
/// vertically centered on the anchor; N/S fans run horizontally.
pub fn ghost_fan_positions(
    anchor: egui::Rect,
    panel_world: egui::Rect,
    sizes: &[egui::Vec2],
) -> Vec<egui::Pos2> {
    if sizes.is_empty() {
        return Vec::new();
    }
    let free_w = anchor.min.x - panel_world.min.x;
    let free_e = panel_world.max.x - anchor.max.x;
    let free_n = anchor.min.y - panel_world.min.y;
    let free_s = panel_world.max.y - anchor.max.y;
    let ghost_size: egui::Vec2 = sizes.iter().fold(egui::Vec2::ZERO, |m, s| m.max(*s));

    // (free space, side). max_by on total order; ties resolve to the LAST max,
    // so list in W,E,N,S order for a stable preference of S > N > E > W on ties.
    let side = [(free_w, 'W'), (free_e, 'E'), (free_n, 'N'), (free_s, 'S')]
        .into_iter()
        .max_by(|a, b| a.0.total_cmp(&b.0))
        .map(|(_, s)| s)
        .unwrap_or('E');

    let n = sizes.len() as f32;
    match side {
        'W' | 'E' => {
            let total_h = n * ghost_size.y + (n - 1.0) * GHOST_GAP;
            let start_y = anchor.center().y - total_h / 2.0;
            let x = if side == 'W' {
                anchor.min.x - GHOST_GAP - ghost_size.x
            } else {
                anchor.max.x + GHOST_GAP
            };
            (0..sizes.len())
                .map(|i| egui::pos2(x, start_y + i as f32 * (ghost_size.y + GHOST_GAP)))
                .collect()
        }
        _ => {
            let total_w = n * ghost_size.x + (n - 1.0) * GHOST_GAP;
            let start_x = anchor.center().x - total_w / 2.0;
            let y = if side == 'N' {
                anchor.min.y - GHOST_GAP - ghost_size.y
            } else {
                anchor.max.y + GHOST_GAP
            };
            (0..sizes.len())
                .map(|i| egui::pos2(start_x + i as f32 * (ghost_size.x + GHOST_GAP), y))
                .collect()
        }
    }
}

// ---------------------------------------------------------------------------
// Prefetched face metadata (ONE index read lock per frame in app.rs)
// ---------------------------------------------------------------------------

/// Per-card metadata prefetched from the index under a single read lock.
pub struct FaceMeta {
    pub id: NoteId,
    pub title: String,
    pub first_line: String,
    pub links: usize,
    pub tags: usize,
    pub is_scrap: bool,
    pub source: Option<String>,
    pub status: jd_core::note::Status,
    pub kind: jd_core::note::Kind,
}

impl FaceMeta {
    pub fn from_note_meta(meta: &NoteMeta, index: &jd_core::index::Index) -> FaceMeta {
        let links = index.outlinks(meta.id).len();
        FaceMeta {
            id: meta.id,
            title: meta.title.clone().unwrap_or_default(),
            first_line: meta.first_line.clone(),
            links,
            tags: meta.tags.len(),
            is_scrap: meta.title.is_none(),
            source: meta.source.clone(),
            status: meta.status,
            kind: meta.kind,
        }
    }
}

// ---------------------------------------------------------------------------
// Drag state
// ---------------------------------------------------------------------------

pub struct DragState {
    pub id: NoteId,
    pub grab_offset: egui::Vec2,
    /// World position at drag start (for Move op).
    pub from: Vec2,
    /// Total pixel drag distance (to gate click vs move).
    pub total_delta: f32,
}

// ---------------------------------------------------------------------------
// DeskEvent
// ---------------------------------------------------------------------------

/// Events emitted by `desk_ui` for `app.rs` to apply.
#[derive(Debug)]
pub enum DeskEvent {
    OpenCard(NoteId),
    SessionOp(SessionOp),
    FocusChanged(Option<NoteId>),
    /// Camera moved/zoomed — NOT journaled; just marks `session_dirty_at`.
    ViewportMoved {
        desk: DeskId,
        cam: DeskCamera,
    },
    /// Context-menu action on a card.
    CardMenu(crate::menus::CardMenuEvent),
    /// Card was dragged onto a rail row (Inbox or a desk row).
    /// app.rs handles this as CardDroppedOnInbox / CardDroppedOnDesk.
    CardDroppedOnRail(crate::rail::RailEvent),
    /// Face-side click on the Nth (0-based ordinal) task checkbox of `id`.
    /// app.rs toggles the raw body byte ([ ]↔[x]) and dispatches VaultOp::SaveBody.
    ToggleTaskBox {
        id: NoteId,
        /// 0-based ordinal of the clicked task box in the raw body.
        ordinal: usize,
    },
    /// A ghost mini was clicked: place the real card at the ghost's world
    /// position (app.rs calls `place_card` — journaled "Place card").
    GhostClicked {
        id: NoteId,
        /// World-space top-left where the ghost stood (the card lands there).
        pos: Vec2,
    },
}

// ---------------------------------------------------------------------------
// DeskUiDeps
// ---------------------------------------------------------------------------

pub struct DeskUiDeps<'a> {
    pub focus: &'a mut Option<NoteId>,
    pub bodies: &'a mut crate::state::BodyCache,
    pub commands: &'a std::sync::mpsc::Sender<jd_core::worker::VaultCommand>,
    pub theme: &'a crate::theme::Theme,
    pub line_cache: &'a mut crate::editor::LineCache,
    pub face_metas: &'a [FaceMeta],
    pub drag: &'a mut Option<DragState>,
    pub editor_open: bool,
    /// True while a delete-confirm modal is pending; suppresses all surface
    /// keyboard handling so the modal's Enter/Esc are the only consumers.
    pub confirm_pending: bool,
    /// True while the Ctrl+K palette overlay is open; suppresses all surface
    /// keyboard handling (same gate pattern as confirm_pending) and stops the
    /// focused card from stealing keyboard focus from the palette input.
    pub palette_open: bool,
    /// WP4 Task 2: highlight pulse at a card the palette panned to —
    /// (card id, age fraction 0..1). desk_ui paints a fading ring around the
    /// card; app.rs owns the timer and clears it after ~600ms.
    pub highlight_pulse: Option<(NoteId, f32)>,
    /// All desks (id + name) for the "Take to Desk ▸" submenu.
    pub desks: &'a [(jd_core::session::DeskId, String)],
    /// The current desk id — used to determine whether a card is "on a desk"
    /// (so Put Away is enabled when desk surface is active).
    pub current_desk_id: jd_core::session::DeskId,
    /// Rail row rects from the previous frame (populated by rail_ui each frame).
    /// On drag release beyond the 4px threshold, if the release pointer position
    /// is inside one of these rects, we emit CardDroppedOnInbox / CardDroppedOnDesk
    /// instead of a plain Move.
    pub rail_row_hits: &'a [(egui::Rect, crate::rail::RailDropTarget)],
    /// WP4 Task 5: the selected/open card the ghost fan + edges anchor to
    /// (None when nothing on this desk is selected or open). Prefetched in
    /// app.rs under the frame's single index read lock, like face_metas.
    pub ghost_anchor: Option<NoteId>,
    /// Top-GHOST_K off-desk ghost candidates for the anchor (ranked).
    pub ghosts: &'a [GhostSpec],
    /// Edges from the anchor to linked/linking cards ON this desk
    /// (`selected_edges` — both directions, deduped).
    pub edges: &'a [(NoteId, NoteId)],
}

// ---------------------------------------------------------------------------
// desk_ui
// ---------------------------------------------------------------------------

/// Render the active desk; returns events for app.rs to apply (desk itself
/// never touches SessionState directly — one mutation site, in app.rs).
pub fn desk_ui(ui: &mut egui::Ui, desk: &Desk, state: &mut DeskUiDeps<'_>) -> Vec<DeskEvent> {
    let mut events: Vec<DeskEvent> = Vec::new();
    let panel = ui.max_rect();

    // Build camera from the desk's viewport.
    let mut cam = DeskCamera {
        center: egui::vec2(desk.viewport.center.x, desk.viewport.center.y),
        zoom: desk.viewport.zoom,
    };

    // ------------------------------------------------------------------
    // 1. Build card positions for focus/reading-order
    // ------------------------------------------------------------------
    let card_positions: Vec<(NoteId, Vec2)> = desk.cards.iter().map(|c| (c.id, c.pos)).collect();

    // ------------------------------------------------------------------
    // 2. Keyboard handling (only when editor is closed, no confirm modal,
    //    and no card context-menu popup is open).
    //
    //    Read the per-card popup flag for the currently focused card.  When
    //    the popup is open, Enter must NOT open the card editor — it should
    //    be handled (or ignored) by the popup itself.  Same for all other
    //    surface keys (arrows, Backspace, Shift+F10).
    // ------------------------------------------------------------------
    let card_popup_open: bool = state
        .focus
        .map(|id| {
            ui.memory(|m| {
                m.data
                    .get_temp::<bool>(card_popup_open_id(id))
                    .unwrap_or(false)
            })
        })
        .unwrap_or(false);

    if !state.editor_open && !state.confirm_pending && !state.palette_open && !card_popup_open {
        for (key, dir) in [
            (egui::Key::ArrowLeft, FocusDir::Left),
            (egui::Key::ArrowRight, FocusDir::Right),
            (egui::Key::ArrowUp, FocusDir::Up),
            (egui::Key::ArrowDown, FocusDir::Down),
        ] {
            if ui.input(|i| i.key_pressed(key)) {
                let next = next_focus(&card_positions, *state.focus, dir);
                if next != *state.focus {
                    *state.focus = next;
                    events.push(DeskEvent::FocusChanged(*state.focus));
                }
            }
        }

        // Ctrl+Enter → promote the focused card if it is fleeting (spec
        // Appendix A: "Promote scrap (card focus or its editor)" — desk cards
        // CAN be fleeting). For a permanent card Ctrl+Enter has no defined
        // action on card focus (the "close editor" meaning applies IN-editor
        // only), so it is a deliberate no-op — but the key is consumed either
        // way so it cannot leak into the just-opened editor this same frame
        // (see the matching consume in inbox.rs) or fire plain OpenCard below.
        let ctrl_enter =
            ui.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, egui::Key::Enter));
        if ctrl_enter
            && let Some(id) = *state.focus
            && state
                .face_metas
                .iter()
                .any(|m| m.id == id && m.status == jd_core::note::Status::Fleeting)
        {
            // Same promote path as the card context menu / inbox Ctrl+Enter.
            events.push(DeskEvent::CardMenu(crate::menus::CardMenuEvent::Promote(
                id,
            )));
        }

        // Enter → open focused card (plain Enter only; Ctrl+Enter is the
        // promote path above — the !command guard keeps a Ctrl+Enter whose
        // Key event was already consumed from opening the card via the
        // still-set modifier state).
        if ui.input(|i| i.key_pressed(egui::Key::Enter) && !i.modifiers.command)
            && let Some(id) = *state.focus
        {
            events.push(DeskEvent::OpenCard(id));
        }

        // Backspace → put away focused card
        if ui.input(|i| i.key_pressed(egui::Key::Backspace))
            && let Some(id) = *state.focus
            && let Some(card) = desk.cards.iter().find(|c| c.id == id)
        {
            let was_at = card.pos;
            // Advance focus before removal
            let next = next_focus(&card_positions, Some(id), FocusDir::Right)
                .or_else(|| next_focus(&card_positions, Some(id), FocusDir::Left));
            *state.focus = next;
            events.push(DeskEvent::FocusChanged(*state.focus));
            events.push(DeskEvent::SessionOp(SessionOp::PutAway {
                desk: desk.id,
                id,
                was_at,
            }));
        }

        // Shift+F10 → open card context menu at the focused card's rect.
        // egui 0.35 cannot open a context_menu programmatically from keyboard
        // input, so we use egui memory to set a flag that the card-render loop
        // reads to open an anchored Popup instead.  This is equivalent to
        // right-click context_menu but initiated via keyboard.
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
        if shift_f10 && state.focus.is_some() {
            ui.memory_mut(|m| {
                m.data.insert_temp(context_menu_open_id(), true);
            });
        }
    }

    // ------------------------------------------------------------------
    // 3. Pan and zoom via scroll
    // ------------------------------------------------------------------
    // Plain/shift scroll → smooth_scroll_delta; Ctrl+scroll → raw y delta used
    // with spec formula 1.0015^delta for precise, spec-compliant zoom speed.
    let scroll = ui.input(|i| i.smooth_scroll_delta);
    // Read Ctrl+scroll raw delta for spec-formula zoom (not zoom_delta() which
    // uses egui's own formula). Sum all MouseWheel events with Ctrl held this frame.
    // Line unit default: egui uses 50px/line; Page unit: use panel height.
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

    // Zoom: spec formula 1.0015^scroll_delta_points.
    if ctrl_scroll_delta.abs() > 1e-6 {
        let zoom_factor = 1.0015_f32.powf(ctrl_scroll_delta);
        let ptr_screen = ui
            .input(|i| i.pointer.latest_pos())
            .unwrap_or(panel.center());
        let ptr_world = cam.to_world(panel, ptr_screen);
        let new_zoom = (cam.zoom * zoom_factor).clamp(ZOOM_MIN, ZOOM_MAX);
        // Anchor invariant: ptr_world stays at ptr_screen.
        // new_center = ptr_world - (ptr_screen - panel_center) / new_zoom
        let new_center =
            egui::vec2(ptr_world.x, ptr_world.y) - (ptr_screen - panel.center()) / new_zoom;
        cam.zoom = new_zoom;
        cam.center = new_center;
        viewport_changed = true;
    }

    // Pan: plain scroll (y = vertical, x = horizontal with Shift).
    if scroll != egui::Vec2::ZERO {
        cam.center -= scroll / cam.zoom;
        viewport_changed = true;
    }

    // Middle-mouse drag → pan
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
        events.push(DeskEvent::ViewportMoved { desk: desk.id, cam });
    }

    // ------------------------------------------------------------------
    // 4. Pointer state for drag
    // ------------------------------------------------------------------
    let pointer_pos = ui.input(|i| i.pointer.latest_pos());
    let primary_pressed = ui.input(|i| i.pointer.button_pressed(egui::PointerButton::Primary));
    let primary_released = ui.input(|i| i.pointer.button_released(egui::PointerButton::Primary));
    let pointer_delta = ui.input(|i| i.pointer.delta());

    // Hit-test which card the pointer is over (for drag/focus).
    // Use per-shape card_size so the hit rect matches the rendered outline.
    let pointer_over_card: Option<NoteId> = pointer_pos.and_then(|pos| {
        desk.cards.iter().find_map(|card| {
            let screen_min = cam.to_screen(panel, egui::pos2(card.pos.x, card.pos.y));
            let meta_size = state.face_metas.iter().find(|m| m.id == card.id).map(|m| {
                crate::card::shape::card_size(crate::card::shape::shape_for(m.status, m.kind))
            });
            let world_size = meta_size.unwrap_or_else(|| egui::vec2(300.0, 200.0));
            let rect = egui::Rect::from_min_size(screen_min, world_size * cam.zoom);
            if rect.contains(pos) {
                Some(card.id)
            } else {
                None
            }
        })
    });

    // Drag start. Gated while the palette overlay is open (same discipline as
    // double-click/keyboard): a press behind the palette must not start a drag.
    if primary_pressed
        && !state.palette_open
        && let Some(id) = pointer_over_card
        && let Some(card) = desk.cards.iter().find(|c| c.id == id)
    {
        let card_screen = cam.to_screen(panel, egui::pos2(card.pos.x, card.pos.y));
        let grab_offset = pointer_pos.unwrap_or(card_screen) - card_screen;
        *state.drag = Some(DragState {
            id,
            grab_offset,
            from: card.pos,
            total_delta: 0.0,
        });
        if *state.focus != Some(id) {
            *state.focus = Some(id);
            events.push(DeskEvent::FocusChanged(*state.focus));
        }
    }

    // Drag motion
    if let Some(ref mut drag) = *state.drag {
        drag.total_delta += pointer_delta.length();
    }

    // Drag release → check rail drop targets first, then emit Move if beyond threshold
    #[allow(clippy::collapsible_if)]
    if primary_released && let Some(drag) = state.drag.take() {
        if drag.total_delta >= 4.0
            && let Some(ptr) = pointer_pos
        {
            // Check if the pointer is over a rail row (drag-to-rail gesture).
            // Rail hits are in screen coordinates, same space as `ptr`.
            let rail_hit = state
                .rail_row_hits
                .iter()
                .find(|(rect, _)| rect.contains(ptr));

            if let Some((_, target)) = rail_hit {
                // Emit the appropriate rail drop event instead of a plain Move.
                match *target {
                    crate::rail::RailDropTarget::Inbox => {
                        events.push(DeskEvent::CardDroppedOnRail(
                            crate::rail::RailEvent::CardDroppedOnInbox {
                                id: drag.id,
                                source_desk: desk.id,
                                was_at: drag.from,
                            },
                        ));
                    }
                    crate::rail::RailDropTarget::Desk(target_desk) => {
                        if target_desk != desk.id {
                            // Cross-desk drop.
                            events.push(DeskEvent::CardDroppedOnRail(
                                crate::rail::RailEvent::CardDroppedOnDesk {
                                    target_desk,
                                    id: drag.id,
                                    source_desk: desk.id,
                                    was_at: drag.from,
                                },
                            ));
                        } else {
                            // Dropped on the same desk's rail row — treat as plain move
                            // within the desk (user probably didn't want to change desks).
                            let new_screen = ptr - drag.grab_offset;
                            let new_world = cam.to_world(panel, new_screen);
                            let to = Vec2 {
                                x: new_world.x,
                                y: new_world.y,
                            };
                            events.push(DeskEvent::SessionOp(SessionOp::Move {
                                desk: desk.id,
                                id: drag.id,
                                from: drag.from,
                                to,
                            }));
                        }
                    }
                }
            } else {
                // Normal move within the desk.
                let new_screen = ptr - drag.grab_offset;
                let new_world = cam.to_world(panel, new_screen);
                let to = Vec2 {
                    x: new_world.x,
                    y: new_world.y,
                };
                events.push(DeskEvent::SessionOp(SessionOp::Move {
                    desk: desk.id,
                    id: drag.id,
                    from: drag.from,
                    to,
                }));
            }
        }
        // else: tiny drag = click, focus already set on press
    }

    // Background drag → pan (pointer on empty background, no card drag active)
    if state.drag.is_none()
        && pointer_over_card.is_none()
        && ui.input(|i| i.pointer.button_down(egui::PointerButton::Primary))
        && pointer_delta != egui::Vec2::ZERO
    {
        cam.center -= pointer_delta / cam.zoom;
        events.push(DeskEvent::ViewportMoved { desk: desk.id, cam });
    }

    // ------------------------------------------------------------------
    // 5. Render card faces (with culling)
    // ------------------------------------------------------------------
    let cull_rect = panel.expand(100.0);

    // Edges-on-select (WP4 Task 5): painted BEFORE the card faces so the
    // lines sit beneath the cards (painter order = z order in one layer).
    // Endpoints are card centers in world space; the stroke is subtle
    // (theme text_weak at half alpha) so edges read as context, not content.
    if !state.edges.is_empty() {
        let world_center = |id: NoteId| -> Option<egui::Pos2> {
            let card = desk.cards.iter().find(|c| c.id == id)?;
            let size = state
                .face_metas
                .iter()
                .find(|m| m.id == id)
                .map(|m| {
                    crate::card::shape::card_size(crate::card::shape::shape_for(m.status, m.kind))
                })
                .unwrap_or_else(|| egui::vec2(300.0, 200.0));
            Some(egui::pos2(
                card.pos.x + size.x / 2.0,
                card.pos.y + size.y / 2.0,
            ))
        };
        let stroke = egui::Stroke::new(1.5, state.theme.text_weak.gamma_multiply(0.5));
        for (a, b) in state.edges {
            if let (Some(ca), Some(cb)) = (world_center(*a), world_center(*b)) {
                ui.painter()
                    .line_segment([cam.to_screen(panel, ca), cam.to_screen(panel, cb)], stroke);
            }
        }
    }

    // Live drag position for the card being dragged
    let dragged_id = state.drag.as_ref().map(|d| d.id);
    let live_drag_screen: Option<egui::Pos2> = if let Some(ref drag) = *state.drag {
        pointer_pos.map(|ptr| ptr - drag.grab_offset)
    } else {
        None
    };

    for card in &desk.cards {
        let card_screen_min = if dragged_id == Some(card.id) {
            live_drag_screen
                .unwrap_or_else(|| cam.to_screen(panel, egui::pos2(card.pos.x, card.pos.y)))
        } else {
            cam.to_screen(panel, egui::pos2(card.pos.x, card.pos.y))
        };

        let meta_opt = state.face_metas.iter().find(|m| m.id == card.id);
        let card_world_size = if let Some(meta) = meta_opt {
            crate::card::shape::card_size(crate::card::shape::shape_for(meta.status, meta.kind))
        } else {
            egui::vec2(300.0, 200.0)
        };
        // Scale card size by zoom to get screen-space dimensions.
        let card_screen_size = card_world_size * cam.zoom;
        let card_screen_rect = egui::Rect::from_min_size(card_screen_min, card_screen_size);

        // Culling — skip cards fully outside the expanded panel
        if !cull_rect.intersects(card_screen_rect) {
            continue;
        }

        let is_focused = *state.focus == Some(card.id);

        let (title, _first_line, links, tags, _is_scrap, source, shape, style) =
            if let Some(meta) = meta_opt {
                let shape = crate::card::shape::shape_for(meta.status, meta.kind);
                (
                    meta.title.as_str(),
                    meta.first_line.as_str(),
                    meta.links,
                    meta.tags,
                    meta.is_scrap,
                    meta.source.as_deref(),
                    shape,
                    crate::card::shape::CardStyle::Paper,
                )
            } else {
                (
                    "",
                    "",
                    0usize,
                    0usize,
                    true,
                    None,
                    crate::card::shape::CardShape::Scrap,
                    crate::card::shape::CardStyle::Paper,
                )
            };

        let body = state
            .bodies
            .get_or_request(card.id, state.commands)
            .map(|b| b.text.as_str());

        let face = crate::card::CardFace {
            id: card.id,
            title,
            body,
            shape,
            style,
            lines: crate::card::shape::RuledLines::Natural,
            source,
            links,
            tags,
            focused: is_focused,
        };

        let (resp, checkbox_ordinal) =
            crate::card::card_face(ui, card_screen_rect, &face, state.theme, state.line_cache);

        // Checkbox click-to-toggle: if the face detected a checkbox click, emit
        // the toggle event (ordinal identifies the Nth task box in the raw body).
        // Gated while the palette overlay is open (same discipline as the
        // double-click gate below): a click behind the palette must not mutate.
        if let Some(ordinal) = checkbox_ordinal
            && !state.palette_open
        {
            events.push(DeskEvent::ToggleTaskBox {
                id: card.id,
                ordinal,
            });
        } else if resp.clicked() && *state.focus != Some(card.id) {
            *state.focus = Some(card.id);
            events.push(DeskEvent::FocusChanged(*state.focus));
        }
        // Mouse paths are gated while the palette overlay is open (same
        // discipline as the keyboard block above): a double-click behind the
        // palette must not open the editor.
        if resp.double_clicked() && !state.palette_open {
            events.push(DeskEvent::OpenCard(card.id));
        }

        // Highlight pulse: fading ring around the card the palette panned to.
        if let Some((pulse_id, frac)) = state.highlight_pulse
            && pulse_id == card.id
        {
            let alpha = (1.0 - frac).clamp(0.0, 1.0);
            let expand = 4.0 + 8.0 * frac;
            ui.painter().rect_stroke(
                card_screen_rect.expand(expand),
                6.0,
                egui::Stroke::new(3.0, state.theme.focus_ring.gamma_multiply(alpha)),
                egui::StrokeKind::Outside,
            );
        }
        // Only hold keyboard focus on the card when the editor is closed.
        // While the editor modal is open, its TextEdit owns keyboard focus;
        // stealing it here every frame would prevent Event::Text from reaching
        // the TextEdit (desk renders inside CentralPanel, before the modal overlay).
        if is_focused && !state.editor_open && !state.palette_open {
            resp.request_focus();
        }

        // ── Card context menu ─────────────────────────────────────────────
        // Build desk name list for "Take to Desk ▸" submenu.
        let desk_refs: Vec<(jd_core::session::DeskId, &str)> = state
            .desks
            .iter()
            .map(|(id, name)| (*id, name.as_str()))
            .collect();

        let menu_ctx = crate::menus::CardMenuCtx {
            id: card.id,
            status: if let Some(m) = meta_opt {
                m.status
            } else {
                jd_core::note::Status::Fleeting
            },
            kind: if let Some(m) = meta_opt {
                m.kind
            } else {
                jd_core::note::Kind::Note
            },
            title: if let Some(m) = meta_opt {
                m.title.as_str()
            } else {
                ""
            },
            desks: &desk_refs,
            on_desk: true, // card is on a desk surface
            editor_open: state.editor_open,
            confirm_pending: state.confirm_pending,
            palette_open: state.palette_open,
        };

        // Right-click context menu via Response::context_menu.
        resp.context_menu(|ui| {
            if let Some(ev) = crate::menus::card_menu_items(ui, &menu_ctx) {
                events.push(DeskEvent::CardMenu(ev));
                ui.close();
            }
        });

        // Shift+F10 on the focused card → anchored Popup at card rect.
        // egui 0.35 cannot programmatically open a context_menu from keyboard,
        // so we use an anchored Popup as an equivalent.  The flag is stored in
        // egui memory (context_menu_open_id) and consumed here for the focused card.
        if is_focused {
            let wants_open: bool =
                ui.memory(|m| m.data.get_temp(context_menu_open_id()).unwrap_or(false));
            if wants_open {
                // Clear the flag so only one card opens the popup.
                ui.memory_mut(|m| m.data.insert_temp(context_menu_open_id(), false));
                ui.memory_mut(|m| m.data.insert_temp(card_popup_open_id(card.id), true));
            }

            let popup_open: bool = ui.memory(|m| {
                m.data
                    .get_temp(card_popup_open_id(card.id))
                    .unwrap_or(false)
            });

            if popup_open {
                let popup_id = egui::Id::new("card_context_popup").with(card.id);
                egui::Popup::from_response(&resp)
                    .id(popup_id)
                    .open(true)
                    .at_position(card_screen_rect.left_bottom())
                    .show(|ui| {
                        if let Some(ev) = crate::menus::card_menu_items(ui, &menu_ctx) {
                            events.push(DeskEvent::CardMenu(ev));
                            ui.memory_mut(|m| {
                                m.data.insert_temp(card_popup_open_id(card.id), false);
                            });
                        }
                    });

                // Close on click outside the popup (resp.clicked_elsewhere() fires when
                // the user clicks anywhere other than on this card's response area).
                if resp.clicked_elsewhere() {
                    ui.memory_mut(|m| {
                        m.data.insert_temp(card_popup_open_id(card.id), false);
                    });
                }

                // Close on Esc. consume_key prevents Esc from leaking to surface
                // handlers or other modals in the same frame (defense in depth).
                if ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape)) {
                    ui.memory_mut(|m| {
                        m.data.insert_temp(card_popup_open_id(card.id), false);
                    });
                }
            }
        } else {
            // Focus moved off this card — clear any stale popup flag so a later
            // refocus does not silently reopen the popup.
            let stale: bool = ui.memory(|m| {
                m.data
                    .get_temp(card_popup_open_id(card.id))
                    .unwrap_or(false)
            });
            if stale {
                ui.memory_mut(|m| {
                    m.data.insert_temp(card_popup_open_id(card.id), false);
                });
            }
        }
    }

    // ------------------------------------------------------------------
    // 6. Ghost fan (WP4 Task 5) — painted ABOVE the cards, at the anchor
    //    card's freest edge.
    //
    // Ghosts are PREVIEWS: they carry AccessKit labels ("Ghost: '<title>'")
    // so they are announced and clickable via assistive tech, but they are
    // deliberately NOT in the arrow-key reading order (reading_order walks
    // desk.cards only). Rationale: the spec's no-spatial-only law is already
    // satisfied — the palette and the Drawer are full keyboard paths to the
    // same notes — and adding transient previews to the focus ring would
    // make arrow traversal unstable as the fan recomputes every selection.
    // ------------------------------------------------------------------
    if let Some(anchor_id) = state.ghost_anchor
        && !state.ghosts.is_empty()
        && let Some(anchor_card) = desk.cards.iter().find(|c| c.id == anchor_id)
    {
        let anchor_size = state
            .face_metas
            .iter()
            .find(|m| m.id == anchor_id)
            .map(|m| crate::card::shape::card_size(crate::card::shape::shape_for(m.status, m.kind)))
            .unwrap_or_else(|| egui::vec2(300.0, 200.0));
        let anchor_rect = egui::Rect::from_min_size(
            egui::pos2(anchor_card.pos.x, anchor_card.pos.y),
            anchor_size,
        );
        let panel_world = egui::Rect::from_two_pos(
            cam.to_world(panel, panel.min),
            cam.to_world(panel, panel.max),
        );
        let mini_sizes: Vec<egui::Vec2> =
            state.ghosts.iter().map(|g| g.size * GHOST_SCALE).collect();
        let positions = ghost_fan_positions(anchor_rect, panel_world, &mini_sizes);

        for ((spec, world_pos), mini_size) in state.ghosts.iter().zip(&positions).zip(&mini_sizes) {
            let screen_min = cam.to_screen(panel, *world_pos);
            let rect = egui::Rect::from_min_size(screen_min, *mini_size * cam.zoom);
            let resp = ui.allocate_rect(rect, egui::Sense::click());
            let label = format!("Ghost: '{}'", spec.title);
            resp.widget_info(|| {
                egui::WidgetInfo::labeled(egui::WidgetType::Button, true, label.as_str())
            });

            // Faded mini = tinted rect + title (the sanctioned fallback,
            // §6.16): card_face has no opacity parameter and its laid-out
            // galleys can't be alpha-multiplied without re-tessellation, so
            // a 40%-alpha fill with a weak title string reads as "ghost"
            // more cheaply and more legibly than a washed-out full face.
            let painter = ui.painter().with_clip_rect(rect);
            painter.rect_filled(rect, 4.0, state.theme.card_plain_bg.gamma_multiply(0.4));
            painter.rect_stroke(
                rect,
                4.0,
                egui::Stroke::new(1.0, state.theme.card_border.gamma_multiply(0.6)),
                egui::StrokeKind::Inside,
            );
            painter.text(
                egui::pos2(rect.min.x + 6.0, rect.center().y),
                egui::Align2::LEFT_CENTER,
                spec.title.as_str(),
                egui::FontId::new(12.0 * cam.zoom.max(0.5), egui::FontFamily::Proportional),
                state.theme.text_weak,
            );

            // Click → the real card lands where the ghost stood (journaled
            // via place_card in app.rs). Gated while the palette is open,
            // same as the card mouse paths.
            if resp.clicked() && !state.palette_open {
                events.push(DeskEvent::GhostClicked {
                    id: spec.id,
                    pos: Vec2 {
                        x: world_pos.x,
                        y: world_pos.y,
                    },
                });
            }
        }
    }

    events
}

/// egui memory key for the Shift+F10 "open context menu on focused card" flag.
pub fn context_menu_open_id() -> egui::Id {
    egui::Id::new("desk_card_context_menu_open")
}

/// egui memory key for a specific card's keyboard-popup open state.
pub fn card_popup_open_id(id: NoteId) -> egui::Id {
    egui::Id::new("desk_card_popup_open").with(id)
}

/// Pan viewport to reveal `id` if it is currently off-screen.
///
/// `face_metas` is used to look up the per-shape card size so the visibility
/// check matches the rendered outline exactly.  When the meta is absent the
/// function falls back to the IndexCard size (300×200).
pub fn reveal(
    desk: &Desk,
    id: NoteId,
    panel: egui::Rect,
    face_metas: &[FaceMeta],
) -> Option<DeskCamera> {
    let card = desk.cards.iter().find(|c| c.id == id)?;
    let cam = DeskCamera {
        center: egui::vec2(desk.viewport.center.x, desk.viewport.center.y),
        zoom: desk.viewport.zoom,
    };
    let card_size = face_metas
        .iter()
        .find(|m| m.id == id)
        .map(|m| crate::card::shape::card_size(crate::card::shape::shape_for(m.status, m.kind)))
        .unwrap_or_else(|| egui::vec2(300.0, 200.0));
    let screen_min = cam.to_screen(panel, egui::pos2(card.pos.x, card.pos.y));
    // Screen-space rect: scale world size by zoom, matching the hit-test path.
    let card_screen = egui::Rect::from_min_size(screen_min, card_size * cam.zoom);
    if panel.contains_rect(card_screen) {
        return None; // already visible
    }
    // Center viewport on the card.
    let half = card_size * 0.5;
    let mut new_cam = cam;
    new_cam.center = egui::vec2(card.pos.x + half.x, card.pos.y + half.y);
    Some(new_cam)
}

#[cfg(test)]
mod tests {
    use super::*;
    use jd_core::geom::Vec2;
    use jd_core::id::NoteId;

    fn id(n: u8) -> NoteId {
        // NoteId::parse (no FromStr impl) from a 26-char ULID; build distinct ids cheaply.
        let s = format!("01ARZ3NDEKTSV4RRFFQ69G5F{n:02}");
        NoteId::parse(&s).unwrap_or_else(|_| panic!("bad test ulid {s}"))
    }

    #[test]
    fn reading_order_is_bands_then_x() {
        // Band height 120: y=10 and y=50 share band 0; y=200 is band 2.
        let cards = vec![
            (id(1), Vec2 { x: 300.0, y: 10.0 }),
            (id(2), Vec2 { x: 5.0, y: 50.0 }),
            (id(3), Vec2 { x: 0.0, y: 200.0 }),
        ];
        assert_eq!(reading_order(&cards), vec![id(2), id(1), id(3)]);
    }

    #[test]
    fn reading_order_stable_under_small_drags() {
        let before = vec![
            (id(1), Vec2 { x: 0.0, y: 100.0 }),
            (id(2), Vec2 { x: 200.0, y: 130.0 }),
        ];
        // y=100 and y=130 share band 1; after a ±20px wiggle (y=120, y=110) they still share band 1, so reading order is unchanged.
        let after = vec![
            (id(1), Vec2 { x: 0.0, y: 120.0 }),
            (id(2), Vec2 { x: 200.0, y: 110.0 }),
        ];
        assert_eq!(reading_order(&before), reading_order(&after));
    }

    #[test]
    fn arrows_traverse_and_do_not_wrap() {
        let cards = vec![
            (id(1), Vec2 { x: 0.0, y: 0.0 }),
            (id(2), Vec2 { x: 400.0, y: 0.0 }),
            (id(3), Vec2 { x: 100.0, y: 300.0 }),
        ];
        assert_eq!(next_focus(&cards, Some(id(1)), FocusDir::Left), None); // no wrap at first
        assert_eq!(
            next_focus(&cards, Some(id(1)), FocusDir::Right),
            Some(id(2))
        );
        assert_eq!(
            next_focus(&cards, Some(id(2)), FocusDir::Right),
            Some(id(3))
        ); // id(3) follows id(2) in reading order
        assert_eq!(next_focus(&cards, Some(id(3)), FocusDir::Right), None); // no wrap at last
        assert_eq!(next_focus(&cards, Some(id(1)), FocusDir::Down), Some(id(3)));
        assert_eq!(next_focus(&cards, Some(id(3)), FocusDir::Up), Some(id(1))); // nearest |Δx|
        assert_eq!(next_focus(&cards, None, FocusDir::Right), Some(id(1))); // no focus → first
    }

    // ── WP4 Task 5: ghost ranking + edges-on-select ─────────────────────

    /// Weight law pinned (architecture decision §6.16): direct link 3.0 >
    /// backlink 2.5 > shared tags 1.0 each; cosine blends in at 0–1.
    #[test]
    fn ghost_score_weights_pinned() {
        // Direct link beats backlink.
        assert!(ghost_score(true, false, 0, 0.0) > ghost_score(false, true, 0, 0.0));
        // Backlink beats two shared tags.
        assert!(ghost_score(false, true, 0, 0.0) > ghost_score(false, false, 2, 0.0));
        // Exact values.
        assert_eq!(ghost_score(true, false, 0, 0.0), 3.0);
        assert_eq!(ghost_score(false, true, 0, 0.0), 2.5);
        assert_eq!(ghost_score(false, false, 2, 0.0), 2.0);
        assert_eq!(ghost_score(false, false, 1, 0.0), 1.0);
        // Relations stack: direct + backlink + one tag.
        assert_eq!(ghost_score(true, true, 1, 0.0), 6.5);
        // Cosine is clamped to 0..1 and added.
        assert_eq!(ghost_score(false, false, 0, 2.0), 1.0);
        assert_eq!(ghost_score(false, false, 0, -1.0), 0.0);
    }

    mod ghost_index_tests {
        use super::super::*;
        use jd_core::index::Index;
        use jd_core::note::{Kind, NoteMeta, Status};
        use jd_core::tag::Tag;
        use jd_core::time::Timestamp;
        use std::collections::HashSet;

        fn gid(n: u8) -> NoteId {
            NoteId([n; 16])
        }

        fn meta(n: u8, title: &str, tags: &[&str], body: &str) -> NoteMeta {
            NoteMeta {
                id: gid(n),
                rel_path: format!("notes/{n}.md").into(),
                title: Some(title.to_owned()),
                first_line: title.to_owned(),
                status: Status::Permanent,
                kind: Kind::Note,
                source: None,
                created: Timestamp(n as i64 * 1000),
                modified: Timestamp(n as i64 * 1000),
                tags: tags.iter().filter_map(|t| Tag::new(t)).collect(),
                links_out: jd_core::doc::extract_links(body),
                word_count: 0,
            }
        }

        fn build(notes: &[(u8, &str, &[&str], &str)]) -> Index {
            let mut ix = Index::new();
            for (n, title, tags, body) in notes {
                ix.upsert(meta(*n, title, tags, body), body);
            }
            ix.refresh_similarity_cache();
            ix
        }

        /// Fixture: A(1) is the anchor. B is directly linked, C backlinks,
        /// D shares two tags, E shares one, F has no link/backlink/tag but
        /// shares a distinctive body token with A (cosine-only candidate).
        /// D and F have symmetric document frequencies so their cosines are
        /// equal; the 0.5 structural gap (tag vs no relation) is the tiebreaker.
        fn fixture() -> Index {
            build(&[
                (1, "Alpha", &["t1", "t2"], "[[Beta]] quantum"),
                (2, "Beta", &[], "banana"),
                (3, "Gamma", &[], "[[Alpha]]"),
                (4, "Delta", &["t1", "t2"], "durian"),
                (5, "Eps", &["t1"], "elder"),
                (6, "Phi", &[], "quantum flux"),
            ])
        }

        #[test]
        fn ghost_candidates_orders_by_pinned_weights() {
            let ix = fixture();
            let on_desk: HashSet<NoteId> = [gid(1)].into_iter().collect();
            let got = ghost_candidates(&ix, gid(1), &on_desk);
            let order: Vec<NoteId> = got.iter().map(|(id, _)| *id).collect();
            assert_eq!(
                order,
                vec![gid(2), gid(3), gid(4), gid(5), gid(6)],
                "direct link > backlink > two shared tags > one shared tag > cosine-only"
            );
            let score = |n: u8| got.iter().find(|(id, _)| *id == gid(n)).unwrap().1;
            // D and E share no body/title tokens with A: pure tag weights.
            assert_eq!(score(4), 2.0);
            assert_eq!(score(5), 1.0);
            // B/C: structural weight plus a small cosine (link-token overlap).
            assert!(score(2) >= 3.0 && score(2) < 4.0);
            assert!(score(3) >= 2.5 && score(3) < 3.5);
            // F: cosine-only, no structural relation. Score is cosine (0,1].
            assert!(score(6) > 0.0 && score(6) <= 1.0);
        }

        #[test]
        fn ghost_candidates_excludes_self_and_on_desk() {
            let ix = fixture();
            let on_desk: HashSet<NoteId> = [gid(1), gid(2)].into_iter().collect();
            let got = ghost_candidates(&ix, gid(1), &on_desk);
            assert!(got.iter().all(|(id, _)| *id != gid(1)), "self excluded");
            assert!(
                got.iter().all(|(id, _)| *id != gid(2)),
                "on-desk note excluded"
            );
            assert_eq!(got.first().map(|(id, _)| *id), Some(gid(3)));
        }

        #[test]
        fn ghost_candidates_caps_at_k() {
            // Anchor + 7 notes sharing one tag: only GHOST_K survive.
            let ix = build(&[
                (1, "Anchor", &["t"], "aa"),
                (2, "N2", &["t"], "bb"),
                (3, "N3", &["t"], "cc"),
                (4, "N4", &["t"], "dd"),
                (5, "N5", &["t"], "ee"),
                (6, "N6", &["t"], "ff"),
                (7, "N7", &["t"], "gg"),
                (8, "N8", &["t"], "hh"),
            ]);
            let on_desk: HashSet<NoteId> = [gid(1)].into_iter().collect();
            let got = ghost_candidates(&ix, gid(1), &on_desk);
            assert_eq!(got.len(), GHOST_K);
        }

        #[test]
        fn ghost_candidates_blends_cosine_only_neighbours() {
            // F(6) shares body terms with the anchor but has no links or tags:
            // it enters purely via the Index::similar cosine blend.
            let ix = build(&[
                (1, "Alpha", &[], "quantum flux capacitor"),
                (6, "Zeta", &[], "quantum flux and more words"),
                (7, "Other", &[], "unrelated entirely"),
            ]);
            let on_desk: HashSet<NoteId> = [gid(1)].into_iter().collect();
            let got = ghost_candidates(&ix, gid(1), &on_desk);
            let f = got.iter().find(|(id, _)| *id == gid(6));
            let (_, s) = f.expect("cosine-only neighbour must be a candidate");
            assert!(*s > 0.0 && *s <= 1.0, "cosine-only score in (0,1], got {s}");
        }

        #[test]
        fn selected_edges_dedup_both_directions_on_desk_only() {
            // A ↔ B mutual links (must dedup to one edge); A → C where C is
            // OFF the desk (no edge); D backlinks A (edge).
            let ix = build(&[
                (1, "Alpha", &[], "[[Beta]] and [[Ceta]]"),
                (2, "Beta", &[], "back at [[Alpha]]"),
                (3, "Ceta", &[], "off desk"),
                (4, "Delta", &[], "see [[Alpha]]"),
            ]);
            let on_desk: HashSet<NoteId> = [gid(1), gid(2), gid(4)].into_iter().collect();
            let edges = selected_edges(&ix, gid(1), &on_desk);
            assert_eq!(edges.len(), 2, "A–B (dedup) and A–D, not off-desk A–C");
            assert!(edges.contains(&(gid(1), gid(2))));
            assert!(edges.contains(&(gid(1), gid(4))));
        }

        #[test]
        fn selected_edges_empty_without_links() {
            let ix = build(&[(1, "Alpha", &[], "solo"), (2, "Beta", &[], "also solo")]);
            let on_desk: HashSet<NoteId> = [gid(1), gid(2)].into_iter().collect();
            assert!(selected_edges(&ix, gid(1), &on_desk).is_empty());
        }
    }

    #[test]
    fn ghost_fan_picks_freest_edge_and_stays_off_anchor() {
        // Anchor pushed toward the panel's east edge → fan goes WEST.
        let panel_world =
            egui::Rect::from_min_max(egui::pos2(-500.0, -400.0), egui::pos2(500.0, 400.0));
        let anchor = egui::Rect::from_min_size(egui::pos2(150.0, -100.0), egui::vec2(300.0, 200.0));
        let sizes = vec![egui::vec2(120.0, 80.0); 3];
        let pos = ghost_fan_positions(anchor, panel_world, &sizes);
        assert_eq!(pos.len(), 3);
        for (p, s) in pos.iter().zip(&sizes) {
            let r = egui::Rect::from_min_size(*p, *s);
            assert!(
                !r.intersects(anchor),
                "ghost {r:?} must not overlap the anchor"
            );
            assert!(
                r.max.x <= anchor.min.x,
                "fan must be on the west (freest) side"
            );
        }
        // Anchor near the north edge → fan goes SOUTH.
        let anchor_n =
            egui::Rect::from_min_size(egui::pos2(-150.0, -390.0), egui::vec2(300.0, 200.0));
        let pos_n = ghost_fan_positions(anchor_n, panel_world, &sizes);
        for (p, s) in pos_n.iter().zip(&sizes) {
            let r = egui::Rect::from_min_size(*p, *s);
            assert!(r.min.y >= anchor_n.max.y, "fan must be on the south side");
        }
    }

    #[test]
    fn a11y_labels_match_spec() {
        assert_eq!(
            card_a11y_label(
                "Immediate mode trades layout power for state simplicity",
                "",
                false,
                3,
                2
            ),
            "Card: 'Immediate mode trades layout power for state simplicity', 3 links, 2 tags"
        );
        assert_eq!(
            card_a11y_label("T", "", false, 1, 0),
            "Card: 'T', 1 link, 0 tags"
        );
        assert_eq!(
            card_a11y_label("", "buy milk", true, 0, 0),
            "Scrap: 'buy milk'"
        );
    }
}
