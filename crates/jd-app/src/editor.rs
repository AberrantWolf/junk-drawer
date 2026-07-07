//! Editor internals. This task: the mixed-size layouter (Spike A).
//! The floating editor window, behaviors, autocomplete arrive in Tasks 10-12.

use std::collections::HashMap;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::sync::Arc;

use eframe::egui::{self, text::LayoutJob};
use jd_core::lexer::{LineState, SpanStyle, StyledSpan, lex_line};

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
