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
        }
    }
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
    // SAFETY: the guard lives for the duration of this function; the closure
    // borrows the guard's data.  We transmute the lifetime so the closure can
    // be stored as `dyn Fn + '_` without the borrow checker fighting the
    // `layouter` closure below.  This is safe because both closures are stack-
    // allocated and outlive each other exactly.
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

        let resp = ui.add(te);

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
