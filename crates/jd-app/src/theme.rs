//! All colors, fonts, and text-format mapping. Every color used anywhere in
//! jd-app is a named constant here, WCAG-checked by test.

use std::sync::Arc;

use eframe::egui::{self, Color32, FontData, FontDefinitions, FontFamily, FontId, TextFormat};
use jd_core::lexer::SpanStyle;

use crate::editor::{BODY_SIZE, MONO_SIZE, heading_size};

/// Rule pitch = the measured body row height: Inter at BODY_SIZE (15.0)
/// lays out at exactly 18.0 px/row (pinned by measurement — see WP5x Task 3).
/// Rules land UNDER text rows, never through them.
pub const RULE_SPACING: f32 = 18.0;
/// Natural red header rule, card-local y. The first heading line (H1, 24px)
/// lays out 29 px tall from content_top=10, so its bottom edge is y=39 — the
/// red rule sits just below it.
pub const RED_RULE_Y: f32 = 39.5;
/// First body rule (blue for Natural, ink for Ink): one body row below the
/// red header rule, i.e. under the first body text line (39..57).
pub const RULE_TOP_OFFSET: f32 = RED_RULE_Y + RULE_SPACING;

// ---------------------------------------------------------------------------
// Polish metrics (WP5x Task 3) — every tuned value is a named constant here
// so the pass stays reversible.
// ---------------------------------------------------------------------------

/// Corner radius shared by card faces, their shadows, and the focus ring.
pub const CARD_CORNER_RADIUS: f32 = 4.0;
/// Corner radius for overlay chrome (editor modal, palette).
pub const OVERLAY_CORNER_RADIUS: f32 = 8.0;
/// Card drop shadow, soft/ambient layer: a slightly larger halo below the card.
pub const CARD_SHADOW_SOFT_OFFSET: egui::Vec2 = egui::vec2(0.0, 5.0);
/// How far the soft shadow layer extends past the card edge.
pub const CARD_SHADOW_SOFT_EXPAND: f32 = 2.0;
/// Card drop shadow, near/contact layer: tight under the card.
pub const CARD_SHADOW_NEAR_OFFSET: egui::Vec2 = egui::vec2(0.0, 2.0);
/// Left rail width (was 160 — too tight for desk names + padding).
pub const RAIL_WIDTH: f32 = 192.0;
/// Left rail row height.
pub const RAIL_ROW_H: f32 = 26.0;
/// Status line height.
pub const STATUS_LINE_H: f32 = 28.0;
/// Inbox pile: outer margin and gutters between scraps (Paper pile layout).
pub const INBOX_PILE_MARGIN: f32 = 28.0;
pub const INBOX_PILE_GAP: f32 = 28.0;
/// Drawer mini grid: outer margin and gutters.
pub const DRAWER_GRID_MARGIN: f32 = 20.0;
pub const DRAWER_GRID_GAP: f32 = 20.0;

/// The one drop shadow used by overlay chrome (editor modal, palette):
/// consistent with the card shadows — soft, low-alpha, cast downward.
pub fn overlay_shadow() -> egui::Shadow {
    egui::Shadow {
        offset: [0, 6],
        blur: 24,
        spread: 0,
        color: Color32::from_black_alpha(56),
    }
}

/// Push the theme into egui's own widget styling so stock widgets (buttons,
/// panels, text inputs, menus) sit on the same warm stationery palette as the
/// custom-painted surfaces. Called from app.rs whenever the theme flips.
pub fn apply_visuals(ctx: &egui::Context, th: &Theme) {
    let mut v = if th.dark {
        egui::Visuals::dark()
    } else {
        egui::Visuals::light()
    };
    v.panel_fill = th.chrome_bg;
    v.window_fill = th.card_plain_bg;
    v.window_stroke = egui::Stroke::new(1.0, th.card_border);
    v.window_corner_radius = egui::CornerRadius::same(OVERLAY_CORNER_RADIUS as u8);
    v.window_shadow = overlay_shadow();
    // Text inputs (palette query, rename field) on plain card stock.
    v.extreme_bg_color = th.card_plain_bg;
    // Warm the stock widget fills (buttons on chrome) + one shared radius.
    v.widgets.inactive.weak_bg_fill = th.rail_hover_bg;
    v.widgets.hovered.weak_bg_fill = th.separator;
    v.widgets.noninteractive.bg_stroke = egui::Stroke::new(1.0, th.separator);
    let r = egui::CornerRadius::same(CARD_CORNER_RADIUS as u8);
    for w in [
        &mut v.widgets.noninteractive,
        &mut v.widgets.inactive,
        &mut v.widgets.hovered,
        &mut v.widgets.active,
        &mut v.widgets.open,
    ] {
        w.corner_radius = r;
    }
    ctx.set_visuals(v);
}

