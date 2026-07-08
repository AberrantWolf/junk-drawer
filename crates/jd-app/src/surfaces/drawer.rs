//! Drawer surface (WP4 Task 4): storage for wandering — a dense grid of card
//! minis plus a filter-chips row.
//!
//! Layout:
//! - Chips row at the top (the row IS the query, always visible): status
//!   (Scraps/Cards), kind (Notes/Literature/Dividers), a tag picker, Unlinked,
//!   Needs Attention. Chips are toggle buttons with an × affordance and
//!   compose with AND; each is AccessKit-labeled "Filter: <name>, <state>".
//! - Below: the mini grid — the SAME card widgets as the desk (`card_face`)
//!   at `MINI_SCALE` (0.6). Fonts inside card_face are fixed-size, so a 60%
//!   rect crops to fewer lines but stays legible (verified by eye + the a11y
//!   tree carries the full face label regardless).
//! - Ordering: newest-modified first (`drawer_ids`, index meta modified desc);
//!   ids + FaceMeta are prefetched in app.rs under ONE index read lock per
//!   frame (the FaceMeta idiom).
//! - Needs Attention also renders quarantined files (not in the index) as
//!   inert rows: "Quarantined: '<filename>' — <reason>" — no face, plain
//!   labels, not focusable-to-open. Quarantined rows IGNORE the other chips:
//!   they aren't indexed notes, so status/kind/tag/Unlinked cannot apply —
//!   the Needs Attention chip alone shows or hides them.
//!
//! Keyboard (gated on editor/confirm/palette overlays and the popups):
//!   Up/Left / Down/Right → linear focus over the grid (list order IS
//!   row-major grid order); Enter → open editor in place; Ctrl+D → desk
//!   picker (shared component, surfaces/desk_picker.rs).

use std::collections::HashSet;

use eframe::egui;
use jd_core::id::NoteId;
use jd_core::index::Index;
use jd_core::note::{Kind, Status};
use jd_core::session::{DeskId, SessionState};
use jd_core::tag::Tag;
use jd_core::vault::scan::QuarantinedFile;

use crate::card::shape::{CardStyle, RuledLines, card_size, shape_for};
use crate::surfaces::desk::FaceMeta;

/// Mini faces render at this fraction of the desk card size.
pub const MINI_SCALE: f32 = 0.6;

// ---------------------------------------------------------------------------
// DrawerEvent
// ---------------------------------------------------------------------------

/// Events emitted by `drawer_ui` for `app.rs` to apply.
#[derive(Debug)]
pub enum DrawerEvent {
    /// Open the card editor for `id` (Enter / double-click) — the existing
    /// open path; the editor overlay is surface-agnostic.
    OpenCard(NoteId),
    /// Place the card on `desk` at that desk's viewport center (Ctrl+D picker).
    PlaceOnDesk { id: NoteId, desk: DeskId },
}

// ---------------------------------------------------------------------------
// DrawerFilters
// ---------------------------------------------------------------------------

/// The chip row's state — active chips compose with AND. Lives in `UiState`
/// (view state, like `focus`: not session, not journaled).
#[derive(Clone, Default)]
pub struct DrawerFilters {
    /// Status chips.
    pub scraps: bool,
    pub cards: bool,
    /// Kind chips.
    pub notes: bool,
    pub literature: bool,
    pub dividers: bool,
    /// Tag chip (picked from the tag popup; None = chip not active).
    pub tag: Option<Tag>,
    /// No outgoing links AND no backlinks.
    pub unlinked: bool,
    /// Conflicted notes (in the index) + quarantined files (inert rows).
    pub needs_attention: bool,
}

impl DrawerFilters {
    pub fn any_active(&self) -> bool {
        self.scraps
            || self.cards
            || self.notes
            || self.literature
            || self.dividers
            || self.tag.is_some()
            || self.unlinked
            || self.needs_attention
    }
}

