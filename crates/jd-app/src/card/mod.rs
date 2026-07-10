pub mod shape;

use crate::editor::{LineCache, layout_body};
use crate::surfaces::desk::card_a11y_label;
use crate::theme::Theme;
use eframe::egui;
use jd_core::id::NoteId;
use shape::{
    CardShape, CardStyle, DIVIDER_TAB, FOOTER_H, RuledLines, divider_full_rect, outline, rules,
};

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// All the data needed to render one card face.  Borrows from the caller's storage.
pub struct CardFace<'a> {
    pub id: NoteId,
    pub title: &'a str,        // "" for scraps
    pub body: Option<&'a str>, // None → blank face (body not yet loaded)
    pub shape: CardShape,
    pub style: CardStyle,
    pub lines: RuledLines,
    pub source: Option<&'a str>, // literature footer text
    pub links: usize,
    pub tags: usize,
    pub focused: bool,
}

// ---------------------------------------------------------------------------
// card_face
// ---------------------------------------------------------------------------

/// Transform task box markers for face-side display only.
///
/// Replaces `"- [ ] "` with `"- □ "` and `"- [x] "` / `"- [X] "` with `"- ■ "`
/// in each line. Returns the transformed text and, for each task box, the
/// 0-based char offset of the `□`/`■` glyph within the returned string.
/// The raw source body is NOT modified.
///
/// Lines inside code fences (``` … ```) are skipped — a line whose
/// `trim_start()` starts with ` ``` ` toggles `in_fence`; while
/// `in_fence` is true, no task recognition is performed.  This mirrors the
/// rule used by `jd_core::lexer::lex_line` exactly.
pub fn face_body_with_checkbox_glyphs(raw: &str) -> (String, Vec<usize>) {
    let mut out = String::with_capacity(raw.len());
    let mut glyph_char_offsets: Vec<usize> = Vec::new();
    let mut char_count = 0usize;
    let mut in_fence = false;
    for (i, line) in raw.split('\n').enumerate() {
        if i > 0 {
            out.push('\n');
            char_count += 1;
        }
        // Fence tracking: a line whose trimmed form starts with ``` toggles
        // in_fence (mirrors jd_core::lexer::lex_line behaviour).
        if line.trim_start().starts_with("```") {
            in_fence = !in_fence;
            out.push_str(line);
            char_count += line.chars().count();
            continue;
        }
        // While inside a fence, copy verbatim — no task recognition.
        if in_fence {
            out.push_str(line);
            char_count += line.chars().count();
            continue;
        }
        // Check for "- [ ] " or "- [x] " / "- [X] " prefix (after optional leading whitespace).
        let trimmed = line.trim_start();
        let indent = line.len() - trimmed.len();
        if trimmed.starts_with("- [ ] ")
            || trimmed.starts_with("- [x] ")
            || trimmed.starts_with("- [X] ")
        {
            // "- " (2 bytes) + "[ ]" or "[x]" (3 bytes) + " " (1 byte) = "- [ ] " (6 bytes)
            let is_checked = !trimmed.starts_with("- [ ] ");
            // Write indent + "- " (ListMarker part, preserved byte-for-byte)
            out.push_str(&line[..indent]);
            out.push_str("- ");
            char_count += indent + 2; // indent chars (ascii) + "- "
            // Record char offset of the glyph.
            glyph_char_offsets.push(char_count);
            // Write the glyph.  □ (U+25A1) for unchecked, ■ (U+25A0) for checked —
            // both are covered by the bundled Inter font.
            let glyph = if is_checked { '■' } else { '□' };
            out.push(glyph);
            char_count += 1; // one char for the glyph
            // Write the rest after the task box marker ("[ ] " or "[x] " = 4 bytes from trimmed[2..6]).
            let rest = &trimmed[6..]; // skip "[ ] " or "[x] " (4 bytes after "- ")
            out.push(' ');
            out.push_str(rest);
            char_count += 1 + rest.chars().count();
        } else {
            out.push_str(line);
            char_count += line.chars().count();
        }
    }
    (out, glyph_char_offsets)
}

