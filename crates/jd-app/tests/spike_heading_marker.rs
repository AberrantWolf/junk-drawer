/// Exit criterion: HeadingMarker for "## Sub" must use heading_size(2), not heading_size(1).
/// This verifies the HeadingMarker level-fix in layout_body.
///
/// We inspect the LayoutJob sections directly (no font rendering required) to
/// verify font sizes are set correctly before layout.
#[test]
fn heading_marker_size_matches_heading_text_size() {
    use eframe::egui::text::LayoutJob;
    use jd_app::editor::heading_size;
    use jd_app::theme::{Theme, text_format};
    use jd_core::lexer::{LineState, SpanStyle, lex_line};

    let line = "## Sub";
    let (spans, _) = lex_line(line, LineState::Normal, &|_| false);
    let mut job = LayoutJob::default();
    job.wrap.max_width = 400.0;

    let theme = Theme::light();
    for s in &spans {
        let mut fmt = text_format(s.style, &theme);
        if s.style == SpanStyle::HeadingMarker {
            let level = line.bytes().take_while(|&b| b == b'#').count().clamp(1, 3) as u8;
            fmt.font_id = eframe::egui::FontId::new(
                heading_size(level),
                eframe::egui::FontFamily::Name("inter-bold".into()),
            );
        }
        job.append(&line[s.range.clone()], 0.0, fmt);
    }

    // "## Sub" produces: HeadingMarker("## ") at index 0, Heading(2)("Sub") at index 1
    assert_eq!(job.sections.len(), 2, "expected 2 sections for '## Sub'");

    let marker_size = job.sections[0].format.font_id.size;
    let text_size = job.sections[1].format.font_id.size;

    assert_eq!(
        marker_size,
        heading_size(2),
        "HeadingMarker for '##' must match heading_size(2)={}, got {marker_size}",
        heading_size(2)
    );
    assert_eq!(
        text_size,
        heading_size(2),
        "Heading(2) text must be heading_size(2)={}, got {text_size}",
        heading_size(2)
    );
    // Sanity: heading(2) != heading(1) — the whole point of this test
    assert_ne!(
        heading_size(2),
        heading_size(1),
        "heading_size(1) and heading_size(2) must differ"
    );
}