/// The Drawer's note list: every indexed note passing all active chips,
/// newest-modified first (tie: created desc, then id for determinism).
///
/// Call under the caller's index read lock (the ONE-lock-per-frame idiom);
/// `conflicts` is `UiState::conflicts` (Needs Attention, Task 3).
pub fn drawer_ids(idx: &Index, filters: &DrawerFilters, conflicts: &[NoteId]) -> Vec<NoteId> {
    // Unlinked is a whole-index computation; do it once, only when active.
    let unlinked: Option<HashSet<NoteId>> = filters
        .unlinked
        .then(|| idx.unlinked().into_iter().collect());

    let mut metas: Vec<_> = idx
        .iter_meta()
        .filter(|m| {
            if filters.scraps && m.status != Status::Fleeting {
                return false;
            }
            if filters.cards && m.status != Status::Permanent {
                return false;
            }
            if filters.notes && m.kind != Kind::Note {
                return false;
            }
            if filters.literature && m.kind != Kind::Literature {
                return false;
            }
            if filters.dividers && m.kind != Kind::Structure {
                return false;
            }
            if let Some(tag) = &filters.tag
                && !m.tags.iter().any(|t| t.matches(tag))
            {
                return false;
            }
            if let Some(u) = &unlinked
                && !u.contains(&m.id)
            {
                return false;
            }
            if filters.needs_attention && !conflicts.contains(&m.id) {
                return false;
            }
            true
        })
        .collect();
    metas.sort_by(|a, b| {
        b.modified
            .cmp(&a.modified)
            .then(b.created.cmp(&a.created))
            .then(a.id.cmp(&b.id))
    });
    metas.into_iter().map(|m| m.id).collect()
}

// ---------------------------------------------------------------------------
// DrawerUiDeps
// ---------------------------------------------------------------------------

/// Everything `drawer_ui` reads but does not own.
pub struct DrawerUiDeps<'a> {
    pub focus: &'a mut Option<NoteId>,
    pub bodies: &'a mut crate::state::BodyCache,
    pub commands: &'a std::sync::mpsc::Sender<jd_core::worker::VaultCommand>,
    pub theme: &'a crate::theme::Theme,
    pub line_cache: &'a mut crate::editor::LineCache,
    /// FaceMeta for the filtered, ordered notes — prefetched under ONE index
    /// read lock in app.rs, in the same order as `ordered_ids`.
    pub face_metas: &'a [FaceMeta],
    /// Filtered note ids, newest-modified first (`drawer_ids`).
    pub ordered_ids: &'a [NoteId],
    /// Chip state — mutated in place by chip toggles (takes effect on the
    /// next frame's `drawer_ids` prefetch; the one-frame-lag idiom).
    pub filters: &'a mut DrawerFilters,
    /// All tags with member counts (`Index::all_tags`, count desc) for the
    /// tag picker popup — prefetched under the same lock.
    pub all_tags: &'a [(Tag, usize)],
    /// Files the last scan could not read (Needs Attention inert rows).
    pub quarantined: &'a [QuarantinedFile],
    /// Current session state — for the desk picker popup.
    pub session: &'a SessionState,
    pub editor_open: bool,
    /// True while a delete-confirm modal is pending (suppresses surface keys).
    pub confirm_pending: bool,
    /// True while the Ctrl+K palette overlay is open (same gate pattern).
    pub palette_open: bool,
}

// ---------------------------------------------------------------------------
// Popup state ids (egui memory)
// ---------------------------------------------------------------------------

fn picker_state_id() -> egui::Id {
    egui::Id::new("drawer_desk_picker")
}

fn tag_popup_id() -> egui::Id {
    egui::Id::new("drawer_tag_popup")
}

fn tag_popup_open(ui: &egui::Ui) -> bool {
    ui.memory(|m| m.data.get_temp::<bool>(tag_popup_id()))
        .unwrap_or(false)
}

