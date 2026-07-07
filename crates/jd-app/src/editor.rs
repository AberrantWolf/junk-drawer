//! Editor internals: the mixed-size layouter (Spike A) + the floating editor
//! window (Task 10).

use std::collections::HashMap;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::sync::Arc;
use std::time::Instant;

use eframe::egui::{self, text::LayoutJob};
use jd_core::id::NoteId;
use jd_core::lexer::{LineState, SpanStyle, StyledSpan, lex_line};
use jd_core::worker::VaultCommand;

/// Per-line lexing is O(n^2) on pathological unclosed-delimiter lines
/// (WP1b review). Lines are lexed only up to this many bytes; the rest is
/// styled as plain text. Invisible for human-authored notes.
pub const MAX_LEXED_LINE_BYTES: usize = 8 * 1024;

pub const BODY_SIZE: f32 = 15.0;
pub const MONO_SIZE: f32 = 14.0;

pub fn heading_size(level: u8) -> f32 {
    match level {
        1 => 24.0,
        2 => 20.0,
        _ => 17.0,
    }
}

/// LineState has only two variants; encode as a bool for use as a hash map key.
#[derive(Clone, PartialEq, Eq, Hash)]
struct LineKey {
    hash: u64,
    in_fence: bool,
}

/// Cache: (line content hash, entry fence state) → (spans, exit state).
/// Only edited lines re-lex; a fence toggle upstream changes entry states
/// downstream, which changes keys, which re-lexes exactly the affected lines.
#[derive(Default)]
pub struct LineCache {
    map: HashMap<LineKey, (Vec<StyledSpan>, LineState)>,
}

fn line_key(line: &str, entry: LineState) -> LineKey {
    let mut h = DefaultHasher::new();
    line.hash(&mut h);
    LineKey {
        hash: h.finish(),
        in_fence: entry == LineState::InCodeFence,
    }
}

fn lex_capped(
    line: &str,
    entry: LineState,
    resolve: &dyn Fn(&str) -> bool,
) -> (Vec<StyledSpan>, LineState) {
    if line.len() <= MAX_LEXED_LINE_BYTES {
        return lex_line(line, entry, resolve);
    }
    // Cap: lex the head (back off to a char boundary), tail is plain Text.
    let mut cut = MAX_LEXED_LINE_BYTES;
    while !line.is_char_boundary(cut) {
        cut -= 1;
    }
    let (mut spans, exit) = lex_line(&line[..cut], entry, resolve);
    spans.push(StyledSpan {
        range: cut..line.len(),
        style: SpanStyle::Text,
    });
    (spans, exit)
}

/// Build the mixed-size galley for `text`. HEADING SIZES ARE REAL SIZES —
/// this is the spike's whole bet. One LayoutJob for the entire buffer;
/// egui's TextEdit maps cursor hits through the galley, so as long as the
/// job's byte ranges exactly tile the buffer, cursor/selection/IME inherit
/// correctness from TextEdit itself.
///
/// `promote_restyle`: when `true`, the first line is rendered as a Heading(1)
/// even without a `# ` prefix — promotion visual feedback (WP3 Task 4).
/// The BUFFER stays raw; the `# ` is prepended at commit time only.
pub fn layout_body(
    ui: &egui::Ui,
    text: &str,
    wrap_width: f32,
    cache: &mut LineCache,
    resolve: &dyn Fn(&str) -> bool,
    theme: &crate::theme::Theme,
    promote_restyle: bool,
) -> Arc<egui::Galley> {
    let mut job = LayoutJob::default();
    job.wrap.max_width = wrap_width;
    let mut state = LineState::Normal;
    let mut offset = 0usize;
    for (i, line) in text.split('\n').enumerate() {
        if i > 0 {
            // The '\n' itself: append with the BODY format so every byte of
            // the buffer is present in the job (cursor mapping requirement).
            job.append("\n", 0.0, crate::theme::text_format(SpanStyle::Text, theme));
            offset += 1;
        }

        // Promotion restyle: first line rendered as heading even without `# `.
        // This is presentation-only; the buffer stays raw.
        if promote_restyle && i == 0 && !line.starts_with('#') {
            let mut fmt = crate::theme::text_format(SpanStyle::Text, theme);
            fmt.font_id = eframe::egui::FontId::new(
                heading_size(1),
                eframe::egui::FontFamily::Name("inter-bold".into()),
            );
            job.append(line, 0.0, fmt);
            offset += line.len();
            state = LineState::Normal; // first line can't be in a fence
            continue;
        }

        let key = line_key(line, state);
        let (spans, exit) = cache
            .map
            .entry(key)
            .or_insert_with(|| lex_capped(line, state, resolve))
            .clone();
        if spans.is_empty() {
            // empty line: append a zero-width body-sized section so the line
            // has a defined height and the cursor can sit on it.
            job.append("", 0.0, crate::theme::text_format(SpanStyle::Text, theme));
        }
        for s in &spans {
            let mut fmt = crate::theme::text_format(s.style, theme);
            // Fix HeadingMarker level: derive level from the '#' count in the line,
            // rather than always using heading_size(1).
            if s.style == SpanStyle::HeadingMarker {
                let level = line.bytes().take_while(|&b| b == b'#').count().clamp(1, 3) as u8;
                fmt.font_id = eframe::egui::FontId::new(
                    heading_size(level),
                    eframe::egui::FontFamily::Name("inter-bold".into()),
                );
            }
            job.append(&line[s.range.clone()], 0.0, fmt);
        }
        state = exit;
        offset += line.len();
    }
    let _ = offset;
    // egui 0.35: fonts_mut gives &mut FontsView which has layout_job().
    // ui.fonts() gives &FontsView (immutable), which does NOT have layout_job.
    ui.ctx().fonts_mut(|f| f.layout_job(job))
}