/// Toggle the Nth (0-based ordinal) task box in the raw body string.
/// Returns the new body with `"- [ ]"` ↔ `"- [x]"` toggled.
/// If the ordinal is out of range, returns the body unchanged.
///
/// Recognition matches `face_body_with_checkbox_glyphs` exactly:
/// - Lines are iterated; task boxes are recognised only at the start of the
///   line (after optional leading whitespace).
/// - Lines inside code fences (``` … ```) are skipped.  A line whose
///   `trim_start()` starts with ` ``` ` toggles `in_fence`.
pub fn toggle_task_box(raw: &str, ordinal: usize) -> String {
    let mut in_fence = false;
    // Collect (byte_start, is_unchecked) for each recognised task box.
    let mut boxes: Vec<(usize, bool)> = Vec::new();
    let mut line_start = 0usize;
    // Split into lines while tracking byte offsets.
    for line in raw.split('\n') {
        let trimmed = line.trim_start();
        // Fence tracking (mirrors lex_line rule).
        if trimmed.starts_with("```") {
            in_fence = !in_fence;
            line_start += line.len() + 1; // +1 for '\n'
            continue;
        }
        if !in_fence {
            let indent = line.len() - trimmed.len();
            // Task-box patterns, line-start-only (after optional indent).
            let is_unchecked = trimmed.starts_with("- [ ] ");
            let is_checked = trimmed.starts_with("- [x] ") || trimmed.starts_with("- [X] ");
            if is_unchecked || is_checked {
                // Byte offset of the '[' in the raw string.
                let bracket_pos = line_start + indent + 2; // "- " = 2 bytes
                boxes.push((bracket_pos, is_unchecked));
            }
        }
        line_start += line.len() + 1;
    }
    // Apply the toggle at the ordinal position.
    if let Some(&(bracket_pos, is_unchecked)) = boxes.get(ordinal) {
        let mut result = String::with_capacity(raw.len());
        result.push_str(&raw[..bracket_pos]);
        if is_unchecked {
            result.push_str("[x] ");
        } else {
            result.push_str("[ ] ");
        }
        result.push_str(&raw[bracket_pos + 4..]); // skip old "[ ] " or "[x] "
        result
    } else {
        // Ordinal not found; return unchanged.
        raw.to_owned()
    }
}