fn set_tag_popup_open(ui: &egui::Ui, open: bool) {
    ui.memory_mut(|m| m.data.insert_temp(tag_popup_id(), open));
}

// ---------------------------------------------------------------------------
// Chips row
// ---------------------------------------------------------------------------

/// One toggle chip. Shows "<name> ×" while active; the whole chip is the
/// toggle (clicking an active chip dismisses it). AccessKit label:
/// "Filter: <name>, active|inactive".
fn chip(ui: &mut egui::Ui, name: &str, active: &mut bool) {
    let text = if *active {
        format!("{name} ×")
    } else {
        name.to_owned()
    };
    let a11y = format!(
        "Filter: {name}, {}",
        if *active { "active" } else { "inactive" }
    );
    let resp = ui.selectable_label(*active, text);
    resp.widget_info(|| {
        egui::WidgetInfo::labeled(egui::WidgetType::Button, *active, a11y.as_str())
    });
    if resp.clicked() {
        *active = !*active;
    }
}

/// The tag chip: "Tag…" opens the picker popup; while a tag is active the
/// chip shows "#tag ×" and clicking it dismisses the tag filter.
fn tag_chip(ui: &mut egui::Ui, filters: &mut DrawerFilters) {
    if let Some(tag) = filters.tag.clone() {
        let a11y = format!("Filter: #{}, active", tag.as_str());
        let resp = ui.selectable_label(true, format!("#{} ×", tag.as_str()));
        resp.widget_info(|| {
            egui::WidgetInfo::labeled(egui::WidgetType::Button, true, a11y.as_str())
        });
        if resp.clicked() {
            filters.tag = None;
        }
    } else {
        let resp = ui.selectable_label(false, "Tag…");
        resp.widget_info(|| {
            egui::WidgetInfo::labeled(egui::WidgetType::Button, false, "Filter: Tag picker")
        });
        if resp.clicked() {
            set_tag_popup_open(ui, true);
        }
    }
}

/// Tag picker popup: all tags with counts, one clickable row per tag
/// (a11y "Tag: #<tag> (<count>)"). Esc or picking closes it.
fn tag_popup_ui(
    ui: &mut egui::Ui,
    panel: egui::Rect,
    all_tags: &[(Tag, usize)],
    theme: &crate::theme::Theme,
) -> Option<Tag> {
    let mut picked: Option<Tag> = None;

    if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
        set_tag_popup_open(ui, false);
        return None;
    }

    let popup_width = 280.0_f32;
    let row_h = 28.0_f32;
    let popup_h = 8.0 + (all_tags.len().max(1)) as f32 * row_h + 8.0;
    let popup_rect = egui::Rect::from_center_size(panel.center(), egui::vec2(popup_width, popup_h));

    // Allocate interactive rects first (mutable borrow of ui).
    let mut rows: Vec<(egui::Rect, String)> = Vec::new();
    for (i, (tag, count)) in all_tags.iter().enumerate() {
        let row_rect = egui::Rect::from_min_size(
            egui::pos2(popup_rect.min.x, popup_rect.min.y + 8.0 + i as f32 * row_h),
            egui::vec2(popup_width, row_h),
        );
        let text = format!("#{} ({count})", tag.as_str());
        let label = format!("Tag: {text}");
        let resp = ui.allocate_rect(row_rect, egui::Sense::click());
        resp.widget_info(|| {
            egui::WidgetInfo::labeled(egui::WidgetType::Button, false, label.as_str())
        });
        if resp.clicked() {
            picked = Some(tag.clone());
        }
        rows.push((row_rect, text));
    }

    // Paint after allocation (painter borrows ui immutably).
    {
        let painter = ui.painter();
        painter.rect_filled(popup_rect, 6.0, theme.card_plain_bg);
        painter.rect_stroke(
            popup_rect,
            6.0,
            egui::Stroke::new(1.0, theme.card_border),
            egui::StrokeKind::Outside,
        );
        let font = egui::FontId::new(14.0, egui::FontFamily::Proportional);
        if all_tags.is_empty() {
            painter.text(
                egui::pos2(popup_rect.min.x + 12.0, popup_rect.center().y),
                egui::Align2::LEFT_CENTER,
                "No tags",
                font,
                theme.text,
            );
        } else {
            for (row_rect, text) in &rows {
                painter.text(
                    egui::pos2(row_rect.min.x + 12.0, row_rect.center().y),
                    egui::Align2::LEFT_CENTER,
                    text.as_str(),
                    font.clone(),
                    theme.text,
                );
            }
        }
    }

    if picked.is_some() {
        set_tag_popup_open(ui, false);
    }
    picked
}