// ---------------------------------------------------------------------------
// EditorState (Task 10)
// ---------------------------------------------------------------------------

/// Live state of the floating editor modal.
pub struct EditorState {
    pub id: NoteId,
    pub buffer: String,
    pub dirty: bool,
    /// Timestamp of the last buffer edit. Used for the 1-second autosave and
    /// the 2-second recovery-journal anchors.
    pub last_edit: Option<Instant>,
    /// Timestamp of the last recovery journal write.
    pub last_journaled: Option<Instant>,
    pub cache: LineCache,
    /// Placeholder: Task 12 replaces with a real word-granularity undo stack.
    pub undo: crate::text_undo::TextUndo,
    /// Whether TextEdit should request focus on the next frame.
    needs_focus: bool,
    /// Currently selected autocomplete index.
    pub ac_selected: usize,
    /// Esc dismissed the popup for the current autocomplete context.
    ac_dismissed: bool,
    /// Cursor state from the previous frame (for pre-TextEdit key interception
    /// and URL-paste-over-selection; the buffer cannot change between frames,
    /// so last frame's cursor is current when this frame's input arrives).
    prev_cursor: Option<egui::text::CCursorRange>,
    /// Whether the card being edited was fleeting when it was opened.
    /// Threaded from FaceMeta/index at open time.
    pub is_fleeting: bool,
    /// Promotion is pending: on close, dispatch a compound
    /// Batch([SaveBody{content: "# line1\nrest"}, Promote{id}]).
    /// Set by Enter-at-end-of-single-line (fleeting only) or Ctrl+Enter.
    pub pending_promotion: bool,
}

impl EditorState {
    /// Open a new editor for `id` pre-loaded with `body` (body-only, no frontmatter).
    /// `saved_undo` is the undo stack from the previous session (if any); when
    /// `None` a fresh stack is created from `body`.
    /// `is_fleeting` is threaded from FaceMeta at open time.
    /// `pending_promotion` is true when the Inbox Ctrl+Enter path immediately seeds
    /// promotion (no newline needed; close will dispatch the compound op).
    pub fn open(
        id: NoteId,
        body: String,
        saved_undo: Option<crate::text_undo::TextUndo>,
        is_fleeting: bool,
        pending_promotion: bool,
    ) -> EditorState {
        let undo = saved_undo.unwrap_or_else(|| crate::text_undo::TextUndo::new(&body));
        EditorState {
            id,
            buffer: body,
            dirty: false,
            last_edit: None,
            last_journaled: None,
            cache: LineCache::default(),
            undo,
            needs_focus: true,
            ac_selected: 0,
            ac_dismissed: false,
            prev_cursor: None,
            is_fleeting,
            pending_promotion,
        }
    }
}

// ---------------------------------------------------------------------------
// Pure editor helpers (Task 11)
// ---------------------------------------------------------------------------

#[derive(Debug, PartialEq, Clone)]
pub enum EnterAction {
    Plain,
    Continue(String),
    EndList { strip_from: usize },
}