pub fn font_definitions() -> FontDefinitions {
    let mut d = FontDefinitions::default();
    let fonts: &[(&str, &[u8])] = &[
        ("inter", include_bytes!("../assets/fonts/Inter-Regular.ttf")),
        (
            "inter-bold",
            include_bytes!("../assets/fonts/Inter-Bold.ttf"),
        ),
        (
            "inter-italic",
            include_bytes!("../assets/fonts/Inter-Italic.ttf"),
        ),
        (
            "inter-bold-italic",
            include_bytes!("../assets/fonts/Inter-BoldItalic.ttf"),
        ),
        (
            "jbmono",
            include_bytes!("../assets/fonts/JetBrainsMono-Regular.ttf"),
        ),
    ];
    for (name, bytes) in fonts {
        d.font_data
            .insert((*name).into(), Arc::new(FontData::from_static(bytes)));
        d.families
            .insert(FontFamily::Name((*name).into()), vec![(*name).into()]);
    }
    d.families
        .get_mut(&FontFamily::Proportional)
        .unwrap()
        .insert(0, "inter".into());
    d.families
        .get_mut(&FontFamily::Monospace)
        .unwrap()
        .insert(0, "jbmono".into());
    d
}

pub fn install_fonts(ctx: &egui::Context) {
    ctx.set_fonts(font_definitions());
}

pub struct Theme {
    pub dark: bool,
    pub desk_bg: Color32,
    pub card_paper_cream: Color32,
    pub card_plain_bg: Color32,
    pub card_border: Color32,
    pub card_shadow: Color32,
    pub text: Color32,
    pub text_weak: Color32,
    pub accent: Color32,
    pub tag_pill_bg: Color32,
    pub code_bg: Color32,
    pub rule_red: Color32,
    pub rule_blue: Color32,
    pub rule_ink: Color32,
    pub focus_ring: Color32,
    pub error_text: Color32,
    pub divider_tab_bg: Color32,
    pub footer_bg: Color32,
    /// Rail / status line / menu bar background — the app chrome around the felt.
    pub chrome_bg: Color32,
    /// Rail row hover wash.
    pub rail_hover_bg: Color32,
    /// Rail active-row fill (paired with an accent edge bar).
    pub rail_active_bg: Color32,
    /// Quiet hairline for separators between chrome groups (non-text).
    pub separator: Color32,
    /// Soft/ambient card shadow layer (near layer is `card_shadow`).
    pub card_shadow_soft: Color32,
}

impl Theme {
    pub fn light() -> Theme {
        Theme {
            dark: false,
            // Warmer desk felt (was 0xE8,0xE4,0xDC — cooler, flatter beige).
            desk_bg: Color32::from_rgb(0xE4, 0xDD, 0xCE),
            card_paper_cream: Color32::from_rgb(0xFB, 0xF7, 0xEB),
            card_plain_bg: Color32::from_rgb(0xFF, 0xFF, 0xFF),
            // Darkened from 0x8A,0x84,0x78 to pass card-border UI pair (>= 3.0 on desk_bg)
            card_border: Color32::from_rgb(0x7C, 0x76, 0x6A),
            // Near/contact shadow layer — tight and darker.
            card_shadow: Color32::from_black_alpha(30),
            text: Color32::from_rgb(0x26, 0x24, 0x20),
            // Darkened from 0x6B,0x66,0x5C so weak text clears 4.5 on the
            // warmer desk felt and rail hover washes too.
            text_weak: Color32::from_rgb(0x63, 0x5D, 0x52),
            accent: Color32::from_rgb(0x1A, 0x56, 0xA0),
            tag_pill_bg: Color32::from_rgb(0xE4, 0xEC, 0xF6),
            code_bg: Color32::from_rgb(0xEF, 0xEA, 0xDD),
            rule_red: Color32::from_rgb(0xD9, 0x8A, 0x8A),
            rule_blue: Color32::from_rgb(0xB9, 0xC8, 0xDD),
            rule_ink: Color32::from_rgb(0x4A, 0x52, 0x60),
            focus_ring: Color32::from_rgb(0x1A, 0x56, 0xA0),
            error_text: Color32::from_rgb(0x9E, 0x2A, 0x2A),
            divider_tab_bg: Color32::from_rgb(0xEA, 0xDF, 0xC8),
            footer_bg: Color32::from_rgb(0xF1, 0xEA, 0xD8),
            chrome_bg: Color32::from_rgb(0xF2, 0xEE, 0xE3),
            rail_hover_bg: Color32::from_rgb(0xE7, 0xE1, 0xD2),
            rail_active_bg: Color32::from_rgb(0xDC, 0xE4, 0xEF),
            separator: Color32::from_rgb(0xD8, 0xD1, 0xC2),
            card_shadow_soft: Color32::from_black_alpha(14),
        }
    }