/// Render a card face at `rect` (desk-space; caller has already applied any
/// viewport transform).  Returns `(response, toggled_ordinal)` where
/// `toggled_ordinal` is `Some(n)` when the user clicked the Nth face-side
/// checkbox (0-based), and `None` otherwise.
///
/// Rendering order:
///   1. Shadow (3 px offset)
///   2. Outline fill (paper cream / plain bg)
///   3. Ruled lines (IndexCard/Literature + Paper only)
///   4. Divider tab bg + title-on-tab  /  Literature footer strip
///   5. (removed) — title IS the body's first heading; layout_body renders it
///   6. Body galley (layout_body, clipped, 10 px margin)
///   7. Focus ring (2 px `focus_ring`) when `face.focused`
pub fn card_face(
    ui: &mut egui::Ui,
    rect: egui::Rect,
    face: &CardFace<'_>,
    th: &Theme,
    cache: &mut LineCache,
) -> (egui::Response, Option<usize>) {
    // -----------------------------------------------------------------------
    // AccessKit / interaction response
    // -----------------------------------------------------------------------
    // For Divider the outline protrudes above rect; use divider_full_rect for
    // the interaction hit-test so the tab is also draggable.
    let sense_rect = if face.shape == CardShape::Divider {
        divider_full_rect(rect)
    } else {
        rect
    };

    let resp = ui.allocate_rect(sense_rect, egui::Sense::click_and_drag());

    // First body line (for Scrap label / fallback)
    let first_body_line = face
        .body
        .unwrap_or("")
        .lines()
        .next()
        .unwrap_or("")
        .trim_start_matches('#')
        .trim();

    // Build the a11y label inside the closure (move) so no `.clone()` is needed.
    let (title, fbl, is_scrap, links, tags) = (
        face.title.to_owned(),
        first_body_line.to_owned(),
        face.shape == CardShape::Scrap,
        face.links,
        face.tags,
    );
    resp.widget_info(move || {
        egui::WidgetInfo::labeled(
            egui::WidgetType::Button,
            true,
            card_a11y_label(&title, &fbl, is_scrap, links, tags),
        )
    });

    let painter = ui.painter();

    // -----------------------------------------------------------------------
    // 1. Shadow — two layers for a softer, more physical read:
    //    a wider low-alpha ambient halo plus a tight contact shadow, both
    //    cast downward (paper on felt, light from above).
    // -----------------------------------------------------------------------
    use crate::theme::{
        CARD_CORNER_RADIUS, CARD_SHADOW_NEAR_OFFSET, CARD_SHADOW_SOFT_EXPAND,
        CARD_SHADOW_SOFT_OFFSET,
    };
    painter.rect_filled(
        rect.expand(CARD_SHADOW_SOFT_EXPAND)
            .translate(CARD_SHADOW_SOFT_OFFSET),
        CARD_CORNER_RADIUS + CARD_SHADOW_SOFT_EXPAND,
        th.card_shadow_soft,
    );
    painter.rect_filled(
        rect.translate(CARD_SHADOW_NEAR_OFFSET),
        CARD_CORNER_RADIUS,
        th.card_shadow,
    );

    // -----------------------------------------------------------------------
    // 2. Outline fill
    // -----------------------------------------------------------------------
    let fill_color = match face.style {
        CardStyle::Paper => th.card_paper_cream,
        CardStyle::Plain => th.card_plain_bg,
    };

    let pts = outline(face.shape, face.style, rect, face.id);
    painter.add(egui::Shape::convex_polygon(
        pts.clone(),
        fill_color,
        egui::Stroke::NONE,
    ));
    // Border stroke drawn as a separate path-stroke (convex_polygon may not
    // handle stroke on non-convex shapes gracefully).
    painter.add(egui::Shape::closed_line(
        pts,
        egui::Stroke::new(1.0, th.card_border),
    ));

    // -----------------------------------------------------------------------
    // 3. Ruled lines (Paper + IndexCard/Literature only)
    // -----------------------------------------------------------------------
    if face.style == CardStyle::Paper
        && matches!(face.shape, CardShape::IndexCard | CardShape::Literature)
    {
        let rule_lines = rules(face.lines, rect.height(), th);
        for (local_y, color) in rule_lines {
            let y = rect.min.y + local_y;
            if y < rect.min.y || y > rect.max.y {
                continue;
            }
            painter.line_segment(
                [
                    egui::pos2(rect.min.x + 6.0, y),
                    egui::pos2(rect.max.x - 6.0, y),
                ],
                egui::Stroke::new(0.8, color),
            );
        }
    }

    // -----------------------------------------------------------------------
    // 4a. Divider tab background + title on tab
    // -----------------------------------------------------------------------
    if face.shape == CardShape::Divider {
        let tw = DIVIDER_TAB.x.min(rect.width());
        let th_size = DIVIDER_TAB.y;
        let tab_rect = egui::Rect::from_min_size(
            egui::pos2(rect.min.x, rect.min.y - th_size),
            egui::vec2(tw, th_size),
        );
        painter.rect_filled(tab_rect, egui::CornerRadius::same(2), th.divider_tab_bg);
        // Title on tab
        if !face.title.is_empty() {
            let tab_font = egui::FontId::new(12.0, egui::FontFamily::Name("inter-bold".into()));
            painter.text(
                tab_rect.center(),
                egui::Align2::CENTER_CENTER,
                face.title,
                tab_font,
                th.text,
            );
        }
    }

    // -----------------------------------------------------------------------
    // 4b. Literature footer strip + source text
    // -----------------------------------------------------------------------
    if face.shape == CardShape::Literature {
        let footer_rect = egui::Rect::from_min_size(
            egui::pos2(rect.min.x, rect.max.y - FOOTER_H),
            egui::vec2(rect.width(), FOOTER_H),
        );
        painter.rect_filled(footer_rect, egui::CornerRadius::ZERO, th.footer_bg);
        if let Some(src) = face.source {
            let footer_font =
                egui::FontId::new(11.0, egui::FontFamily::Name("inter-italic".into()));
            painter.text(
                egui::pos2(footer_rect.min.x + 8.0, footer_rect.center().y),
                egui::Align2::LEFT_CENTER,
                src,
                footer_font,
                th.text_weak,
            );
        }
    }

    // -----------------------------------------------------------------------
    // 5. Title galley — REMOVED for IndexCard/Literature.
    //
    // In this vault format the note title IS the body's first `#` heading, so
    // the body galley (layout_body) already renders it large/bold.  Painting a
    // separate title string here would duplicate it on every real card face.
    //
    // Divider: title lives on the tab (step 4a); no title in body area.
    // Scrap:   no title concept.
    // -----------------------------------------------------------------------

    // -----------------------------------------------------------------------
    // 6. Body galley
    // -----------------------------------------------------------------------
    let mut toggled_ordinal: Option<usize> = None;
    if let Some(body_text) = face.body {
        // For IndexCard/Literature the body galley starts near the top — the
        // first heading line IS the title, rendered large/bold by layout_body.
        // For Scrap/Divider: also start at top with 10 px margin.
        // Literature: leave room at the bottom for the footer strip.
        let content_top = rect.min.y + 10.0;
        let content_bottom = if face.shape == CardShape::Literature {
            rect.max.y - FOOTER_H - 4.0
        } else {
            rect.max.y - 10.0
        };

        // For Divider: the title is already on the tab, so strip the first
        // heading line from the body before rendering to avoid duplication.
        // (The shared layouter must keep the raw source byte-exact for the
        // editor's cursor mapping; we do the strip here, face-side only.)
        let divider_stripped: Option<String> =
            if face.shape == CardShape::Divider && !face.title.is_empty() {
                let first_line_end = body_text
                    .find('\n')
                    .map(|i| i + 1)
                    .unwrap_or(body_text.len());
                let first_line = body_text[..first_line_end]
                    .trim_end_matches('\n')
                    .trim_start_matches('#')
                    .trim();
                if first_line == face.title {
                    Some(body_text[first_line_end..].to_owned())
                } else {
                    None
                }
            } else {
                None
            };
        let after_divider_strip: &str = divider_stripped.as_deref().unwrap_or(body_text);

        // Face-side text transform: replace "- [ ]" / "- [x]" with ☐ / ☑.
        // Presentation only; the editor and the raw body are unaffected.
        let (face_body, checkbox_char_offsets) =
            face_body_with_checkbox_glyphs(after_divider_strip);

        let wrap_width = rect.width() - 20.0;
        let galley = layout_body(ui, &face_body, wrap_width, cache, &|_| false, th, false);

        let galley_pos = egui::pos2(rect.min.x + 10.0, content_top);

        // Whole-row clipping: never cut a text row mid-glyph. The clip bottom
        // is the bottom of the last row that fits fully in the content area
        // (falling back to the raw content_bottom when not even one row fits).
        let avail_h = content_bottom - content_top;
        let mut whole_rows_h = 0.0_f32;
        for row in &galley.rows {
            let bottom = row.pos.y + row.rect().height();
            if bottom <= avail_h + 0.5 {
                whole_rows_h = whole_rows_h.max(bottom);
            } else {
                break;
            }
        }
        let clip_bottom = if whole_rows_h > 0.0 {
            content_top + whole_rows_h
        } else {
            content_bottom
        };
        let clip_rect =
            egui::Rect::from_min_max(galley_pos, egui::pos2(rect.max.x - 10.0, clip_bottom));

        // Paint with clipping
        let painter_clipped = ui.painter().with_clip_rect(clip_rect);
        painter_clipped.galley(galley_pos, galley.clone(), th.text);

        // Click-to-toggle: if the card was clicked, check if the pointer is
        // over a checkbox glyph row, using ordinal matching.
        // Only handle clicks (not drags); use the raw pointer position since
        // card_face already has the click registered on `resp`.
        if resp.clicked()
            && !checkbox_char_offsets.is_empty()
            && let Some(ptr) = ui.input(|i| i.pointer.interact_pos())
        {
            // Convert screen pointer to galley-local position.
            let local = ptr - galley_pos.to_vec2();
            for (ordinal, &char_off) in checkbox_char_offsets.iter().enumerate() {
                // Find the cursor rect for the checkbox glyph in galley-local space.
                let cursor = egui::text::CCursor::new(char_off);
                let cursor_rect = galley.pos_from_cursor(cursor);
                // Expand the cursor rect to the full row height for easy hitting.
                let row_h = cursor_rect.height().max(18.0);
                let hit_rect = egui::Rect::from_min_max(
                    egui::pos2(cursor_rect.min.x, cursor_rect.min.y - row_h * 0.1),
                    egui::pos2(cursor_rect.min.x + row_h, cursor_rect.max.y + row_h * 0.1),
                );
                if hit_rect.contains(egui::pos2(local.x, local.y)) {
                    toggled_ordinal = Some(ordinal);
                    break;
                }
            }
        }
    }

    // -----------------------------------------------------------------------
    // 7. Focus ring
    // -----------------------------------------------------------------------
    if face.focused {
        painter.rect_stroke(
            sense_rect,
            CARD_CORNER_RADIUS,
            egui::Stroke::new(2.0, th.focus_ring),
            egui::StrokeKind::Outside,
        );
    }

    (resp, toggled_ordinal)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn face_body_replaces_task_boxes_with_glyphs() {
        let raw = "# Title\n- [ ] unchecked\n- [x] checked\n- [X] also checked\n- plain item";
        let (face, offsets) = face_body_with_checkbox_glyphs(raw);
        // Glyph replacements — □ (U+25A1) / ■ (U+25A0), both in Inter.
        assert!(face.contains('□'), "must contain □ for unchecked");
        assert!(face.contains('■'), "must contain ■ for checked");
        // Raw markers must not appear in the face body
        assert!(
            !face.contains("- [ ]"),
            "raw unchecked marker must not appear in face body"
        );
        assert!(
            !face.contains("- [x]"),
            "raw checked marker must not appear in face body"
        );
        // Three task boxes
        assert_eq!(offsets.len(), 3, "must have 3 checkbox char offsets");
        // Non-task lines are unchanged
        assert!(face.contains("plain item"), "non-task lines preserved");
        assert!(face.contains("# Title"), "heading preserved");
    }

    #[test]
    fn toggle_task_box_unchecked_to_checked() {
        let raw = "- [ ] task one\n- [ ] task two\n- [x] task three";
        let result = toggle_task_box(raw, 0);
        assert!(
            result.contains("- [x] task one"),
            "first box must be checked, got: {result:?}"
        );
        assert!(
            result.contains("- [ ] task two"),
            "second box unchanged, got: {result:?}"
        );
    }

    #[test]
    fn toggle_task_box_checked_to_unchecked() {
        let raw = "- [ ] task one\n- [x] task two";
        let result = toggle_task_box(raw, 1);
        assert!(
            result.contains("- [ ] task two"),
            "second box (checked→unchecked), got: {result:?}"
        );
        assert!(
            result.contains("- [ ] task one"),
            "first box unchanged, got: {result:?}"
        );
    }

    #[test]
    fn toggle_task_box_ordinal_out_of_range_returns_unchanged() {
        let raw = "- [ ] only one box";
        let result = toggle_task_box(raw, 5);
        assert_eq!(
            result, raw,
            "out-of-range ordinal must return body unchanged"
        );
    }

    #[test]
    fn face_body_preserves_non_task_content() {
        let raw = "# Heading\nplain text\n**bold**";
        let (face, offsets) = face_body_with_checkbox_glyphs(raw);
        assert_eq!(face, raw, "no task boxes: face body must equal raw");
        assert!(offsets.is_empty(), "no checkbox offsets for task-free body");
    }

    /// A "- [ ]" line inside a code fence must not produce a glyph on the
    /// face and must not be counted as a task box ordinal.
    #[test]
    fn face_body_skips_task_boxes_inside_code_fence() {
        let raw = "- [ ] real task one\n```\n- [ ] fenced fake\n```\n- [ ] real task two";
        let (face, offsets) = face_body_with_checkbox_glyphs(raw);
        // Only the two real tasks get glyphs.
        assert_eq!(
            offsets.len(),
            2,
            "fenced task must not produce a glyph offset"
        );
        // The fenced line must appear verbatim (no glyph substitution).
        assert!(
            face.contains("- [ ] fenced fake"),
            "fenced task line must be copied verbatim, got: {face:?}"
        );
        // The real tasks must be substituted.
        assert!(
            face.contains("□ real task one"),
            "first real task must get □, got: {face:?}"
        );
        assert!(
            face.contains("□ real task two"),
            "second real task must get □, got: {face:?}"
        );
    }

    /// Toggling ordinal 1 (the SECOND real task) must skip the fenced line
    /// and toggle only "real task two".  Byte positions are asserted exactly.
    #[test]
    fn toggle_task_box_skips_fenced_tasks() {
        let raw = "- [ ] real task one\n```\n- [ ] fenced fake\n```\n- [ ] real task two";
        // ordinal 0 → real task one
        let r0 = toggle_task_box(raw, 0);
        assert!(
            r0.contains("- [x] real task one"),
            "ordinal 0 must toggle first real task, got: {r0:?}"
        );
        assert!(
            r0.contains("- [ ] fenced fake"),
            "fenced line must be untouched, got: {r0:?}"
        );
        assert!(
            r0.contains("- [ ] real task two"),
            "second real task must be untouched, got: {r0:?}"
        );
        // ordinal 1 → real task two (fenced one is ordinal-invisible)
        let r1 = toggle_task_box(raw, 1);
        assert!(
            r1.contains("- [ ] real task one"),
            "first real task must be untouched, got: {r1:?}"
        );
        assert!(
            r1.contains("- [ ] fenced fake"),
            "fenced line must be untouched, got: {r1:?}"
        );
        assert!(
            r1.contains("- [x] real task two"),
            "ordinal 1 must toggle second real task, got: {r1:?}"
        );
        // Byte-assert: "real task two" starts toggled at exact position.
        let expected_byte_off = raw.rfind("- [ ] real task two").unwrap();
        assert_eq!(
            &r1[expected_byte_off..expected_byte_off + 19],
            "- [x] real task two",
            "toggled bytes must be at exact offset"
        );
    }
}