pub fn enter_action(line: &str) -> EnterAction {
    // Count leading whitespace (for nested lists)
    let trimmed = line.trim_start();
    let indent_len = line.len() - trimmed.len();
    let indent = &line[..indent_len];

    // Blockquote
    if trimmed.starts_with("> ") {
        let content = trimmed.strip_prefix("> ").unwrap_or_default();
        let prefix = format!("{}> ", indent);
        if content.trim().is_empty() {
            return EnterAction::EndList {
                strip_from: indent_len,
            };
        }
        return EnterAction::Continue(prefix);
    }

    // Unordered list: "- [ ] ", "- [x] ", "- "
    if trimmed.starts_with("- [ ] ") || trimmed.starts_with("- [x] ") {
        let content = &trimmed[6..];
        let prefix = format!("{}- [ ] ", indent);
        if content.trim().is_empty() {
            return EnterAction::EndList {
                strip_from: indent_len,
            };
        }
        return EnterAction::Continue(prefix);
    }
    if trimmed.starts_with("- ") {
        let content = trimmed.strip_prefix("- ").unwrap_or_default();
        let prefix = format!("{}- ", indent);
        if content.trim().is_empty() {
            return EnterAction::EndList {
                strip_from: indent_len,
            };
        }
        return EnterAction::Continue(prefix);
    }

    // Ordered list: "N. "
    if let Some(dot_pos) = trimmed.find(". ") {
        let num_str = &trimmed[..dot_pos];
        if !num_str.is_empty()
            && num_str.chars().all(|c| c.is_ascii_digit())
            && let Ok(n) = num_str.parse::<u64>()
        {
            let content = &trimmed[dot_pos + 2..];
            let prefix = format!("{}{}. ", indent, n + 1);
            if content.trim().is_empty() {
                return EnterAction::EndList {
                    strip_from: indent_len,
                };
            }
            return EnterAction::Continue(prefix);
        }
    }

    EnterAction::Plain
}

/// Returns Some(new_line) if the line was indented/outdented, None if unchanged.
pub fn indent_line(line: &str, outdent: bool) -> Option<String> {
    // Only apply to list lines
    let trimmed = line.trim_start();
    let is_list = trimmed.starts_with("- ")
        || trimmed.starts_with("> ")
        || trimmed.starts_with("- [ ] ")
        || trimmed.starts_with("- [x] ")
        || trimmed.find(". ").is_some_and(|p| {
            !trimmed[..p].is_empty() && trimmed[..p].chars().all(|c| c.is_ascii_digit())
        });
    if !is_list {
        return None;
    }

    if outdent {
        line.strip_prefix("  ").map(str::to_owned)
    } else {
        Some(format!("  {}", line))
    }
}

#[derive(Debug, PartialEq, Clone)]
pub enum AcContext {
    None,
    Link { start: usize, query: String },
    Tag { start: usize, query: String },
}

pub fn ac_context(buffer: &str, cursor_byte: usize) -> AcContext {
    let before = &buffer[..cursor_byte.min(buffer.len())];

    // Find the current line start
    let line_start = before.rfind('\n').map_or(0, |p| p + 1);
    let line_before = &before[line_start..];

    // Check for [[ link
    if let Some(bracket_pos) = line_before.rfind("[[") {
        // Make sure there's no ]] after it
        let after_bracket = &line_before[bracket_pos + 2..];
        if !after_bracket.contains("]]") {
            let query = after_bracket.to_owned();
            return AcContext::Link {
                start: line_start + bracket_pos,
                query,
            };
        }
    }

    // Check for # tag (must be preceded by whitespace or be at start)
    // But NOT if the # starts the line (that's a heading)
    if let Some(hash_pos) = line_before.rfind('#') {
        // A heading: # is the very first char on the line
        if line_before.starts_with('#') {
            return AcContext::None;
        }
        let before_hash = &line_before[..hash_pos];
        // Must be preceded by whitespace or start of line
        let valid_tag_start =
            before_hash.is_empty() || before_hash.ends_with(' ') || before_hash.ends_with('\t');
        if valid_tag_start {
            let query = &line_before[hash_pos + 1..];
            return AcContext::Tag {
                start: line_start + hash_pos,
                query: query.to_owned(),
            };
        }
    }

    AcContext::None
}

pub fn is_probably_url(s: &str) -> bool {
    s.starts_with("https://") || s.starts_with("http://")
}

// ---------------------------------------------------------------------------
// Autocomplete plumbing (Task 11)
// ---------------------------------------------------------------------------

/// One row of the autocomplete popup.
#[derive(Clone, Debug)]
enum AcItem {
    /// An existing note title / tag; accepting inserts it.
    Existing(String),
    /// "Link as new card: '<query>'" — accepting inserts the query verbatim
    /// (an unresolved link the user can flesh out later).
    NewCard(String),
}

