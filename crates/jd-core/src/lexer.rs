//! Line-oriented markdown lexer: styled spans over raw source (spec §5).
//! No egui types — jd-app maps SpanStyle → TextFormat. Only code fences carry
//! state across lines. Span semantics per architecture decision §6.11:
//! emphasis spans include their delimiters, no nesting; heading rest is one
//! Heading(n) span; quote rests inline-lex with plain runs emitted as Quote.

use std::ops::Range;

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum LineState {
    #[default]
    Normal,
    InCodeFence,
}

#[derive(Clone, PartialEq, Debug)]
pub struct StyledSpan {
    pub range: Range<usize>,
    pub style: SpanStyle,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SpanStyle {
    Text,
    Heading(u8), // 1..=3; the text after the marker
    HeadingMarker,
    Bold,
    Italic,
    BoldItalic,
    Strike,
    InlineCode,
    CodeFenceMarker,
    CodeBlock,
    ListMarker,
    TaskBoxUnchecked,
    TaskBoxChecked,
    QuoteMarker,
    Quote,
    WikiLink { resolved: bool },
    Tag,
    Url,
    MdLinkText,
    MdLinkUrl,
}

fn span(range: Range<usize>, style: SpanStyle) -> StyledSpan {
    StyledSpan { range, style }
}

/// Lex one line (no terminator). `entry` carries fence state from the previous
/// line. Invariants: spans ascend, never overlap, cover the whole line exactly
/// (empty line → no spans), and boundaries are char boundaries.
pub fn lex_line(
    line: &str,
    entry: LineState,
    resolve: &dyn Fn(&str) -> bool,
) -> (Vec<StyledSpan>, LineState) {
    if line.is_empty() {
        return (Vec::new(), entry);
    }
    let is_fence_marker = line.trim_start().starts_with("```");
    if entry == LineState::InCodeFence {
        return if is_fence_marker {
            (
                vec![span(0..line.len(), SpanStyle::CodeFenceMarker)],
                LineState::Normal,
            )
        } else {
            (
                vec![span(0..line.len(), SpanStyle::CodeBlock)],
                LineState::InCodeFence,
            )
        };
    }
    if is_fence_marker {
        return (
            vec![span(0..line.len(), SpanStyle::CodeFenceMarker)],
            LineState::InCodeFence,
        );
    }

    let mut spans = Vec::new();

    // Heading: 1–3 '#' at column 0, then a space.
    let hashes = line.bytes().take_while(|&b| b == b'#').count();
    if (1..=3).contains(&hashes) && line.as_bytes().get(hashes) == Some(&b' ') {
        let marker_end = hashes + 1;
        spans.push(span(0..marker_end, SpanStyle::HeadingMarker));
        if marker_end < line.len() {
            spans.push(span(
                marker_end..line.len(),
                SpanStyle::Heading(hashes as u8),
            ));
        }
        return (spans, LineState::Normal);
    }

    let indent = line.len() - line.trim_start().len();
    let after_indent = &line[indent..];

    // Blockquote: indent + '>' + optional one space, all part of the marker.
    if let Some(rest) = after_indent.strip_prefix('>') {
        let marker_end = indent + 1 + usize::from(rest.starts_with(' '));
        spans.push(span(0..marker_end, SpanStyle::QuoteMarker));
        lex_inline(
            line,
            marker_end..line.len(),
            SpanStyle::Quote,
            resolve,
            &mut spans,
        );
        return (spans, LineState::Normal);
    }

    // Unordered list: indent + "- ", optional task box.
    if let Some(rest) = after_indent.strip_prefix("- ") {
        let dash_end = indent + 2;
        spans.push(span(0..dash_end, SpanStyle::ListMarker));
        let mut content_start = dash_end;
        for (pat, style) in [
            ("[ ] ", SpanStyle::TaskBoxUnchecked),
            ("[x] ", SpanStyle::TaskBoxChecked),
            ("[X] ", SpanStyle::TaskBoxChecked),
        ] {
            if rest.starts_with(pat) {
                spans.push(span(dash_end..dash_end + 3, style));
                content_start = dash_end + 3;
                break;
            }
        }
        lex_inline(
            line,
            content_start..line.len(),
            SpanStyle::Text,
            resolve,
            &mut spans,
        );
        return (spans, LineState::Normal);
    }

    // Ordered list: indent + digits + ". ".
    let digits = after_indent.bytes().take_while(u8::is_ascii_digit).count();
    if digits > 0 && after_indent[digits..].starts_with(". ") {
        let marker_end = indent + digits + 2;
        spans.push(span(0..marker_end, SpanStyle::ListMarker));
        lex_inline(
            line,
            marker_end..line.len(),
            SpanStyle::Text,
            resolve,
            &mut spans,
        );
        return (spans, LineState::Normal);
    }

    lex_inline(line, 0..line.len(), SpanStyle::Text, resolve, &mut spans);
    (spans, LineState::Normal)
}

/// Inline pass over `region`, emitting construct spans and `base`-styled gap
/// runs. Task 1 stub: everything is a gap run; Tasks 2–3 add constructs.
fn lex_inline(
    _line: &str,
    region: Range<usize>,
    base: SpanStyle,
    _resolve: &dyn Fn(&str) -> bool,
    out: &mut Vec<StyledSpan>,
) {
    if region.start < region.end {
        out.push(span(region, base));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn no_resolve(_: &str) -> bool {
        false
    }

    /// Lex with Normal entry and no resolution; assert exact spans.
    fn check(line: &str, expected: &[(std::ops::Range<usize>, SpanStyle)]) {
        let (spans, _) = lex_line(line, LineState::Normal, &no_resolve);
        let got: Vec<(std::ops::Range<usize>, SpanStyle)> =
            spans.into_iter().map(|s| (s.range, s.style)).collect();
        assert_eq!(got, expected, "spans for {line:?}");
    }

    #[test]
    fn empty_line_has_no_spans() {
        check("", &[]);
    }

    #[test]
    fn plain_text_is_one_span() {
        check("just words here", &[(0..15, SpanStyle::Text)]);
    }

    #[test]
    fn headings_one_to_three() {
        check(
            "# Title",
            &[
                (0..2, SpanStyle::HeadingMarker),
                (2..7, SpanStyle::Heading(1)),
            ],
        );
        check(
            "## Sub",
            &[
                (0..3, SpanStyle::HeadingMarker),
                (3..6, SpanStyle::Heading(2)),
            ],
        );
        check(
            "### Deep",
            &[
                (0..4, SpanStyle::HeadingMarker),
                (4..8, SpanStyle::Heading(3)),
            ],
        );
        // marker with no text after the space
        check("# ", &[(0..2, SpanStyle::HeadingMarker)]);
    }

    #[test]
    fn four_hashes_and_no_space_are_not_headings() {
        check("#### Too deep", &[(0..13, SpanStyle::Text)]);
        // '#nospace' is a tag (Task 3); at this task's stage it must simply NOT be a heading.
        let (spans, _) = lex_line("#nospace", LineState::Normal, &no_resolve);
        assert!(
            spans
                .iter()
                .all(|s| !matches!(s.style, SpanStyle::Heading(_) | SpanStyle::HeadingMarker))
        );
    }

    #[test]
    fn fence_lines_and_state_carry() {
        let (spans, state) = lex_line("```rust", LineState::Normal, &no_resolve);
        assert_eq!(
            spans,
            vec![StyledSpan {
                range: 0..7,
                style: SpanStyle::CodeFenceMarker
            }]
        );
        assert_eq!(state, LineState::InCodeFence);

        let (spans, state) = lex_line("# not a heading", LineState::InCodeFence, &no_resolve);
        assert_eq!(
            spans,
            vec![StyledSpan {
                range: 0..15,
                style: SpanStyle::CodeBlock
            }]
        );
        assert_eq!(state, LineState::InCodeFence);

        let (spans, state) = lex_line("```", LineState::InCodeFence, &no_resolve);
        assert_eq!(
            spans,
            vec![StyledSpan {
                range: 0..3,
                style: SpanStyle::CodeFenceMarker
            }]
        );
        assert_eq!(state, LineState::Normal);
    }

    #[test]
    fn blockquote_marker_and_quote_rest() {
        check(
            "> quoted words",
            &[(0..2, SpanStyle::QuoteMarker), (2..14, SpanStyle::Quote)],
        );
        // no space after '>' still quotes; indent belongs to the marker
        check(
            ">tight",
            &[(0..1, SpanStyle::QuoteMarker), (1..6, SpanStyle::Quote)],
        );
        check(
            "  > indented",
            &[(0..4, SpanStyle::QuoteMarker), (4..12, SpanStyle::Quote)],
        );
    }

    #[test]
    fn unordered_list_and_task_boxes() {
        check(
            "- item text",
            &[(0..2, SpanStyle::ListMarker), (2..11, SpanStyle::Text)],
        );
        check(
            "  - nested",
            &[(0..4, SpanStyle::ListMarker), (4..10, SpanStyle::Text)],
        );
        check(
            "- [ ] buy milk",
            &[
                (0..2, SpanStyle::ListMarker),
                (2..5, SpanStyle::TaskBoxUnchecked),
                (5..14, SpanStyle::Text),
            ],
        );
        check(
            "- [x] done",
            &[
                (0..2, SpanStyle::ListMarker),
                (2..5, SpanStyle::TaskBoxChecked),
                (5..10, SpanStyle::Text),
            ],
        );
        // '-' without a space is not a list
        check("-not a list", &[(0..11, SpanStyle::Text)]);
    }

    #[test]
    fn ordered_list_marker() {
        check(
            "1. first",
            &[(0..3, SpanStyle::ListMarker), (3..8, SpanStyle::Text)],
        );
        check(
            "12. twelfth",
            &[(0..4, SpanStyle::ListMarker), (4..11, SpanStyle::Text)],
        );
        check("1.no space", &[(0..10, SpanStyle::Text)]);
    }

    #[test]
    fn multibyte_plain_text() {
        let line = "日本語 and 🎉";
        let (spans, _) = lex_line(line, LineState::Normal, &no_resolve);
        assert_eq!(
            spans,
            vec![StyledSpan {
                range: 0..line.len(),
                style: SpanStyle::Text
            }]
        );
    }
}
