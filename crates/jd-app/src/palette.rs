//! Ctrl+K palette (WP4 Task 1): overlay with a query input and three strata —
//! title fuzzy matches, body search hits, and an always-last "New scrap" row.
//!
//! Events-out discipline: the palette mutates nothing but its own PaletteState;
//! app.rs owns open/close and applies row activation (Task 2: place /
//! pan-to-existing / place-and-open / new scrap).

use eframe::egui;
use jd_core::id::NoteId;
use jd_core::index::Index;
use jd_core::index::fuzzy::{FuzzyScore, fuzzy_match};
use jd_core::index::search::{Snippet, make_snippet, parse_query};
use jd_core::worker::VaultCommand;

use crate::card::shape::{card_size, shape_for};
use crate::state::BodyCache;

/// Per-stratum result cap (spec: ~8).
const STRATUM_CAP: usize = 8;
/// Snippet radius in bytes each side of the first match (spec: ~30).
const SNIPPET_RADIUS: usize = 30;
/// The query syntax help, shown verbatim when the query is empty (spec §7).
pub const SYNTAX_HELP: &str = "plain words (AND) · \"quoted phrases\" · #tag · -word";

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

/// One palette result row. Strata order in `results` is always
/// Title* < Body* < NewScrap.
#[derive(Clone, Debug)]
pub enum PaletteRow {
    /// Stratum 1: fuzzy title match.
    Title { id: NoteId, score: FuzzyScore },
    /// Stratum 2: body search hit (deduped against stratum 1). The snippet is
    /// blank while the body is still loading (BodyCache discipline).
    Body { id: NoteId, snippet: Snippet },
    /// Stratum 3: always-last "New scrap: '<query>'" (query nonempty).
    NewScrap,
}

/// What the palette asked app.rs to do this frame.
#[derive(Clone, Debug)]
pub enum PaletteEvent {
    /// Esc — close the palette, nothing else.
    Close,
    /// Enter / Ctrl+Enter on the selected row. `open_after` = Ctrl held
    /// (place-or-pan AND open the editor). app.rs closes the palette after
    /// applying the activation.
    Activate { row: PaletteRow, open_after: bool },
}

pub struct PaletteState {
    pub query: String,
    pub selected: usize,
    pub results: Vec<PaletteRow>,
    /// The query `results` was computed for. Results are recomputed only when
    /// the query changed or a stratum-2 snippet is still pending (body load).
    last_query: Option<String>,
    /// One-shot: the input grabs keyboard focus on the first rendered frame.
    wants_focus: bool,
}

impl PaletteState {
    pub fn new() -> PaletteState {
        PaletteState {
            query: String::new(),
            selected: 0,
            results: Vec::new(),
            last_query: None,
            wants_focus: true,
        }
    }

    /// Ids of the current result rows (Title + Body strata; NewScrap carries
    /// no id). Lightweight read for the map's palette-dim highlight (WP5).
    pub fn result_ids(&self) -> Vec<NoteId> {
        self.results
            .iter()
            .filter_map(|r| match r {
                PaletteRow::Title { id, .. } | PaletteRow::Body { id, .. } => Some(*id),
                PaletteRow::NewScrap => None,
            })
            .collect()
    }
}

impl Default for PaletteState {
    fn default() -> Self {
        PaletteState::new()
    }
}

pub struct PaletteDeps<'a> {
    pub index: &'a jd_core::index::SharedIndex,
    pub bodies: &'a mut BodyCache,
    pub commands: &'a std::sync::mpsc::Sender<VaultCommand>,
    pub theme: &'a crate::theme::Theme,
}

// ---------------------------------------------------------------------------
// Result computation
// ---------------------------------------------------------------------------