/// Up to 8 fuzzy candidates for the current context, plus the new-card row
/// for links when the query is not an exact (case-insensitive) title.
fn ac_candidates(index: &jd_core::index::Index, ctx: &AcContext) -> Vec<AcItem> {
    use jd_core::index::fuzzy::{FuzzyTier, fuzzy_match};
    fn ranked(mut hits: Vec<(FuzzyTier, i32, String)>) -> impl Iterator<Item = String> {
        hits.sort_by(|a, b| {
            a.0.cmp(&b.0)
                .then_with(|| b.1.cmp(&a.1))
                .then_with(|| a.2.cmp(&b.2))
        });
        hits.into_iter().take(8).map(|(_, _, t)| t)
    }
    match ctx {
        AcContext::Link { query, .. } if !query.is_empty() => {
            let hits = index
                .iter_meta()
                .filter_map(|m| m.title.as_deref())
                .filter_map(|t| fuzzy_match(query, t).map(|s| (s.tier, s.score, t.to_owned())))
                .collect();
            let mut items: Vec<AcItem> = ranked(hits).map(AcItem::Existing).collect();
            if index.resolve_title(query).is_none() {
                items.push(AcItem::NewCard(query.clone()));
            }
            items
        }
        AcContext::Tag { query, .. } if !query.is_empty() => {
            let hits = index
                .all_tags()
                .into_iter()
                .filter_map(|(t, _)| {
                    fuzzy_match(query, t.as_str()).map(|s| (s.tier, s.score, t.as_str().to_owned()))
                })
                .collect();
            ranked(hits).map(AcItem::Existing).collect()
        }
        _ => Vec::new(),
    }
}

/// Place the TextEdit caret at `char_pos` (collapsed selection) for the next frame.
fn set_cursor_char(ctx: &egui::Context, te_id: egui::Id, char_pos: usize) {
    let mut state = egui::text_edit::TextEditState::load(ctx, te_id).unwrap_or_default();
    state
        .cursor
        .set_char_range(Some(egui::text::CCursorRange::one(
            egui::text::CCursor::new(char_pos),
        )));
    state.store(ctx, te_id);
}

/// Accept an autocomplete item: replace the open `[[query` / `#query` with the
/// completed text and park the caret after it. If the buffer already has `]]`
/// immediately ahead of the cursor, don't double it.
fn ac_accept(
    ed: &mut EditorState,
    ctx: &egui::Context,
    te_id: egui::Id,
    ac_ctx: &AcContext,
    item: &AcItem,
    cursor_byte: usize,
) {
    let text = match item {
        AcItem::Existing(t) | AcItem::NewCard(t) => t.as_str(),
    };
    match *ac_ctx {
        AcContext::Link { start, .. } => {
            let closing_ahead = ed.buffer[cursor_byte..].starts_with("]]");
            let replacement = if closing_ahead {
                format!("[[{text}")
            } else {
                format!("[[{text}]]")
            };
            let start_char = ed.buffer[..start].chars().count();
            ed.buffer.replace_range(start..cursor_byte, &replacement);
            let after =
                start_char + replacement.chars().count() + if closing_ahead { 2 } else { 0 };
            set_cursor_char(ctx, te_id, after);
        }
        AcContext::Tag { start, .. } => {
            let replacement = format!("#{text} ");
            let start_char = ed.buffer[..start].chars().count();
            ed.buffer.replace_range(start..cursor_byte, &replacement);
            set_cursor_char(ctx, te_id, start_char + replacement.chars().count());
        }
        AcContext::None => return,
    }
    ed.dirty = true;
    ed.last_edit = Some(Instant::now());
    ed.ac_selected = 0;
}

// ---------------------------------------------------------------------------
// EditorDeps and editor_ui (Task 10)
// ---------------------------------------------------------------------------

/// Dependencies injected into `editor_ui` each frame.
pub struct EditorDeps<'a> {
    pub theme: &'a crate::theme::Theme,
    pub commands: &'a std::sync::mpsc::Sender<VaultCommand>,
    pub index: &'a std::sync::Arc<std::sync::RwLock<jd_core::index::Index>>,
    /// Passed through to future animation logic (unused in WP2 rendering).
    pub reduced_motion: bool,
}

/// What the editor wants the caller to do after this frame.
pub enum EditorEvent {
    KeepOpen,
    CloseAndSave,
}

/// Convert a char index to a byte offset in `s`.
fn char_idx_to_byte(s: &str, char_idx: usize) -> usize {
    s.char_indices().nth(char_idx).map_or(s.len(), |(i, _)| i)
}

