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
    /// All desks (id + name) for the "Take to Desk ▸" submenu.
    pub desks: &'a [(jd_core::session::DeskId, String)],
    /// The current desk id — used to determine whether a card is "on a desk"
    /// (so Put Away is enabled when desk surface is active).
    pub current_desk_id: jd_core::session::DeskId,
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

    if !state.editor_open && !state.confirm_pending && !card_popup_open {
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

        // Enter → open focused card
        if ui.input(|i| i.key_pressed(egui::Key::Enter))
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

    // Drag start
    if primary_pressed
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

    // Drag release → emit Move if beyond threshold
    #[allow(clippy::collapsible_if)]
    if primary_released && let Some(drag) = state.drag.take() {
        if drag.total_delta >= 4.0
            && let Some(ptr) = pointer_pos
        {
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

        let resp =
            crate::card::card_face(ui, card_screen_rect, &face, state.theme, state.line_cache);

        if resp.clicked() && *state.focus != Some(card.id) {
            *state.focus = Some(card.id);
            events.push(DeskEvent::FocusChanged(*state.focus));
        }
        if resp.double_clicked() {
            events.push(DeskEvent::OpenCard(card.id));
        }
        // Only hold keyboard focus on the card when the editor is closed.
        // While the editor modal is open, its TextEdit owns keyboard focus;
        // stealing it here every frame would prevent Event::Text from reaching
        // the TextEdit (desk renders inside CentralPanel, before the modal overlay).
        if is_focused && !state.editor_open {
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
                        // Close on click outside (egui handles this via Popup's focus rules).
                    });

                // Check if popup should close (click outside or Esc).
                // consume_key prevents Esc from leaking to surface handlers or
                // other modals in the same frame (defense in depth).
                if ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape)) {
                    ui.memory_mut(|m| {
                        m.data.insert_temp(card_popup_open_id(card.id), false);
                    });
                }
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