/// Compute the three strata for `query`. Called once per frame under ONE
/// index read lock (cheap at our scale — recompute-on-render also picks up
/// Body events arriving between frames, so snippets fill in as bodies load).
///
/// - Stratum 1: fuzzy over titles (untitled scraps skipped — no title),
///   ranked by (tier, score desc) with recency tiebreak (modified desc),
///   capped at STRATUM_CAP.
/// - Stratum 2: `Index::query(parse_query(query))`, ids already in stratum 1
///   deduped out; snippet via `make_snippet` when the body is cached
///   (`get_or_request`; blank while loading — same discipline as faces).
/// - Stratum 3: NewScrap, always last, when the query is nonempty.
pub fn compute_results(
    idx: &Index,
    query: &str,
    bodies: &mut BodyCache,
    commands: &std::sync::mpsc::Sender<VaultCommand>,
) -> Vec<PaletteRow> {
    let query = query.trim();
    if query.is_empty() {
        return Vec::new();
    }

    // Stratum 1 — fuzzy over titles.
    let mut titled: Vec<(&jd_core::note::NoteMeta, FuzzyScore)> = idx
        .iter_meta()
        .filter_map(|m| {
            let title = m.title.as_deref()?; // untitled scraps: no stratum 1
            fuzzy_match(query, title).map(|s| (m, s))
        })
        .collect();
    // Ordering contract (fuzzy.rs): lower tier better, then higher score;
    // recency tiebreak: modified desc; id asc pins full determinism.
    titled.sort_by(|(ma, sa), (mb, sb)| {
        sa.tier
            .cmp(&sb.tier)
            .then_with(|| sb.score.cmp(&sa.score))
            .then_with(|| mb.modified.cmp(&ma.modified))
            .then_with(|| ma.id.cmp(&mb.id))
    });
    titled.truncate(STRATUM_CAP);

    let mut rows: Vec<PaletteRow> = titled
        .iter()
        .map(|(m, score)| PaletteRow::Title {
            id: m.id,
            score: score.clone(),
        })
        .collect();
    let stratum1_ids: std::collections::HashSet<NoteId> =
        titled.iter().map(|(m, _)| m.id).collect();

    // Stratum 2 — body search, deduped against stratum 1.
    let q = parse_query(query);
    for hit in idx.query(&q, STRATUM_CAP) {
        if stratum1_ids.contains(&hit.id) {
            continue;
        }
        // Blank snippet while the body loads; the Body event triggers a
        // repaint and the next recompute fills it in.
        let snippet = match bodies.get_or_request(hit.id, commands) {
            Some(cached) => make_snippet(&cached.text, &hit.matched_terms, SNIPPET_RADIUS),
            None => Snippet {
                text: String::new(),
                highlights: Vec::new(),
            },
        };
        rows.push(PaletteRow::Body {
            id: hit.id,
            snippet,
        });
    }

    // Stratum 3 — always-last New scrap row.
    rows.push(PaletteRow::NewScrap);
    rows
}

// ---------------------------------------------------------------------------
// UI
// ---------------------------------------------------------------------------

/// Row display data snapshotted from NoteMeta under the same index lock.
struct RowView {
    label: String,
    heading: String,
    detail: Option<String>,
    tags: Vec<String>,
    status: jd_core::note::Status,
    kind: jd_core::note::Kind,
}