/// Render the floating modal editor over the desk.
///
/// Returns `CloseAndSave` when the user presses Esc or Ctrl+Enter.
/// Autosave (dirty + last_edit > 1 s) and recovery-journal (changed +
/// last_journaled > 2 s) are handled here as side-effects.
pub fn editor_ui(
    ui: &mut egui::Ui,
    ed: &mut EditorState,
    deps: &mut EditorDeps<'_>,
) -> EditorEvent {
    // --- build the resolve closure from the index ---
    let index_guard = deps.index.read().unwrap();
    // `index_guard` lives for the duration of this function; `resolve_fn` borrows
    // it immutably.  Both are stack-local, so the lifetime is sound.
    let resolve_fn = |title: &str| index_guard.resolve_title(title).is_some();

    let mut close_requested = false;

    // Check Ctrl+Enter before showing the Modal so we consume the key even
    // when egui's own TextEdit would otherwise handle Enter.
    let ctrl_enter = ui.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, egui::Key::Enter));
    if ctrl_enter {
        // Task 4: Ctrl+Enter on a fleeting single-line card = promote-without-typing.
        // Set pending_promotion immediately; close falls through below.
        if ed.is_fleeting && !ed.buffer.contains('\n') && !ed.pending_promotion {
            ed.pending_promotion = true;
        }
        close_requested = true;
    }

    // --- modal window ---
    let modal = egui::Modal::new(egui::Id::new("editor_modal"))
        .frame(
            egui::Frame::default()
                .fill(deps.theme.card_paper_cream)
                .shadow(egui::Shadow {
                    offset: [0, 4],
                    blur: 16,
                    spread: 0,
                    color: egui::Color32::from_black_alpha(80),
                })
                .corner_radius(8.0)
                .inner_margin(egui::Margin::same(16)),
        )
        .backdrop_color(egui::Color32::from_black_alpha(80));

    let modal_resp = modal.show(ui.ctx(), |ui| {
        ui.set_min_size(egui::vec2(540.0, 440.0));
        ui.set_max_size(egui::vec2(540.0, 440.0));

        // --- Pre-TextEdit interception (Task 11 + 12) ---
        // Everything that must win over TextEdit's own key handling happens
        // HERE, before `te.show` — consumed events never reach the widget.
        // Positions come from last frame's cursor; the buffer cannot have
        // changed since, so it is current when this frame's input arrives.
        let te_id = egui::Id::new("editor_te");

        // Task 12: Ctrl+Z (undo) and Ctrl+Shift+Z / Ctrl+Y (redo) are consumed
        // here so egui's built-in TextEdit undoer never sees them.
        // `now_ms` from ui.input(|i| i.time) keeps TextUndo free of Instant.
        let now_ms = ui.input(|i| (i.time * 1000.0) as u64);
        let do_undo = ui.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, egui::Key::Z));
        let do_redo = ui.input_mut(|i| {
            i.consume_key(
                egui::Modifiers::COMMAND | egui::Modifiers::SHIFT,
                egui::Key::Z,
            ) || i.consume_key(egui::Modifiers::COMMAND, egui::Key::Y)
        });
        if do_undo && let Some(snap) = ed.undo.undo(&ed.buffer) {
            ed.buffer = snap.text;
            set_cursor_char(ui.ctx(), te_id, snap.cursor);
            ed.dirty = true;
            ed.last_edit = Some(Instant::now());
            // Clear pending_promotion ONLY when the undo snapshot no longer contains
            // the triggering newline. Partial undo (e.g. body text typed after the
            // promoting Enter) reverts the body group but the buffer still has "title\n",
            // so promotion must survive — close still needs to dispatch the compound op.
            if ed.pending_promotion && !ed.buffer.contains('\n') {
                ed.pending_promotion = false;
            }
        }
        if do_redo && let Some(snap) = ed.undo.redo() {
            ed.buffer = snap.text;
            set_cursor_char(ui.ctx(), te_id, snap.cursor);
            ed.dirty = true;
            ed.last_edit = Some(Instant::now());
        }
        let has_focus = ui.ctx().memory(|m| m.has_focus(te_id));
        let cursor_char = ed.prev_cursor.map(|cr| cr.primary.index.0);
        let cursor_byte = cursor_char.map(|c| char_idx_to_byte(&ed.buffer, c));

        // Task 12: snapshot buffer length at the start of this frame so we can
        // detect any buffer change (from pre-show code OR from TextEdit itself)
        // and call record() once at the end.
        let buffer_at_frame_start = ed.buffer.clone();

        // URL paste over a selection → [selection](url). No other paste transform.
        if let Some(prev_cr) = ed.prev_cursor {
            let lo = prev_cr.primary.index.0.min(prev_cr.secondary.index.0);
            let hi = prev_cr.primary.index.0.max(prev_cr.secondary.index.0);
            if lo != hi {
                let mut paste_url: Option<String> = Option::None;
                ui.input_mut(|i| {
                    i.events.retain(|ev| {
                        if let egui::Event::Paste(text) = ev
                            && is_probably_url(text)
                        {
                            paste_url = Some(text.clone());
                            false // consume: TextEdit must not also insert it
                        } else {
                            true
                        }
                    });
                });
                if let Some(url) = paste_url {
                    let byte_lo = char_idx_to_byte(&ed.buffer, lo);
                    let byte_hi = char_idx_to_byte(&ed.buffer, hi);
                    let selected_text = ed.buffer[byte_lo..byte_hi].to_owned();
                    let md_link = format!("[{selected_text}]({url})");
                    ed.buffer.replace_range(byte_lo..byte_hi, &md_link);
                    set_cursor_char(ui.ctx(), te_id, lo + md_link.chars().count());
                    ed.dirty = true;
                    ed.last_edit = Some(Instant::now());
                }
            }
        }

        // Autocomplete context + candidates (from the pre-edit cursor).
        let ac_ctx = cursor_byte.map_or(AcContext::None, |cb| ac_context(&ed.buffer, cb));
        if matches!(ac_ctx, AcContext::None) {
            ed.ac_dismissed = false;
            ed.ac_selected = 0;
        }
        let ac_items = if ed.ac_dismissed {
            Vec::new()
        } else {
            ac_candidates(&index_guard, &ac_ctx)
        };
        let popup_active = has_focus && !ac_items.is_empty();
        if popup_active {
            ed.ac_selected = ed.ac_selected.min(ac_items.len() - 1);
        }

        if popup_active {
            // The popup owns Up/Down/Enter/Tab/Esc while it is showing.
            let n = ac_items.len();
            if ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown)) {
                ed.ac_selected = (ed.ac_selected + 1) % n;
            }
            if ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp)) {
                ed.ac_selected = (ed.ac_selected + n - 1) % n;
            }
            if ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape)) {
                // Esc dismisses the popup ONLY; consuming it here keeps it from
                // reaching Modal::should_close, so the editor stays open.
                ed.ac_dismissed = true;
            }
            let accept = ui.input_mut(|i| {
                i.consume_key(egui::Modifiers::NONE, egui::Key::Enter)
                    || i.consume_key(egui::Modifiers::NONE, egui::Key::Tab)
            });
            if accept && let Some(cb) = cursor_byte {
                let item = ac_items[ed.ac_selected].clone();
                ac_accept(ed, ui.ctx(), te_id, &ac_ctx, &item, cb);
            }
        } else if has_focus && let (Some(c), Some(cb)) = (cursor_char, cursor_byte) {
            // Promotion trigger (Task 4): fleeting card, exactly one line in
            // the buffer, cursor at the end of that line → Enter promotes.
            // Compose with list continuation: promotion check runs first; if
            // the single line happens to be a list prefix ("- "), list
            // continuation takes priority (the scrap isn't meaningful yet).
            // In practice a lone "- " triggers EndList which clears it, so
            // entering an empty list line cannot accidentally promote.
            let is_single_line = !ed.buffer.contains('\n');
            let cursor_at_end = cb == ed.buffer.len();
            // Guard: buffer must be non-empty (trimmed) — an empty fleeting card
            // pressing Enter would produce a "# \n" empty-title note otherwise.
            let has_title = !ed.buffer.trim().is_empty();
            if ed.is_fleeting
                && !ed.pending_promotion
                && is_single_line
                && cursor_at_end
                && has_title
                && enter_action(&ed.buffer) == EnterAction::Plain
            {
                if ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Enter)) {
                    // Insert the newline so the user can type body text.
                    ed.buffer.push('\n');
                    // Move cursor to after the newline (start of line 2).
                    set_cursor_char(ui.ctx(), te_id, c + 1);
                    ed.dirty = true;
                    ed.last_edit = Some(Instant::now());
                    // Record now so Ctrl+Z can revert just the newline.
                    ed.undo.record(&ed.buffer, c + 1, now_ms);
                    ed.pending_promotion = true;
                    // Skip the regular list/continuation block below.
                }
            } else {
                // Enter: list/quote continuation. Plain Enter passes through
                // (only Continue/EndList consume the key).
                let line_start = ed.buffer[..cb].rfind('\n').map_or(0, |p| p + 1);
                let line_before = ed.buffer[line_start..cb].to_owned();
                let mut edited = false;
                match enter_action(&line_before) {
                    EnterAction::Plain => {}
                    EnterAction::Continue(prefix) => {
                        if ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Enter))
                        {
                            let ins = format!("\n{prefix}");
                            ed.buffer.insert_str(cb, &ins);
                            set_cursor_char(ui.ctx(), te_id, c + ins.chars().count());
                            ed.dirty = true;
                            ed.last_edit = Some(Instant::now());
                            edited = true;
                        }
                    }
                    EnterAction::EndList { strip_from } => {
                        if ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Enter))
                        {
                            let strip_start = line_start + strip_from;
                            let stripped = ed.buffer[strip_start..cb].chars().count();
                            ed.buffer.replace_range(strip_start..cb, "");
                            set_cursor_char(ui.ctx(), te_id, c - stripped);
                            ed.dirty = true;
                            ed.last_edit = Some(Instant::now());
                            edited = true;
                        }
                    }
                }

                // Tab / Shift+Tab: indent/outdent list lines only; non-list Tab
                // passes through (TextEdit inserts \t — acceptable v1). Skipped
                // when Enter already edited the buffer (cursor bytes are stale).
                if !edited {
                    let line_end = ed.buffer[cb..]
                        .find('\n')
                        .map_or(ed.buffer.len(), |p| cb + p);
                    let line = ed.buffer[line_start..line_end].to_owned();
                    if indent_line(&line, false).is_some() {
                        let outdent =
                            ui.input_mut(|i| i.consume_key(egui::Modifiers::SHIFT, egui::Key::Tab));
                        let indent = !outdent
                            && ui.input_mut(|i| {
                                i.consume_key(egui::Modifiers::NONE, egui::Key::Tab)
                            });
                        if (indent || outdent)
                            && let Some(new_line) = indent_line(&line, outdent)
                        {
                            ed.buffer.replace_range(line_start..line_end, &new_line);
                            let new_c = if outdent { c.saturating_sub(2) } else { c + 2 };
                            set_cursor_char(ui.ctx(), te_id, new_c);
                            ed.dirty = true;
                            ed.last_edit = Some(Instant::now());
                        }
                    }
                }
            } // closes else { (non-promotion path)
        }

        // Build the layouter closure; borrows `ed.cache` and `resolve_fn`.
        let cache = &mut ed.cache;
        let theme = deps.theme;
        let promote_restyle = ed.pending_promotion;
        let mut layouter = |ui: &egui::Ui, buf: &dyn egui::TextBuffer, wrap: f32| {
            layout_body(
                ui,
                buf.as_str(),
                wrap,
                cache,
                &resolve_fn,
                theme,
                promote_restyle,
            )
        };

        let te = egui::TextEdit::multiline(&mut ed.buffer)
            .id(te_id)
            .desired_width(540.0 - 32.0) // full width minus padding
            .desired_rows(24)
            .layouter(&mut layouter);

        let te_out = te.show(ui);
        let resp = te_out.response;

        // Store cursor for next frame (URL paste selection detection).
        ed.prev_cursor = te_out.cursor_range;

        // Grant focus on first open.
        if ed.needs_focus {
            resp.request_focus();
            ed.needs_focus = false;
        }

        // Dirty detection via response.changed().
        if resp.changed() {
            ed.dirty = true;
            ed.last_edit = Some(Instant::now());
            // Recovery journal: if last_journaled elapsed > 2s (or never).
            let should_journal = ed
                .last_journaled
                .is_none_or(|t| t.elapsed().as_secs_f32() > 2.0);
            if should_journal {
                let _ = deps.commands.send(VaultCommand::JournalBuffer {
                    id: ed.id,
                    content: ed.buffer.clone(),
                });
                ed.last_journaled = Some(Instant::now());
            }
        }

        // Task 12: record any buffer change (from TextEdit OR from pre-show code)
        // into the undo stack. The cursor after the edit is whichever the TextEdit
        // reports for this frame (te_out.cursor_range), falling back to prev_cursor.
        // This single record() call covers regular typing, autocomplete accept,
        // Enter continuation, URL paste, and indent/outdent uniformly.
        if ed.buffer != buffer_at_frame_start {
            let post_cursor = te_out
                .cursor_range
                .map(|cr| cr.primary.index.0)
                .or(cursor_char)
                .unwrap_or(0);
            ed.undo.record(&ed.buffer, post_cursor, now_ms);
        }

        // --- Autocomplete popup rendering (anchored at the caret) ---
        if popup_active {
            let anchor_pos = te_out
                .cursor_range
                .map(|cr| {
                    let r = te_out.galley.pos_from_cursor(cr.primary);
                    te_out.galley_pos + r.left_bottom().to_vec2()
                })
                .unwrap_or_else(|| resp.rect.left_bottom());
            let mut clicked: Option<usize> = Option::None;
            egui::Popup::from_response(&resp)
                .id(egui::Id::new("ac_popup"))
                .open(true)
                .at_position(anchor_pos)
                .show(|ui| {
                    for (i, item) in ac_items.iter().enumerate() {
                        let label = match item {
                            AcItem::Existing(t) => t.clone(),
                            AcItem::NewCard(q) => format!("Link as new card: '{q}'"),
                        };
                        if ui.selectable_label(i == ed.ac_selected, label).clicked() {
                            clicked = Some(i);
                        }
                    }
                });
            if let (Some(i), Some(cb)) = (clicked, cursor_byte) {
                ac_accept(ed, ui.ctx(), te_id, &ac_ctx, &ac_items[i].clone(), cb);
            }
        }
    });

    // Esc to close (Modal::should_close handles Esc + backdrop click).
    if modal_resp.should_close() {
        close_requested = true;
    }

    // Autosave: dirty && last_edit > 1s → save, clear dirty, stay open.
    // IMPORTANT: skip autosave while promotion is pending — the ONE-compound-entry
    // constraint (WP3 Task 4) requires that the SaveBody for a promotion is bundled
    // with the Promote op in a single Batch dispatch on close. A mid-pending autosave
    // would journal a second SaveBody entry, violating the single-entry invariant.
    // Recovery journaling (JournalBuffer) is unaffected and may continue normally.
    if ed.dirty
        && !ed.pending_promotion
        && let Some(last_edit) = ed.last_edit
        && last_edit.elapsed().as_secs_f32() > 1.0
    {
        let _ = deps.commands.send(VaultCommand::Op {
            op: jd_core::command::VaultOp::SaveBody {
                id: ed.id,
                content: ed.buffer.clone(),
            },
            source: jd_core::command::OpSource::User,
        });
        ed.dirty = false;
        // Keep last_edit so the next edit cycle anchors correctly.
    }

    if close_requested {
        EditorEvent::CloseAndSave
    } else {
        EditorEvent::KeepOpen
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_enter_action() {
        let cases: &[(&str, EnterAction)] = &[
            ("- item", EnterAction::Continue("- ".to_owned())),
            ("3. x", EnterAction::Continue("4. ".to_owned())),
            ("- [ ] x", EnterAction::Continue("- [ ] ".to_owned())),
            ("- [x] x", EnterAction::Continue("- [ ] ".to_owned())),
            ("> q", EnterAction::Continue("> ".to_owned())),
            ("- ", EnterAction::EndList { strip_from: 0 }),
            ("", EnterAction::Plain),
            ("9. x", EnterAction::Continue("10. ".to_owned())),
            ("plain line", EnterAction::Plain),
            ("  - nested", EnterAction::Continue("  - ".to_owned())),
            ("  - ", EnterAction::EndList { strip_from: 2 }),
        ];
        for (input, expected) in cases {
            let got = enter_action(input);
            assert_eq!(got, *expected, "enter_action({:?})", input);
        }
    }

    #[test]
    fn test_indent_line() {
        // indent
        assert_eq!(indent_line("- item", false), Some("  - item".to_owned()));
        assert_eq!(
            indent_line("  - item", false),
            Some("    - item".to_owned())
        );
        // outdent
        assert_eq!(indent_line("  - item", true), Some("- item".to_owned()));
        assert_eq!(indent_line("- item", true), None); // no leading space to remove
        assert_eq!(indent_line("plain", false), None); // not a list line
        assert_eq!(indent_line("plain", true), None);
    }

    #[test]
    fn test_ac_context() {
        // Link context
        let buf = "see [[Zettel";
        let ctx = ac_context(buf, buf.len());
        assert!(
            matches!(ctx, AcContext::Link { ref query, .. } if query == "Zettel"),
            "expected Link{{Zettel}}, got {:?}",
            ctx
        );

        // Tag context
        let buf2 = "word #ta";
        let ctx2 = ac_context(buf2, buf2.len());
        assert!(
            matches!(ctx2, AcContext::Tag { ref query, .. } if query == "ta"),
            "expected Tag{{ta}}, got {:?}",
            ctx2
        );

        // Heading line - not a tag autocomplete
        let buf3 = "# heading";
        let ctx3 = ac_context(buf3, buf3.len());
        assert!(
            matches!(ctx3, AcContext::None),
            "heading should be None, got {:?}",
            ctx3
        );

        // Mid-word # not a tag
        let buf4 = "a#b";
        let ctx4 = ac_context(buf4, buf4.len());
        assert!(
            matches!(ctx4, AcContext::None),
            "a#b should be None, got {:?}",
            ctx4
        );

        // Closed bracket - no longer in link
        let buf5 = "see [[Foo]]";
        let ctx5 = ac_context(buf5, buf5.len());
        assert!(
            matches!(ctx5, AcContext::None),
            "closed [[ should be None, got {:?}",
            ctx5
        );
    }

    #[test]
    fn test_is_probably_url() {
        assert!(is_probably_url("https://example.com"));
        assert!(is_probably_url("http://x.com/path?q=1"));
        assert!(!is_probably_url("not a url"));
        assert!(!is_probably_url("ftp://x"));
        assert!(!is_probably_url(""));
        assert!(!is_probably_url("file:///home/user"));
    }
}