// ---------------------------------------------------------------------------
// Grid layout
// ---------------------------------------------------------------------------

/// Grid cell: the largest face footprint (Literature, 300×224) at MINI_SCALE,
/// so every shape fits and rows stay aligned.
fn cell_size() -> egui::Vec2 {
    card_size(crate::card::shape::CardShape::Literature) * MINI_SCALE
}

const GRID_GAP: f32 = 16.0;
const GRID_MARGIN: f32 = 16.0;

/// Top-left position of the mini at `index`, row-major over `cols` columns,
/// with the grid starting at `top` (below the chips row).
fn mini_pos(index: usize, cols: usize, panel: egui::Rect, top: f32) -> egui::Pos2 {
    let cell = cell_size();
    let col = index % cols;
    let row = index / cols;
    egui::pos2(
        panel.min.x + GRID_MARGIN + col as f32 * (cell.x + GRID_GAP),
        top + GRID_MARGIN + row as f32 * (cell.y + GRID_GAP),
    )
}

fn grid_cols(panel: egui::Rect) -> usize {
    let cell = cell_size();
    (((panel.width() - GRID_MARGIN) / (cell.x + GRID_GAP)).floor() as usize).max(1)
}

// ---------------------------------------------------------------------------
// drawer_ui
// ---------------------------------------------------------------------------