/// Render the palette overlay. Returns the event the palette asked app.rs to
/// apply this frame (Esc → Close; Enter/Ctrl+Enter or a row click →
/// Activate, Cmd-click = open_after), if any.
/// Ctrl+K toggling and the open gate live in app.rs.
pub fn palette_ui(
    ui: &mut egui::Ui,
    pal: &mut PaletteState,
    deps: &mut PaletteDeps<'_>,
) -> Option<PaletteEvent> {
    // ── Key handling (consumed BEFORE the TextEdit sees them — the editor's
    //    popup-first pattern; Esc must close ONLY the palette). ──
    let close = ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape));
    let down = ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown));
    let up = ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp));
    // Ctrl+Enter FIRST (consume_key is exact on modifiers, but checking the
    // modified chord before the plain one keeps the intent obvious).
    let enter_open = ui.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, egui::Key::Enter));
    let enter = ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Enter));

    let mut event: Option<PaletteEvent> = if close {
        Some(PaletteEvent::Close)
    } else {
        None
    };

    let panel_width = 560.0_f32;
    egui::Area::new(egui::Id::new("palette_overlay"))
        .order(egui::Order::Foreground)
        .anchor(egui::Align2::CENTER_TOP, egui::vec2(0.0, 72.0))
        .show(ui.ctx(), |ui| {
            egui::Frame::default()
                .fill(deps.theme.card_plain_bg)
                .stroke(egui::Stroke::new(1.0, deps.theme.card_border))
                .corner_radius(8.0)
                .shadow(egui::Shadow {
                    offset: [0, 4],
                    blur: 16,
                    spread: 0,
                    color: egui::Color32::from_black_alpha(80),
                })
                .inner_margin(egui::Margin::same(10))
                .show(ui, |ui| {
                    ui.set_width(panel_width);

                    // ── Query input (grabs focus on open) ──
                    let te = ui.add(
                        egui::TextEdit::singleline(&mut pal.query)
                            .hint_text("Find or create…")
                            .desired_width(f32::INFINITY),
                    );
                    if pal.wants_focus {
                        te.request_focus();
                        pal.wants_focus = false;
                    }

                    // ── Results: recompute under ONE index read lock (which
                    //    also serves the row renderer's meta lookups), but
                    //    ONLY when the query changed or a stratum-2 snippet is
                    //    still pending (blank while its body loads — the Body
                    //    event repaints and this branch fills it in). ──
                    let idx = deps.index.read().unwrap();
                    let query_changed = pal.last_query.as_deref() != Some(pal.query.as_str());
                    let snippet_pending = pal.results.iter().any(|r| {
                        matches!(r, PaletteRow::Body { snippet, .. } if snippet.text.is_empty())
                    });
                    if query_changed || snippet_pending {
                        pal.results = compute_results(&idx, &pal.query, deps.bodies, deps.commands);
                        pal.last_query = Some(pal.query.clone());
                        if query_changed {
                            pal.selected = 0;
                        }
                    }

                    // Selection movement + clamp.
                    if !pal.results.is_empty() {
                        if down {
                            pal.selected = (pal.selected + 1).min(pal.results.len() - 1);
                        }
                        if up {
                            pal.selected = pal.selected.saturating_sub(1);
                        }
                        pal.selected = pal.selected.min(pal.results.len() - 1);
                    } else {
                        pal.selected = 0;
                    }

                    // Enter activates the selected row (Ctrl+Enter = also open
                    // the editor). Esc wins if both arrived in one frame.
                    if (enter || enter_open)
                        && event.is_none()
                        && let Some(row) = pal.results.get(pal.selected)
                    {
                        event = Some(PaletteEvent::Activate {
                            row: row.clone(),
                            open_after: enter_open,
                        });
                    }

                    ui.add_space(8.0);

                    if pal.query.trim().is_empty() {
                        // Empty palette: the query syntax help, verbatim.
                        ui.label(SYNTAX_HELP);
                        return;
                    }

                    // Snapshot row views under the lock, then render.
                    let views: Vec<RowView> = pal
                        .results
                        .iter()
                        .map(|row| row_view(row, &idx, &pal.query))
                        .collect();
                    drop(idx);

                    egui::ScrollArea::vertical()
                        .max_height(420.0)
                        .show(ui, |ui| {
                            for (i, view) in views.iter().enumerate() {
                                let resp = render_row(
                                    ui,
                                    view,
                                    i == pal.selected,
                                    panel_width,
                                    deps.theme,
                                );
                                // Mouse activation: a click on a row acts like
                                // Enter on it (Cmd-click = also open the
                                // editor, mirroring Ctrl+Enter).
                                if resp.clicked()
                                    && event.is_none()
                                    && let Some(row) = pal.results.get(i)
                                {
                                    event = Some(PaletteEvent::Activate {
                                        row: row.clone(),
                                        open_after: ui.input(|inp| inp.modifiers.command),
                                    });
                                }
                            }
                        });
                });
        });

    event
}