    pub fn dark() -> Theme {
        Theme {
            dark: true,
            // Warmer near-black felt (was 0x1E,0x1F,0x22 — cool blue-grey).
            desk_bg: Color32::from_rgb(0x23, 0x22, 0x20),
            card_paper_cream: Color32::from_rgb(0x2A, 0x2C, 0x31),
            card_plain_bg: Color32::from_rgb(0x26, 0x28, 0x2C),
            card_border: Color32::from_rgb(0x8E, 0x93, 0x9E),
            card_shadow: Color32::from_black_alpha(110),
            text: Color32::from_rgb(0xE8, 0xE6, 0xE1),
            text_weak: Color32::from_rgb(0xA6, 0xA4, 0x9C),
            accent: Color32::from_rgb(0x7F, 0xB3, 0xF0),
            tag_pill_bg: Color32::from_rgb(0x22, 0x33, 0x48),
            code_bg: Color32::from_rgb(0x1A, 0x1B, 0x1E),
            rule_red: Color32::from_rgb(0x6E, 0x4A, 0x4A),
            rule_blue: Color32::from_rgb(0x3E, 0x4A, 0x5C),
            rule_ink: Color32::from_rgb(0x55, 0x5E, 0x6E),
            focus_ring: Color32::from_rgb(0x7F, 0xB3, 0xF0),
            error_text: Color32::from_rgb(0xF0, 0x9A, 0x9A),
            divider_tab_bg: Color32::from_rgb(0x37, 0x33, 0x28),
            footer_bg: Color32::from_rgb(0x30, 0x2E, 0x28),
            chrome_bg: Color32::from_rgb(0x2A, 0x28, 0x25),
            rail_hover_bg: Color32::from_rgb(0x33, 0x31, 0x2C),
            rail_active_bg: Color32::from_rgb(0x2B, 0x3A, 0x4E),
            separator: Color32::from_rgb(0x3D, 0x3A, 0x34),
            card_shadow_soft: Color32::from_black_alpha(60),
        }
    }
}

/// THE span→format mapping (editor + card faces both use this).
pub fn text_format(style: SpanStyle, th: &Theme) -> TextFormat {
    let prop = |size: f32| FontId::new(size, FontFamily::Name("inter".into()));
    let named = |fam: &str, size: f32| FontId::new(size, FontFamily::Name(fam.into()));
    let mono = || FontId::new(MONO_SIZE, FontFamily::Name("jbmono".into()));
    let mut f = TextFormat::simple(prop(BODY_SIZE), th.text);
    match style {
        SpanStyle::Text => {}
        SpanStyle::Heading(n) => f.font_id = named("inter-bold", heading_size(n)),
        SpanStyle::HeadingMarker => {
            // NOTE: HeadingMarker carries no level; level is derived by layout_body
            // from the line content. This default is overridden by layout_body.
            f.font_id = named("inter-bold", heading_size(1));
            f.color = th.text_weak;
        }
        SpanStyle::Bold => f.font_id = named("inter-bold", BODY_SIZE),
        SpanStyle::Italic => f.font_id = named("inter-italic", BODY_SIZE),
        SpanStyle::BoldItalic => f.font_id = named("inter-bold-italic", BODY_SIZE),
        SpanStyle::Strike => f.strikethrough = egui::Stroke::new(1.0, th.text),
        SpanStyle::InlineCode | SpanStyle::CodeBlock => {
            f.font_id = mono();
            f.background = th.code_bg;
        }
        SpanStyle::CodeFenceMarker => {
            f.font_id = mono();
            f.color = th.text_weak;
        }
        SpanStyle::ListMarker => f.color = th.text_weak,
        // Face-side ☐/☑ glyph substitution is done in card/mod.rs (face_body_with_checkbox_glyphs).
        // The glyph is styled as TaskBoxUnchecked/Checked here, keeping it visually distinct.
        // The editor keeps the raw "- [ ]" / "- [x]" source unchanged.
        SpanStyle::TaskBoxUnchecked | SpanStyle::TaskBoxChecked => f.color = th.text_weak,
        SpanStyle::QuoteMarker => f.color = th.text_weak,
        SpanStyle::Quote => f.font_id = named("inter-italic", BODY_SIZE),
        SpanStyle::WikiLink { resolved: true } => {
            f.color = th.accent;
            f.underline = egui::Stroke::new(1.0, th.accent);
        }
        SpanStyle::WikiLink { resolved: false } => {
            // egui has no dashed underline: unresolved = weak color + underline (§6.13).
            f.color = th.text_weak;
            f.underline = egui::Stroke::new(1.0, th.text_weak);
        }
        SpanStyle::Tag => {
            f.color = th.accent;
            f.background = th.tag_pill_bg;
        }
        SpanStyle::Url | SpanStyle::MdLinkUrl => {
            f.color = th.accent;
            f.underline = egui::Stroke::new(1.0, th.accent);
        }
        SpanStyle::MdLinkText => f.color = th.accent,
    }
    f
}