/// Render the Drawer surface; returns events for app.rs to apply.
/// `drawer_ui` never mutates session state — it only emits events (chip
/// toggles mutate `deps.filters`, which is view state like `focus`).
pub fn drawer_ui(ui: &mut egui::Ui, deps: &mut DrawerUiDeps<'_>) -> Vec<DrawerEvent> {
    let mut events: Vec<DrawerEvent> = Vec::new();
    let panel = ui.max_rect();

    // -------------------------------------------------------------------
    // Chips row (always visible at the top — the row IS the query).
    // -------------------------------------------------------------------
    ui.horizontal_wrapped(|ui| {
        chip(ui, "Scraps", &mut deps.filters.scraps);
        chip(ui, "Cards", &mut deps.filters.cards);
        ui.separator();
        chip(ui, "Notes", &mut deps.filters.notes);
        chip(ui, "Literature", &mut deps.filters.literature);
        chip(ui, "Dividers", &mut deps.filters.dividers);
        ui.separator();
        tag_chip(ui, deps.filters);
        chip(ui, "Unlinked", &mut deps.filters.unlinked);
        chip(ui, "Needs Attention", &mut deps.filters.needs_attention);
    });
    ui.separator();
    let grid_top = ui.cursor().min.y;

    let picker_open = crate::surfaces::desk_picker::is_open(ui, picker_state_id());
    let popup_open = tag_popup_open(ui);

    // -------------------------------------------------------------------
    // Keyboard (gated on overlays + our own popups): linear focus over the
    // grid (list order is row-major), Enter opens, Ctrl+D → desk picker.
    // -------------------------------------------------------------------
    let ids = deps.ordered_ids;
    if !deps.editor_open
        && !deps.confirm_pending
        && !deps.palette_open
        && !picker_open
        && !popup_open
    {
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
            events.push(DrawerEvent::OpenCard(id));
        }

        // Ctrl+D → desk picker for the focused mini.
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
            crate::surfaces::desk_picker::open_for(ui, picker_state_id(), id);
        }
    }

    // -------------------------------------------------------------------
    // Needs Attention: quarantined files render as inert rows — they are
    // not in the index, so no face; plain labels, not focusable-to-open.
    // -------------------------------------------------------------------
    let mut y = grid_top;
    if deps.filters.needs_attention {
        ui.add_space(4.0);
        for q in deps.quarantined {
            let filename = q
                .rel_path
                .file_name()
                .map(|f| f.to_string_lossy().into_owned())
                .unwrap_or_else(|| q.rel_path.display().to_string());
            ui.label(format!("Quarantined: '{}' — {}", filename, q.error));
        }
        y = ui.cursor().min.y;
    }

    // -------------------------------------------------------------------
    // Mini grid: the SAME card widgets as the desk at MINI_SCALE, in
    // ordered_ids order (newest-modified first), row-major.
    // -------------------------------------------------------------------
    let cols = grid_cols(panel);
    // face_metas is prefetched in app.rs IN ordered_ids order under the same
    // index read lock, so a straight zip replaces the old per-id O(n²) find.
    for (idx, (&id, meta)) in ids.iter().zip(deps.face_metas.iter()).enumerate() {
        debug_assert_eq!(meta.id, id, "face_metas must be in ordered_ids order");
        let shape = shape_for(meta.status, meta.kind);
        let size = card_size(shape) * MINI_SCALE;
        let top_left = mini_pos(idx, cols, panel, y);
        let rect = egui::Rect::from_min_size(top_left, size);

        // Cull minis fully outside the panel.
        if !panel.expand(200.0).intersects(rect) {
            continue;
        }

        let is_focused = *deps.focus == Some(id);
        let body_str = deps
            .bodies
            .get_or_request(id, deps.commands)
            .map(|b| b.text.as_str());

        let face = crate::card::CardFace {
            id,
            title: meta.title.as_str(),
            body: body_str,
            shape,
            style: CardStyle::Paper,
            lines: RuledLines::Natural,
            source: meta.source.as_deref(),
            links: meta.links,
            tags: meta.tags,
            focused: is_focused,
        };
        let (resp, _checkbox_ordinal) =
            crate::card::card_face(ui, rect, &face, deps.theme, deps.line_cache);
        // Drawer minis: checkbox click-to-toggle intentionally not wired
        // (same rationale as the Inbox — edit via the editor instead).

        if resp.clicked() && *deps.focus != Some(id) {
            *deps.focus = Some(id);
        }
        if resp.double_clicked() && !deps.palette_open && !popup_open && !picker_open {
            events.push(DrawerEvent::OpenCard(id));
        }
        if is_focused && !deps.editor_open && !deps.palette_open && !popup_open && !picker_open {
            resp.request_focus();
        }
    }

    if ids.is_empty() && !deps.filters.needs_attention {
        let msg = if deps.filters.any_active() {
            "No cards match the active filters"
        } else {
            "The drawer is empty"
        };
        ui.centered_and_justified(|ui| {
            ui.label(egui::RichText::new(msg).weak().size(16.0));
        });
    }

    // -------------------------------------------------------------------
    // Popups (rendered above the grid).
    // -------------------------------------------------------------------
    if popup_open && let Some(tag) = tag_popup_ui(ui, panel, deps.all_tags, deps.theme) {
        deps.filters.tag = Some(tag);
    }
    if picker_open
        && let Some((id, desk)) = crate::surfaces::desk_picker::desk_picker_ui(
            ui,
            picker_state_id(),
            panel,
            deps.session,
            deps.theme,
        )
    {
        events.push(DrawerEvent::PlaceOnDesk { id, desk });
    }

    events
}
