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

/// Render a card face at `rect` (desk-space; caller has already applied any
/// viewport transform).  Returns the egui Response for click/drag/focus.
///
/// Rendering order:
///   1. Shadow (3 px offset)
///   2. Outline fill (paper cream / plain bg)
///   3. Ruled lines (IndexCard/Literature + Paper only)
///   4. Divider tab bg + title-on-tab  /  Literature footer strip
///   5. Title galley (inter-bold 17.0) for non-Scrap, non-Divider
///   6. Body galley (layout_body, clipped, 10 px margin)
///   7. Focus ring (2 px `focus_ring`) when `face.focused`
pub fn card_face(
    ui: &mut egui::Ui,
    rect: egui::Rect,
    face: &CardFace<'_>,
    th: &Theme,
    cache: &mut LineCache,
) -> egui::Response {
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

    let label = card_a11y_label(
        face.title,
        first_body_line,
        face.shape == CardShape::Scrap,
        face.links,
        face.tags,
    );
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, label.clone()));

    let painter = ui.painter();

    // -----------------------------------------------------------------------
    // 1. Shadow
    // -----------------------------------------------------------------------
    let shadow_rect = rect.translate(egui::vec2(3.0, 3.0));
    painter.rect_filled(shadow_rect, 4.0, th.card_shadow);

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
    // 5. Title galley (non-Scrap, non-Divider)
    // -----------------------------------------------------------------------
    let body_top = if face.shape == CardShape::Literature {
        // Leave room for the title above the body, but still place it
        rect.min.y + 8.0
    } else {
        rect.min.y + 10.0
    };

    if face.shape != CardShape::Scrap && face.shape != CardShape::Divider && !face.title.is_empty()
    {
        let title_font = egui::FontId::new(17.0, egui::FontFamily::Name("inter-bold".into()));
        painter.text(
            egui::pos2(rect.min.x + 10.0, body_top),
            egui::Align2::LEFT_TOP,
            face.title,
            title_font,
            th.text,
        );
    }

    // -----------------------------------------------------------------------
    // 6. Body galley
    // -----------------------------------------------------------------------
    if let Some(body_text) = face.body {
        // For Scrap: start at top with 10px margin; no separate title.
        // For Literature: start below title + leave footer room.
        // For Divider/IndexCard: start below title with margin.
        let content_top = match face.shape {
            CardShape::Scrap => rect.min.y + 10.0,
            CardShape::Divider => rect.min.y + 10.0,
            CardShape::Literature => body_top + 22.0,
            CardShape::IndexCard => body_top + 22.0,
        };
        let content_bottom = if face.shape == CardShape::Literature {
            rect.max.y - FOOTER_H - 4.0
        } else {
            rect.max.y - 10.0
        };

        let wrap_width = rect.width() - 20.0;
        let galley = layout_body(ui, body_text, wrap_width, cache, &|_| false, th);

        let clip_rect = egui::Rect::from_min_max(
            egui::pos2(rect.min.x + 10.0, content_top),
            egui::pos2(rect.max.x - 10.0, content_bottom),
        );

        // Paint with clipping
        let painter_clipped = ui.painter().with_clip_rect(clip_rect);
        painter_clipped.galley(egui::pos2(rect.min.x + 10.0, content_top), galley, th.text);
    }

    // -----------------------------------------------------------------------
    // 7. Focus ring
    // -----------------------------------------------------------------------
    if face.focused {
        painter.rect_stroke(
            sense_rect,
            4.0,
            egui::Stroke::new(2.0, th.focus_ring),
            egui::StrokeKind::Outside,
        );
    }

    resp
}