#[cfg(test)]
mod tests {
    use super::*;
    use eframe::egui::Color32;

    fn lum(c: Color32) -> f64 {
        fn chan(u: u8) -> f64 {
            let s = u as f64 / 255.0;
            if s <= 0.04045 {
                s / 12.92
            } else {
                ((s + 0.055) / 1.055).powf(2.4)
            }
        }
        0.2126 * chan(c.r()) + 0.7152 * chan(c.g()) + 0.0722 * chan(c.b())
    }
    fn contrast(a: Color32, b: Color32) -> f64 {
        let (l1, l2) = (lum(a).max(lum(b)), lum(a).min(lum(b)));
        (l1 + 0.05) / (l2 + 0.05)
    }

    #[test]
    fn wcag_aa_for_every_used_pair() {
        for theme in [Theme::light(), Theme::dark()] {
            let text_pairs: &[(&str, Color32, Color32)] = &[
                ("body on paper", theme.text, theme.card_paper_cream),
                ("body on plain", theme.text, theme.card_plain_bg),
                ("weak on paper", theme.text_weak, theme.card_paper_cream),
                ("weak on plain", theme.text_weak, theme.card_plain_bg),
                ("accent on paper", theme.accent, theme.card_paper_cream),
                ("accent on plain", theme.accent, theme.card_plain_bg),
                ("accent on tag pill", theme.accent, theme.tag_pill_bg),
                ("code on code bg", theme.text, theme.code_bg),
                ("text on desk (status)", theme.text, theme.desk_bg),
                ("error on desk", theme.error_text, theme.desk_bg),
                ("title on divider tab", theme.text, theme.divider_tab_bg),
                ("source on footer", theme.text_weak, theme.footer_bg),
                // WP5x Task 3 chrome pairs.
                (
                    "weak on desk (empty states)",
                    theme.text_weak,
                    theme.desk_bg,
                ),
                ("text on chrome", theme.text, theme.chrome_bg),
                ("weak on chrome", theme.text_weak, theme.chrome_bg),
                ("accent on chrome", theme.accent, theme.chrome_bg),
                ("error on chrome", theme.error_text, theme.chrome_bg),
                ("text on rail hover", theme.text, theme.rail_hover_bg),
                ("weak on rail hover", theme.text_weak, theme.rail_hover_bg),
                ("text on rail active", theme.text, theme.rail_active_bg),
                ("weak on rail active", theme.text_weak, theme.rail_active_bg),
            ];
            for (what, fg, bg) in text_pairs {
                assert!(
                    contrast(*fg, *bg) >= 4.5,
                    "{} ({:?}): {:.2} < 4.5 [dark={}]",
                    what,
                    (fg, bg),
                    contrast(*fg, *bg),
                    theme.dark
                );
            }
            let ui_pairs: &[(&str, Color32, Color32)] = &[
                ("card border on desk", theme.card_border, theme.desk_bg),
                ("focus ring on desk", theme.focus_ring, theme.desk_bg),
                (
                    "focus ring on paper",
                    theme.focus_ring,
                    theme.card_paper_cream,
                ),
            ];
            for (what, fg, bg) in ui_pairs {
                assert!(
                    contrast(*fg, *bg) >= 3.0,
                    "{} : {:.2} < 3.0 [dark={}]",
                    what,
                    contrast(*fg, *bg),
                    theme.dark
                );
            }
            assert!(
                contrast(theme.rule_blue, theme.card_paper_cream) < 4.5,
                "rules stay quiet"
            );
        }
    }

    #[test]
    fn fonts_are_bundled_and_parse() {
        let defs = font_definitions();
        for fam in [
            "inter",
            "inter-bold",
            "inter-italic",
            "inter-bold-italic",
            "jbmono",
        ] {
            assert!(
                defs.families
                    .contains_key(&eframe::egui::FontFamily::Name(fam.into())),
                "missing family {fam}"
            );
        }
    }
}
