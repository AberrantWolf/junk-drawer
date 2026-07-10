//! Geometric card visual language: shapes, sizes, torn-edge outlines, ruled lines.
//! Pure geometry — no rendering. Task 7 draws the results.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use eframe::egui;
use jd_core::id::NoteId;
use jd_core::note::{Kind, Status};
use jd_core::rng::Xorshift128;

use crate::theme::{RED_RULE_Y, RULE_SPACING, RULE_TOP_OFFSET, Theme};

// ---------------------------------------------------------------------------
// Card classification
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CardShape {
    Scrap,
    IndexCard,
    Literature,
    Divider,
}

/// Map (Status, Kind) → CardShape.
/// Fleeting → Scrap regardless of kind.
/// Permanent: Literature → Literature, Structure → Divider, Note → IndexCard.
pub fn shape_for(status: Status, kind: Kind) -> CardShape {
    match status {
        Status::Fleeting => CardShape::Scrap,
        Status::Permanent => match kind {
            Kind::Literature => CardShape::Literature,
            Kind::Structure => CardShape::Divider,
            Kind::Note => CardShape::IndexCard,
        },
    }
}

// ---------------------------------------------------------------------------
// Card sizes
// ---------------------------------------------------------------------------

/// Nominal card body dimensions (not including any protruding tab).
pub fn card_size(shape: CardShape) -> egui::Vec2 {
    match shape {
        CardShape::Scrap => egui::vec2(240.0, 88.0),
        CardShape::IndexCard => egui::vec2(300.0, 200.0),
        CardShape::Literature => egui::vec2(300.0, 224.0),
        CardShape::Divider => egui::vec2(300.0, 208.0),
    }
}

/// Width × height of the divider's protruding tab (sits above the body top edge).
pub const DIVIDER_TAB: egui::Vec2 = egui::vec2(96.0, 26.0);

/// Height of the literature footer band.
pub const FOOTER_H: f32 = 24.0;