/// Build the display snapshot for one row (index lock held by the caller).
fn row_view(row: &PaletteRow, idx: &Index, query: &str) -> RowView {
    match row {
        PaletteRow::Title { id, .. } | PaletteRow::Body { id, .. } => {
            let meta = idx.get(*id);
            let heading = meta
                .map(|m| m.title.clone().unwrap_or_else(|| m.first_line.clone()))
                .unwrap_or_default();
            let tags: Vec<String> = meta
                .map(|m| {
                    m.tags
                        .iter()
                        .take(2)
                        .map(|t| format!("#{}", t.as_str()))
                        .collect()
                })
                .unwrap_or_default();
            let (status, kind) = meta
                .map(|m| (m.status, m.kind))
                .unwrap_or((jd_core::note::Status::Fleeting, jd_core::note::Kind::Note));
            let detail = match row {
                PaletteRow::Body { snippet, .. } if !snippet.text.is_empty() => {
                    Some(snippet.text.clone())
                }
                _ => None,
            };
            RowView {
                label: format!("Result: '{heading}'"),
                heading,
                detail,
                tags,
                status,
                kind,
            }
        }
        PaletteRow::NewScrap => RowView {
            label: format!("New scrap: '{}'", query.trim()),
            heading: format!("New scrap: '{}'", query.trim()),
            detail: None,
            tags: Vec::new(),
            status: jd_core::note::Status::Fleeting,
            kind: jd_core::note::Kind::Note,
        },
    }
}

