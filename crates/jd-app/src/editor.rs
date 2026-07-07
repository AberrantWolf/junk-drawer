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
pub fn layout_body(
    ui: &egui::Ui,
    text: &str,
    wrap_width: f32,
    cache: &mut LineCache,
    resolve: &dyn Fn(&str) -> bool,
    theme: &crate::theme::Theme,
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
    /// Cursor state from the previous frame (for URL-paste-over-selection).
    prev_cursor: Option<egui::text::CCursorRange>,
}

impl EditorState {
    /// Open a new editor for `id` pre-loaded with `body` (body-only, no frontmatter).
    pub fn open(id: NoteId, body: String) -> EditorState {
        let undo = crate::text_undo::TextUndo::new(&body);
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
            prev_cursor: None,
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

        // --- URL paste intercept (before TextEdit) ---
        // If the paste is a URL and we had a selection, transform to [text](url).
        if let Some(prev_cr) = ed.prev_cursor {
            let had_selection = prev_cr.primary != prev_cr.secondary;
            if had_selection {
                let mut paste_url: Option<String> = Option::None;
                ui.input_mut(|i| {
                    for ev in i.events.iter_mut() {
                        if let egui::Event::Paste(text) = ev
                            && is_probably_url(text)
                        {
                            paste_url = Some(text.clone());
                            // Blank out the raw paste so TextEdit doesn't insert it.
                            *ev = egui::Event::Paste(String::new());
                        }
                    }
                });
                if let Some(url) = paste_url {
                    let lo = prev_cr.primary.index.0.min(prev_cr.secondary.index.0);
                    let hi = prev_cr.primary.index.0.max(prev_cr.secondary.index.0);
                    let byte_lo = char_idx_to_byte(&ed.buffer, lo);
                    let byte_hi = char_idx_to_byte(&ed.buffer, hi);
                    let selected_text = ed.buffer[byte_lo..byte_hi].to_owned();
                    let md_link = format!("[{}]({})", selected_text, url);
                    ed.buffer.replace_range(byte_lo..byte_hi, &md_link);
                    ed.dirty = true;
                    ed.last_edit = Some(Instant::now());
                }
            }
        }

        // Build the layouter closure; borrows `ed.cache` and `resolve_fn`.
        let cache = &mut ed.cache;
        let theme = deps.theme;
        let mut layouter = |ui: &egui::Ui, buf: &dyn egui::TextBuffer, wrap: f32| {
            layout_body(ui, buf.as_str(), wrap, cache, &resolve_fn, theme)
        };

        let te = egui::TextEdit::multiline(&mut ed.buffer)
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

        // --- Enter key: list continuation ---
        let plain_enter = resp.has_focus()
            && ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Enter));
        if plain_enter && let Some(cr) = te_out.cursor_range {
            let cursor_byte = char_idx_to_byte(&ed.buffer, cr.primary.index.0);
            // Find line start
            let line_start = ed.buffer[..cursor_byte].rfind('\n').map_or(0, |p| p + 1);
            let line_before = &ed.buffer[line_start..cursor_byte].to_owned();
            match enter_action(line_before) {
                EnterAction::Plain => {
                    ed.buffer.insert(cursor_byte, '\n');
                }
                EnterAction::Continue(prefix) => {
                    let ins = format!("\n{}", prefix);
                    ed.buffer.insert_str(cursor_byte, &ins);
                }
                EnterAction::EndList { strip_from } => {
                    let strip_start = line_start + strip_from;
                    ed.buffer.replace_range(strip_start..cursor_byte, "\n");
                }
            }
            ed.dirty = true;
            ed.last_edit = Some(Instant::now());
        }

        // --- Tab / Shift+Tab: indent/outdent list lines ---
        let tab_pressed = resp.has_focus()
            && ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Tab));
        let shift_tab_pressed = resp.has_focus()
            && ui.input_mut(|i| i.consume_key(egui::Modifiers::SHIFT, egui::Key::Tab));
        if (tab_pressed || shift_tab_pressed)
            && let Some(cr) = te_out.cursor_range
        {
            let outdent = shift_tab_pressed;
            let cursor_byte = char_idx_to_byte(&ed.buffer, cr.primary.index.0);
            let line_start = ed.buffer[..cursor_byte].rfind('\n').map_or(0, |p| p + 1);
            let line_end = ed.buffer[cursor_byte..]
                .find('\n')
                .map_or(ed.buffer.len(), |p| cursor_byte + p);
            let line = ed.buffer[line_start..line_end].to_owned();
            if let Some(new_line) = indent_line(&line, outdent) {
                ed.buffer.replace_range(line_start..line_end, &new_line);
                ed.dirty = true;
                ed.last_edit = Some(Instant::now());
            }
        }

        // --- Autocomplete popup ---
        if let Some(cr) = te_out.cursor_range {
            let cursor_byte = char_idx_to_byte(&ed.buffer, cr.primary.index.0);
            let ctx = ac_context(&ed.buffer, cursor_byte);
            match ctx {
                AcContext::Link { start, ref query } => {
                    // Collect matching note titles via BM25 query.
                    let matches: Vec<String> = {
                        use jd_core::index::search::parse_query;
                        let q = parse_query(query);
                        let results = index_guard.query(&q, 10);
                        results
                            .into_iter()
                            .filter_map(|hit| index_guard.get(hit.id).and_then(|m| m.title.clone()))
                            .collect()
                    };
                    if !matches.is_empty() {
                        egui::Popup::from_response(&resp)
                            .id(egui::Id::new("ac_popup_link"))
                            .open(true)
                            .show(|ui| {
                                for (i, title) in matches.iter().enumerate() {
                                    let selected = i == ed.ac_selected;
                                    if ui.selectable_label(selected, title).clicked() {
                                        let byte_end =
                                            char_idx_to_byte(&ed.buffer, cr.primary.index.0);
                                        let insert_text = format!("{}]]", title);
                                        ed.buffer.replace_range(start + 2..byte_end, &insert_text);
                                        ed.dirty = true;
                                        ed.last_edit = Some(Instant::now());
                                        ed.ac_selected = 0;
                                    }
                                }
                            });
                    }
                }
                AcContext::Tag { start, ref query } => {
                    // Collect matching tags.
                    let matches: Vec<String> = {
                        let all_tags = index_guard.all_tags();
                        let q = query.to_lowercase();
                        all_tags
                            .into_iter()
                            .filter(|(t, _)| t.as_str().to_lowercase().contains(&q))
                            .take(10)
                            .map(|(t, _)| t.as_str().to_owned())
                            .collect()
                    };
                    if !matches.is_empty() {
                        egui::Popup::from_response(&resp)
                            .id(egui::Id::new("ac_popup_tag"))
                            .open(true)
                            .show(|ui| {
                                for (i, tag) in matches.iter().enumerate() {
                                    let selected = i == ed.ac_selected;
                                    if ui.selectable_label(selected, tag).clicked() {
                                        let byte_end =
                                            char_idx_to_byte(&ed.buffer, cr.primary.index.0);
                                        let insert_text = format!("{} ", tag);
                                        ed.buffer.replace_range(start + 1..byte_end, &insert_text);
                                        ed.dirty = true;
                                        ed.last_edit = Some(Instant::now());
                                        ed.ac_selected = 0;
                                    }
                                }
                            });
                    }
                }
                AcContext::None => {
                    ed.ac_selected = 0;
                }
            }
        }
    });

    // Esc to close (Modal::should_close handles Esc + backdrop click).
    if modal_resp.should_close() {
        close_requested = true;
    }

    // Autosave: dirty && last_edit > 1s → save, clear dirty, stay open.
    if ed.dirty
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
