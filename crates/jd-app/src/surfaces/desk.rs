//! The desk surface. This file owns spatial focus order (Spike B) and,
//! from Task 8, the pannable canvas itself.

use jd_core::geom::Vec2;
use jd_core::id::NoteId;

/// 0.6 × index-card height (200.0). Rounded y-bands make reading order
/// stable under small drags (architecture §3, spec §12).
pub const BAND_HEIGHT: f32 = 120.0;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FocusDir {
    Left,
    Right,
    Up,
    Down,
}

fn band(y: f32) -> i64 {
    (y / BAND_HEIGHT).round() as i64
}

pub fn reading_order(cards: &[(NoteId, Vec2)]) -> Vec<NoteId> {
    let mut v: Vec<&(NoteId, Vec2)> = cards.iter().collect();
    v.sort_by(|a, b| {
        band(a.1.y)
            .cmp(&band(b.1.y))
            .then(a.1.x.total_cmp(&b.1.x))
            .then(a.0.cmp(&b.0))
    });
    v.into_iter().map(|(id, _)| *id).collect()
}

pub fn next_focus(
    cards: &[(NoteId, Vec2)],
    current: Option<NoteId>,
    dir: FocusDir,
) -> Option<NoteId> {
    if cards.is_empty() {
        return None;
    }
    let order = reading_order(cards);
    let Some(cur) = current else {
        return order.first().copied();
    };
    let Some(idx) = order.iter().position(|id| *id == cur) else {
        return order.first().copied();
    };
    match dir {
        FocusDir::Left => idx.checked_sub(1).map(|i| order[i]),
        FocusDir::Right => order.get(idx + 1).copied(),
        FocusDir::Up | FocusDir::Down => {
            let pos = cards.iter().find(|(id, _)| *id == cur)?.1;
            let cur_band = band(pos.y);
            let step: i64 = if dir == FocusDir::Down { 1 } else { -1 };
            // Search outward band by band for the nearest card by |Δx|.
            let bands: std::collections::BTreeSet<i64> =
                cards.iter().map(|(_, p)| band(p.y)).collect();
            let mut target = cur_band + step;
            let (min_b, max_b) = (*bands.iter().next()?, *bands.iter().last()?);
            while target >= min_b && target <= max_b {
                let mut best: Option<(f32, NoteId)> = None;
                for (id, p) in cards {
                    if band(p.y) == target {
                        let dx = (p.x - pos.x).abs();
                        if best.is_none_or(|(bd, bid)| dx < bd || (dx == bd && *id < bid)) {
                            best = Some((dx, *id));
                        }
                    }
                }
                if let Some((_, id)) = best {
                    return Some(id);
                }
                target += step;
            }
            None
        }
    }
}

pub fn card_a11y_label(
    title: &str,
    first_line: &str,
    is_scrap: bool,
    links: usize,
    tags: usize,
) -> String {
    if is_scrap {
        return format!("Scrap: '{first_line}'");
    }
    let l = if links == 1 { "link" } else { "links" };
    let t = if tags == 1 { "tag" } else { "tags" };
    format!("Card: '{title}', {links} {l}, {tags} {t}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use jd_core::geom::Vec2;
    use jd_core::id::NoteId;

    fn id(n: u8) -> NoteId {
        // NoteId::parse (no FromStr impl) from a 26-char ULID; build distinct ids cheaply.
        let s = format!("01ARZ3NDEKTSV4RRFFQ69G5F{n:02}");
        NoteId::parse(&s).unwrap_or_else(|_| panic!("bad test ulid {s}"))
    }

    #[test]
    fn reading_order_is_bands_then_x() {
        // Band height 120: y=10 and y=50 share band 0; y=200 is band 2.
        let cards = vec![
            (id(1), Vec2 { x: 300.0, y: 10.0 }),
            (id(2), Vec2 { x: 5.0, y: 50.0 }),
            (id(3), Vec2 { x: 0.0, y: 200.0 }),
        ];
        assert_eq!(reading_order(&cards), vec![id(2), id(1), id(3)]);
    }

    #[test]
    fn reading_order_stable_under_small_drags() {
        let before = vec![
            (id(1), Vec2 { x: 0.0, y: 100.0 }),
            (id(2), Vec2 { x: 200.0, y: 130.0 }),
        ];
        // 20px vertical wiggle does not change bands (100→0.83 rounds 1; 120→1).
        let after = vec![
            (id(1), Vec2 { x: 0.0, y: 120.0 }),
            (id(2), Vec2 { x: 200.0, y: 110.0 }),
        ];
        assert_eq!(reading_order(&before), reading_order(&after));
    }

    #[test]
    fn arrows_traverse_and_do_not_wrap() {
        let cards = vec![
            (id(1), Vec2 { x: 0.0, y: 0.0 }),
            (id(2), Vec2 { x: 400.0, y: 0.0 }),
            (id(3), Vec2 { x: 100.0, y: 300.0 }),
        ];
        assert_eq!(
            next_focus(&cards, Some(id(1)), FocusDir::Right),
            Some(id(2))
        );
        assert_eq!(
            next_focus(&cards, Some(id(2)), FocusDir::Right),
            Some(id(3))
        ); // id(3) follows id(2) in reading order
        assert_eq!(next_focus(&cards, Some(id(3)), FocusDir::Right), None); // no wrap at last
        assert_eq!(next_focus(&cards, Some(id(1)), FocusDir::Down), Some(id(3)));
        assert_eq!(next_focus(&cards, Some(id(3)), FocusDir::Up), Some(id(1))); // nearest |Δx|
        assert_eq!(next_focus(&cards, None, FocusDir::Right), Some(id(1))); // no focus → first
    }

    #[test]
    fn a11y_labels_match_spec() {
        assert_eq!(
            card_a11y_label(
                "Immediate mode trades layout power for state simplicity",
                "",
                false,
                3,
                2
            ),
            "Card: 'Immediate mode trades layout power for state simplicity', 3 links, 2 tags"
        );
        assert_eq!(
            card_a11y_label("T", "", false, 1, 0),
            "Card: 'T', 1 link, 0 tags"
        );
        assert_eq!(
            card_a11y_label("", "buy milk", true, 0, 0),
            "Scrap: 'buy milk'"
        );
    }
}