/// Paint one result row: selection highlight, miniature face silhouette
/// (shape.rs metrics at ~8% scale — the divider tab / literature footer ARE
/// the kind cues, kept cheap as painted rects), heading, top-2 tags, and an
/// optional snippet line. AccessKit label via WidgetInfo (inbox-picker idiom).
fn render_row(
    ui: &mut egui::Ui,
    view: &RowView,
    selected: bool,
    width: f32,
    th: &crate::theme::Theme,
) -> egui::Response {
    let row_h = if view.detail.is_some() { 46.0 } else { 30.0 };
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(width, row_h), egui::Sense::click());
    {
        let label = view.label.clone();
        resp.widget_info(move || {
            egui::WidgetInfo::labeled(egui::WidgetType::Button, selected, label.as_str())
        });
    }
    if !ui.is_rect_visible(rect) {
        return resp;
    }
    let painter = ui.painter();
    if selected {
        painter.rect_filled(rect, 4.0, th.focus_ring.gamma_multiply(0.25));
    }

    // Miniature face silhouette: card_size at ~8% scale.
    let shape = shape_for(view.status, view.kind);
    let mini = card_size(shape) * 0.08;
    let mini_rect =
        egui::Rect::from_center_size(egui::pos2(rect.min.x + 16.0, rect.min.y + 15.0), mini);
    painter.rect_filled(mini_rect, 1.0, th.card_paper_cream);
    painter.rect_stroke(
        mini_rect,
        1.0,
        egui::Stroke::new(1.0, th.card_border),
        egui::StrokeKind::Outside,
    );
    match shape {
        crate::card::shape::CardShape::Divider => {
            // Kind cue: the protruding tab.
            let tab = egui::Rect::from_min_size(
                egui::pos2(mini_rect.min.x + 2.0, mini_rect.min.y - 3.0),
                egui::vec2(mini.x * 0.4, 3.0),
            );
            painter.rect_filled(tab, 0.5, th.divider_tab_bg);
        }
        crate::card::shape::CardShape::Literature => {
            // Kind cue: the footer band.
            let footer = egui::Rect::from_min_max(
                egui::pos2(mini_rect.min.x, mini_rect.max.y - 3.0),
                mini_rect.max,
            );
            painter.rect_filled(footer, 0.5, th.footer_bg);
        }
        _ => {}
    }

    // Heading + top-2 tags on the first line.
    let text_x = rect.min.x + 34.0;
    let heading_font = egui::FontId::new(14.0, egui::FontFamily::Proportional);
    let galley = painter.layout_no_wrap(view.heading.clone(), heading_font, th.text);
    let heading_w = galley.rect.width();
    painter.galley(
        egui::pos2(text_x, rect.min.y + 15.0 - galley.rect.height() / 2.0),
        galley,
        th.text,
    );
    if !view.tags.is_empty() {
        painter.text(
            egui::pos2(text_x + heading_w + 10.0, rect.min.y + 15.0),
            egui::Align2::LEFT_CENTER,
            view.tags.join(" "),
            egui::FontId::new(12.0, egui::FontFamily::Proportional),
            th.text_weak,
        );
    }

    // Snippet on the second line (Body rows with a cached body).
    if let Some(detail) = &view.detail {
        painter.text(
            egui::pos2(text_x, rect.min.y + 36.0),
            egui::Align2::LEFT_CENTER,
            detail.replace('\n', " "),
            egui::FontId::new(12.0, egui::FontFamily::Proportional),
            th.text_weak,
        );
    }

    resp
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use jd_core::note::{Kind, NoteMeta, Status};
    use jd_core::time::Timestamp;
    use std::sync::mpsc;

    fn nid(n: u8) -> NoteId {
        let s = format!("01ARZ3NDEKTSV4RRFFQ69G5F{n:02}");
        NoteId::parse(&s).unwrap()
    }

    fn meta(n: u8, title: Option<&str>, body: &str, modified: i64) -> NoteMeta {
        NoteMeta {
            id: nid(n),
            rel_path: format!("notes/{n}.md").into(),
            title: title.map(str::to_owned),
            first_line: body.lines().next().unwrap_or("").to_owned(),
            status: if title.is_some() {
                Status::Permanent
            } else {
                Status::Fleeting
            },
            kind: Kind::Note,
            source: None,
            created: Timestamp(modified),
            modified: Timestamp(modified),
            tags: Default::default(),
            links_out: Vec::new(),
            word_count: body.split_whitespace().count() as u32,
        }
    }

    fn index_with(notes: &[(u8, Option<&str>, &str, i64)]) -> Index {
        let mut idx = Index::new();
        for &(n, title, body, modified) in notes {
            idx.upsert(meta(n, title, body, modified), body);
        }
        idx
    }

    #[test]
    fn empty_query_yields_no_rows() {
        let idx = index_with(&[(1, Some("Alpha idea"), "body", 10)]);
        let (tx, _rx) = mpsc::channel();
        let mut bodies = BodyCache::default();
        assert!(compute_results(&idx, "", &mut bodies, &tx).is_empty());
        assert!(compute_results(&idx, "   ", &mut bodies, &tx).is_empty());
    }

    #[test]
    fn strata_order_title_then_body_then_new_scrap() {
        let idx = index_with(&[
            (1, Some("Alpha idea"), "first body", 10),
            (2, Some("Beta note"), "alpha appears in this body", 20),
        ]);
        let (tx, _rx) = mpsc::channel();
        let mut bodies = BodyCache::default();
        let rows = compute_results(&idx, "alpha", &mut bodies, &tx);
        assert_eq!(rows.len(), 3, "title + body + new-scrap, got {rows:?}");
        assert!(matches!(&rows[0], PaletteRow::Title { id, .. } if *id == nid(1)));
        assert!(matches!(&rows[1], PaletteRow::Body { id, .. } if *id == nid(2)));
        assert!(matches!(&rows[2], PaletteRow::NewScrap));
    }

    #[test]
    fn stratum_two_dedupes_stratum_one_ids() {
        // "Alpha idea" matches by title AND body — it must appear once (Title).
        let idx = index_with(&[(1, Some("Alpha idea"), "alpha alpha alpha", 10)]);
        let (tx, _rx) = mpsc::channel();
        let mut bodies = BodyCache::default();
        let rows = compute_results(&idx, "alpha", &mut bodies, &tx);
        let hits: Vec<_> = rows
            .iter()
            .filter(|r| !matches!(r, PaletteRow::NewScrap))
            .collect();
        assert_eq!(hits.len(), 1, "deduped: {rows:?}");
        assert!(matches!(hits[0], PaletteRow::Title { id, .. } if *id == nid(1)));
    }

    #[test]
    fn untitled_scraps_excluded_from_stratum_one_but_match_stratum_two() {
        let idx = index_with(&[(1, None, "alpha scribble", 10)]);
        let (tx, _rx) = mpsc::channel();
        let mut bodies = BodyCache::default();
        let rows = compute_results(&idx, "alpha", &mut bodies, &tx);
        assert!(
            !rows.iter().any(|r| matches!(r, PaletteRow::Title { .. })),
            "untitled scrap must not be a Title row: {rows:?}"
        );
        assert!(
            rows.iter()
                .any(|r| matches!(r, PaletteRow::Body { id, .. } if *id == nid(1))),
            "untitled scrap must match via body: {rows:?}"
        );
    }

    #[test]
    fn recency_breaks_title_score_ties() {
        // Same tier + score (equal-length prefix matches); newer modified wins.
        let idx = index_with(&[
            (1, Some("Alpha aa"), "x", 10),
            (2, Some("Alpha bb"), "y", 99),
        ]);
        let (tx, _rx) = mpsc::channel();
        let mut bodies = BodyCache::default();
        let rows = compute_results(&idx, "alpha", &mut bodies, &tx);
        assert!(
            matches!(&rows[0], PaletteRow::Title { id, .. } if *id == nid(2)),
            "newer note must rank first on tie: {rows:?}"
        );
        assert!(matches!(&rows[1], PaletteRow::Title { id, .. } if *id == nid(1)));
    }

    #[test]
    fn body_snippet_blank_while_loading_and_requests_once() {
        let idx = index_with(&[(1, Some("Beta note"), "alpha appears here", 10)]);
        let (tx, rx) = mpsc::channel();
        let mut bodies = BodyCache::default();
        let rows = compute_results(&idx, "alpha", &mut bodies, &tx);
        let PaletteRow::Body { snippet, .. } = &rows[0] else {
            panic!("expected Body row: {rows:?}");
        };
        assert!(snippet.text.is_empty(), "blank while loading");
        // Exactly ONE ReadBody was fired.
        let sent: Vec<_> = rx.try_iter().collect();
        assert_eq!(sent.len(), 1);
        assert!(matches!(sent[0], VaultCommand::ReadBody { id } if id == nid(1)));

        // Body arrives → snippet fills in on the next recompute; no re-request.
        bodies.insert(nid(1), "alpha appears here".into());
        let rows = compute_results(&idx, "alpha", &mut bodies, &tx);
        let PaletteRow::Body { snippet, .. } = &rows[0] else {
            panic!("expected Body row: {rows:?}");
        };
        assert!(snippet.text.contains("alpha"), "snippet: {snippet:?}");
        assert!(!snippet.highlights.is_empty(), "highlights: {snippet:?}");
        assert!(rx.try_iter().next().is_none(), "no duplicate ReadBody");
    }

    #[test]
    fn new_scrap_is_always_last_even_with_no_hits() {
        let idx = index_with(&[(1, Some("Unrelated"), "nothing here", 10)]);
        let (tx, _rx) = mpsc::channel();
        let mut bodies = BodyCache::default();
        let rows = compute_results(&idx, "zzz-no-match", &mut bodies, &tx);
        assert_eq!(rows.len(), 1);
        assert!(matches!(&rows[0], PaletteRow::NewScrap));
    }

    #[test]
    fn strata_caps_at_eight_per_stratum() {
        let notes: Vec<(u8, Option<String>, String, i64)> = (1..=12)
            .map(|n| {
                (
                    n,
                    Some(format!("Alpha {n:02}")),
                    format!("body {n}"),
                    n as i64,
                )
            })
            .collect();
        let mut idx = Index::new();
        for (n, title, body, modified) in &notes {
            idx.upsert(meta(*n, title.as_deref(), body, *modified), body);
        }
        let (tx, _rx) = mpsc::channel();
        let mut bodies = BodyCache::default();
        let rows = compute_results(&idx, "alpha", &mut bodies, &tx);
        let title_count = rows
            .iter()
            .filter(|r| matches!(r, PaletteRow::Title { .. }))
            .count();
        assert_eq!(title_count, 8, "stratum 1 capped at 8: {rows:?}");
    }
}