// ---------------------------------------------------------------------------
// Style / ruled-line enums
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CardStyle {
    Paper,
    Plain,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum RuledLines {
    None,
    Natural,
    Ink,
}

// ---------------------------------------------------------------------------
// Card outline geometry
// ---------------------------------------------------------------------------

/// Seed a u64 from a NoteId by hashing its display string bytes.
fn seed_from_id(id: NoteId) -> u64 {
    let mut h = DefaultHasher::new();
    id.to_string().as_bytes().hash(&mut h);
    h.finish()
}

/// Return the card outline polygon vertices.
///
/// - `Plain` style: a rounded-rect polygon (≤ 8 vertices, 4 corners each = 1 point).
/// - `Paper` style for `Scrap`: top and bottom edges have a torn jitter seeded
///   from `id`; sides stay straight.
/// - `Paper` style for other shapes: a rounded polygon with the appropriate silhouette
///   (Divider includes the tab notch; Literature and IndexCard are plain rounded rects
///   for their outline — Tab and footer are decorations drawn on top).
///
/// All points are guaranteed to stay within `rect.expand(0.1)` for `Scrap`,
/// `IndexCard`, and `Literature`. For `Divider`, the tab vertices protrude
/// `DIVIDER_TAB.y` above `rect.min.y` by design — use [`divider_full_rect`] to
/// obtain the bounding box that includes the tab.
pub fn outline(
    shape: CardShape,
    style: CardStyle,
    rect: egui::Rect,
    id: NoteId,
) -> Vec<egui::Pos2> {
    match style {
        CardStyle::Plain => plain_rounded_rect(rect),
        CardStyle::Paper => match shape {
            CardShape::Scrap => scrap_torn_outline(rect, id),
            CardShape::Divider => divider_outline(rect),
            _ => plain_rounded_rect(rect),
        },
    }
}

/// Plain rounded rect — 4 corner points (one per corner, no arc subdivision).
/// Returns exactly 4 vertices (≤ 8 as required).
fn plain_rounded_rect(rect: egui::Rect) -> Vec<egui::Pos2> {
    let r = crate::theme::CARD_CORNER_RADIUS
        .min(rect.width() * 0.05)
        .min(rect.height() * 0.05);
    vec![
        egui::pos2(rect.min.x + r, rect.min.y),
        egui::pos2(rect.max.x - r, rect.min.y),
        egui::pos2(rect.max.x, rect.min.y + r),
        egui::pos2(rect.max.x, rect.max.y - r),
        egui::pos2(rect.max.x - r, rect.max.y),
        egui::pos2(rect.min.x + r, rect.max.y),
        egui::pos2(rect.min.x, rect.max.y - r),
        egui::pos2(rect.min.x, rect.min.y + r),
    ]
}

/// Scrap torn-edge outline: irregular top AND bottom edges seeded from `id`
/// (a strip torn from a sheet), straight sides.
///
/// Walk each torn edge in ~14px steps, jittering y by ±3px around a baseline
/// inset 3px from that edge.  Top: baseline `rect.min.y + 3.0`, tear points in
/// `[rect.min.y, rect.min.y + 6]`.  Bottom: baseline `rect.max.y - 3.0`, tear
/// points in `[rect.max.y - 6, rect.max.y]`.  Both edges are genuinely
/// two-sided without any clamping, and both draw from the same seeded stream
/// so the whole outline is deterministic per id.  The left/right edges are
/// straight lines.
fn scrap_torn_outline(rect: egui::Rect, id: NoteId) -> Vec<egui::Pos2> {
    let seed = seed_from_id(id);
    let mut rng = Xorshift128::new(seed);

    let step = 14.0_f32;
    let jitter = 3.0_f32;
    let width = rect.width();
    let steps = ((width / step).ceil() as usize).max(2);

    // Map the next u64 from the stream to [-jitter, +jitter].
    let next_dy = |rng: &mut Xorshift128| {
        let raw = rng.next_u64();
        // Scale to [0.0, 1.0] then to [-jitter, +jitter]
        let frac = (raw & 0xFFFF) as f32 / 65535.0;
        frac * 2.0 * jitter - jitter
    };

    let mut pts: Vec<egui::Pos2> = Vec::with_capacity(2 * steps + 10);

    // Top-left start
    pts.push(egui::pos2(rect.min.x, rect.min.y));

    // Torn top edge: walk left → right around a baseline inset 3px below.
    let top_baseline = rect.min.y + jitter;
    let mut x = rect.min.x + step;
    for _ in 0..steps {
        if x >= rect.max.x {
            break;
        }
        // No clamp needed: baseline ± jitter = [rect.min.y, rect.min.y + 6]
        let y = top_baseline + next_dy(&mut rng);
        pts.push(egui::pos2(x.min(rect.max.x), y));
        x += step;
    }

    // Top-right, then down the straight right side to bottom-right.
    pts.push(egui::pos2(rect.max.x, rect.min.y));
    pts.push(egui::pos2(rect.max.x, rect.max.y));

    // Torn bottom edge: walk right → left (polygon winding) around a baseline
    // inset 3px above, drawing from the same seeded stream.
    let bottom_baseline = rect.max.y - jitter;
    let mut x = rect.max.x - step;
    for _ in 0..steps {
        if x <= rect.min.x {
            break;
        }
        // No clamp needed: baseline ± jitter = [rect.max.y - 6, rect.max.y]
        let y = bottom_baseline + next_dy(&mut rng);
        pts.push(egui::pos2(x.max(rect.min.x), y));
        x -= step;
    }

    // Bottom-left
    pts.push(egui::pos2(rect.min.x, rect.max.y));

    pts
}

/// Return the full bounding box of a Divider card, including the protruding tab.
///
/// `body_rect` is the rect passed to [`outline`] for a `Divider` card.  The
/// returned rect extends `DIVIDER_TAB.y` above `body_rect.min.y` and is wide
/// enough to enclose both the body and the tab.  Task 7 should use this instead
/// of computing the tab offset manually.
pub fn divider_full_rect(body_rect: egui::Rect) -> egui::Rect {
    egui::Rect::from_min_max(
        egui::pos2(body_rect.min.x, body_rect.min.y - DIVIDER_TAB.y),
        body_rect.max,
    )
}

/// Divider outline: body rect plus a tab protruding above the top-left corner.
/// The tab is `DIVIDER_TAB` wide and tall, attached at the left of the top edge.
fn divider_outline(rect: egui::Rect) -> Vec<egui::Pos2> {
    let tw = DIVIDER_TAB.x.min(rect.width());
    let th = DIVIDER_TAB.y;
    vec![
        // Tab: top-left → top-right of tab, down to body top
        egui::pos2(rect.min.x, rect.min.y - th),
        egui::pos2(rect.min.x + tw, rect.min.y - th),
        egui::pos2(rect.min.x + tw, rect.min.y),
        // Body: right across top, down right side, across bottom, up left side
        egui::pos2(rect.max.x, rect.min.y),
        egui::pos2(rect.max.x, rect.max.y),
        egui::pos2(rect.min.x, rect.max.y),
    ]
}

// ---------------------------------------------------------------------------
// Ruled lines
// ---------------------------------------------------------------------------

/// Return (y_position, color) pairs for ruled lines across a card face.
///
/// Positions are in card-local space (0 = top of content area).
/// - `None` → empty.
/// - `Natural` → one red header rule at `RED_RULE_Y` (below the title line),
///   then blue rules every `RULE_SPACING` starting at `RULE_TOP_OFFSET`.
/// - `Ink` → ink-color rules every `RULE_SPACING` starting at `RULE_TOP_OFFSET`,
///   no red header.
pub fn rules(lines: RuledLines, height: f32, th: &Theme) -> Vec<(f32, egui::Color32)> {
    match lines {
        RuledLines::None => vec![],
        RuledLines::Natural => {
            let mut result = Vec::new();
            // Red header rule, below the first heading (title) line
            result.push((RED_RULE_Y, th.rule_red));
            // Blue body rules
            let mut y = RULE_TOP_OFFSET;
            while y < height {
                result.push((y, th.rule_blue));
                y += RULE_SPACING;
            }
            result
        }
        RuledLines::Ink => {
            let mut result = Vec::new();
            let mut y = RULE_TOP_OFFSET;
            while y < height {
                result.push((y, th.rule_ink));
                y += RULE_SPACING;
            }
            result
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use eframe::egui;

    fn nid(n: u8) -> jd_core::id::NoteId {
        let s = format!("01ARZ3NDEKTSV4RRFFQ69G5F{n:02}");
        jd_core::id::NoteId::parse(&s).unwrap_or_else(|_| panic!("bad test ulid {s}"))
    }

    #[test]
    fn scrap_is_wider_than_tall_and_card_is_3x5() {
        let s = card_size(CardShape::Scrap);
        assert_eq!(s, egui::vec2(240.0, 88.0), "scrap is a low torn strip");
        assert!(s.x / s.y > 1.5, "scrap reads as a torn strip");
        let c = card_size(CardShape::IndexCard);
        assert!((c.x / c.y - 1.5).abs() < 0.01, "3x5 proportions");
    }

    #[test]
    fn torn_edge_is_deterministic_and_paper_only() {
        let rect = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), card_size(CardShape::Scrap));
        let a = outline(CardShape::Scrap, CardStyle::Paper, rect, nid(1));
        let b = outline(CardShape::Scrap, CardStyle::Paper, rect, nid(1));
        assert_eq!(a, b, "same id, same tear");
        let c = outline(CardShape::Scrap, CardStyle::Paper, rect, nid(2));
        assert_ne!(a, c, "different id, different tear");
        let plain = outline(CardShape::Scrap, CardStyle::Plain, rect, nid(1));
        assert!(plain.len() <= 8, "plain = rounded rect, no tear vertices");
        // Semantic shape survives Plain: still scrap-sized (caller controls rect; the
        // outline never exceeds it).
        for p in &plain {
            assert!(rect.expand(0.1).contains(*p));
        }
        for p in &a {
            assert!(rect.expand(0.1).contains(*p), "tear stays inside the rect");
        }

        // Two-sidedness on BOTH torn edges: each edge's tear points must have
        // at least one point strictly above AND one strictly below that edge's
        // baseline. Top baseline = rect.min.y + 3; bottom baseline =
        // rect.max.y - 3. Use nid(1) which is deterministic.
        let mid_y = rect.center().y;
        let inner: Vec<_> = a
            .iter()
            .filter(|p| p.x > rect.min.x && p.x < rect.max.x)
            .collect();
        let top_pts: Vec<_> = inner.iter().filter(|p| p.y < mid_y).collect();
        let bottom_pts: Vec<_> = inner.iter().filter(|p| p.y > mid_y).collect();
        assert!(!top_pts.is_empty(), "top edge has tear points");
        assert!(!bottom_pts.is_empty(), "bottom edge has tear points");

        let top_baseline = rect.min.y + 3.0;
        assert!(
            top_pts.iter().any(|p| p.y < top_baseline),
            "top tear must have at least one point above its baseline"
        );
        assert!(
            top_pts.iter().any(|p| p.y > top_baseline),
            "top tear must have at least one point below its baseline"
        );

        let bottom_baseline = rect.max.y - 3.0;
        assert!(
            bottom_pts.iter().any(|p| p.y < bottom_baseline),
            "bottom tear must have at least one point above its baseline"
        );
        assert!(
            bottom_pts.iter().any(|p| p.y > bottom_baseline),
            "bottom tear must have at least one point below its baseline"
        );
    }

    #[test]
    fn shape_for_exhaustive() {
        use jd_core::note::{Kind, Status};
        // Fleeting always → Scrap regardless of kind
        assert_eq!(shape_for(Status::Fleeting, Kind::Note), CardShape::Scrap);
        assert_eq!(
            shape_for(Status::Fleeting, Kind::Literature),
            CardShape::Scrap
        );
        assert_eq!(
            shape_for(Status::Fleeting, Kind::Structure),
            CardShape::Scrap
        );
        // Permanent: kind determines shape
        assert_eq!(
            shape_for(Status::Permanent, Kind::Note),
            CardShape::IndexCard
        );
        assert_eq!(
            shape_for(Status::Permanent, Kind::Literature),
            CardShape::Literature
        );
        assert_eq!(
            shape_for(Status::Permanent, Kind::Structure),
            CardShape::Divider
        );
    }

    #[test]
    fn natural_rules_have_red_header_then_blue() {
        use crate::theme::{RED_RULE_Y, RULE_TOP_OFFSET};
        let th = crate::theme::Theme::light();
        let r = rules(RuledLines::Natural, 200.0, &th);
        assert!(r.len() >= 6);
        assert_eq!(r[0].1, th.rule_red);
        // Red rule sits BELOW the title: the first heading line renders at
        // 24px from content_top=10, so its bottom edge is ~y=39.
        assert_eq!(r[0].0, RED_RULE_Y);
        assert!(r[0].0 >= 39.0, "red rule lands below the title line");
        // Blue rules start below the red one.
        assert_eq!(r[1].0, RULE_TOP_OFFSET);
        assert!(r[1].0 > r[0].0, "blue rules start below the red rule");
        assert!(r[1..].iter().all(|(_, c)| *c == th.rule_blue));
        assert!(
            r.windows(2).all(|w| w[1].0 > w[0].0),
            "descending down the card"
        );
        assert!(r.last().unwrap().0 < 200.0);
        assert!(rules(RuledLines::None, 200.0, &th).is_empty());
        let ink = rules(RuledLines::Ink, 200.0, &th);
        assert!(ink.iter().all(|(_, c)| *c == th.rule_ink));
        assert!(
            ink[0].0 >= 39.0,
            "ink rules must not collide with the title line"
        );
    }
}
