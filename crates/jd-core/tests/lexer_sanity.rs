//! Randomized lexer invariants (spec §13): for arbitrary input lines, spans
//! ascend, never overlap, cover the whole line exactly, land on char
//! boundaries, and lexing is deterministic. Fence state only changes on
//! fence-marker lines.

use jd_core::lexer::{LineState, SpanStyle, lex_line};
use jd_core::rng::Xorshift128;

const FRAGMENTS: &[&str] = &[
    "# ",
    "## ",
    "#### ",
    "#",
    "#tag",
    "> ",
    "- ",
    "- [ ] ",
    "- [x] ",
    "1. ",
    "```",
    "``",
    "`",
    "`code`",
    "**",
    "*",
    "***",
    "~~",
    "~~x~~",
    "**bold**",
    "*i*",
    "[[",
    "]]",
    "[[Link]]",
    "[[a|b]]",
    "[",
    "]",
    "(",
    ")",
    "[t](u)",
    "https://x.io",
    "http://",
    "plain words ",
    "  ",
    "\t",
    "é",
    "日本語",
    "🎉",
    "نص",
    "x#y",
    " #t ",
    "a*b*c",
    "|",
];

fn gen_line(rng: &mut Xorshift128) -> String {
    let n = rng.gen_range(0..12);
    let mut line = String::new();
    for _ in 0..n {
        line.push_str(FRAGMENTS[rng.gen_range(0..FRAGMENTS.len() as u64) as usize]);
    }
    line
}

fn assert_invariants(line: &str, entry: LineState) -> LineState {
    let resolve = |t: &str| t.len().is_multiple_of(2); // arbitrary but deterministic
    let (spans, state) = lex_line(line, entry, &resolve);
    let (spans2, state2) = lex_line(line, entry, &resolve);
    assert_eq!(spans, spans2, "non-deterministic lex of {line:?}");
    assert_eq!(state, state2);

    let mut cursor = 0;
    for s in &spans {
        assert_eq!(
            s.range.start, cursor,
            "gap or overlap at {} in {line:?}: {spans:?}",
            s.range.start
        );
        assert!(
            s.range.start < s.range.end,
            "empty span in {line:?}: {spans:?}"
        );
        assert!(
            line.is_char_boundary(s.range.start) && line.is_char_boundary(s.range.end),
            "span splits UTF-8 in {line:?}: {:?}",
            s.range
        );
        cursor = s.range.end;
    }
    assert_eq!(cursor, line.len(), "spans don't cover {line:?}: {spans:?}");
    state
}

#[test]
fn randomized_lines_uphold_invariants() {
    let mut rng = Xorshift128::new(0x0001_E4E4);
    for _ in 0..2000 {
        let line = gen_line(&mut rng);
        assert_invariants(&line, LineState::Normal);
        assert_invariants(&line, LineState::InCodeFence);
    }
}

#[test]
fn fence_state_threads_across_lines() {
    let mut rng = Xorshift128::new(0xFE2CE);
    let mut state = LineState::Normal;
    for _ in 0..2000 {
        let line = gen_line(&mut rng);
        let before = state;
        state = assert_invariants(&line, state);
        let is_marker = line.trim_start().starts_with("```");
        if is_marker {
            assert_ne!(state, before, "fence marker must flip state: {line:?}");
        } else if !line.is_empty() {
            assert_eq!(state, before, "state changed on non-marker line: {line:?}");
        }
    }
}

#[test]
fn code_fence_content_is_never_styled() {
    let resolve = |_: &str| true;
    let (_, s1) = lex_line("```rust", LineState::Normal, &resolve);
    assert_eq!(s1, LineState::InCodeFence);
    let (spans, _) = lex_line("[[link]] #tag **b** `c`", s1, &resolve);
    assert_eq!(spans.len(), 1);
    assert_eq!(spans[0].style, SpanStyle::CodeBlock);
}
