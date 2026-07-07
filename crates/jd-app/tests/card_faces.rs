mod common;

use eframe::egui;
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use jd_app::card::shape::{CardShape, CardStyle, RuledLines};
use jd_app::card::{CardFace, card_face};
use jd_app::theme::Theme;

fn nid(n: u8) -> jd_core::id::NoteId {
    let s = format!("01ARZ3NDEKTSV4RRFFQ69G5F{n:02}");
    jd_core::id::NoteId::parse(&s).unwrap_or_else(|_| panic!("bad test ulid {s}"))
}

/// Owned mirror of CardFace so the closure can be 'static.
struct OwnedFace {
    id: jd_core::id::NoteId,
    title: String,
    body: Option<String>,
    source: Option<String>,
    shape: CardShape,
    style: CardStyle,
    lines: RuledLines,
    links: usize,
    tags: usize,
    focused: bool,
    dark: bool,
}

impl OwnedFace {
    fn borrow(&self) -> CardFace<'_> {
        CardFace {
            id: self.id,
            title: &self.title,
            body: self.body.as_deref(),
            source: self.source.as_deref(),
            shape: self.shape,
            style: self.style,
            lines: self.lines,
            links: self.links,
            tags: self.tags,
            focused: self.focused,
        }
    }

    fn sample(shape: CardShape, style: CardStyle, lines: RuledLines, dark: bool) -> Self {
        let source = if shape == CardShape::Literature {
            Some("Ahrens 2017".to_string())
        } else {
            None
        };
        OwnedFace {
            id: nid(1),
            title: "Ideas want linking".to_string(),
            body: Some(
                "# Ideas want linking\nBody with **bold**, a [[Link]], and #tag\n- [ ] a task"
                    .to_string(),
            ),
            source,
            shape,
            style,
            lines,
            links: 3,
            tags: 2,
            focused: false,
            dark,
        }
    }
}

/// eframe App wrapper for face rendering — installs fonts before first frame
/// via CreationContext so the font family is available during layout.
struct FaceApp {
    face: OwnedFace,
    cache: jd_app::editor::LineCache,
}

impl eframe::App for FaceApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let th = if self.face.dark {
            Theme::dark()
        } else {
            Theme::light()
        };
        let rect = egui::Rect::from_min_size(
            egui::pos2(24.0, 24.0),
            jd_app::card::shape::card_size(self.face.shape),
        );
        card_face(ui, rect, &self.face.borrow(), &th, &mut self.cache);
    }
}

fn face_harness(face_owned: OwnedFace) -> Harness<'static, FaceApp> {
    Harness::builder()
        .with_size(egui::vec2(360.0, 280.0))
        .build_eframe(move |cc| {
            jd_app::theme::install_fonts(&cc.egui_ctx);
            FaceApp {
                face: face_owned,
                cache: jd_app::editor::LineCache::default(),
            }
        })
}

/// The full legal matrix (architecture WP2: 4 shapes × 2 styles + 3 line
/// variants on the Paper index card).
#[test]
fn snapshot_card_face_matrix() {
    use egui_kittest::SnapshotResults;

    let combos: Vec<(&str, CardShape, CardStyle, RuledLines)> = vec![
        (
            "scrap_paper",
            CardShape::Scrap,
            CardStyle::Paper,
            RuledLines::None,
        ),
        (
            "scrap_plain",
            CardShape::Scrap,
            CardStyle::Plain,
            RuledLines::None,
        ),
        (
            "index_paper_none",
            CardShape::IndexCard,
            CardStyle::Paper,
            RuledLines::None,
        ),
        (
            "index_paper_natural",
            CardShape::IndexCard,
            CardStyle::Paper,
            RuledLines::Natural,
        ),
        (
            "index_paper_ink",
            CardShape::IndexCard,
            CardStyle::Paper,
            RuledLines::Ink,
        ),
        (
            "index_plain",
            CardShape::IndexCard,
            CardStyle::Plain,
            RuledLines::None,
        ),
        (
            "literature_paper",
            CardShape::Literature,
            CardStyle::Paper,
            RuledLines::Natural,
        ),
        (
            "literature_plain",
            CardShape::Literature,
            CardStyle::Plain,
            RuledLines::None,
        ),
        (
            "divider_paper",
            CardShape::Divider,
            CardStyle::Paper,
            RuledLines::None,
        ),
        (
            "divider_plain",
            CardShape::Divider,
            CardStyle::Plain,
            RuledLines::None,
        ),
        (
            "index_dark_ink",
            CardShape::IndexCard,
            CardStyle::Paper,
            RuledLines::Ink,
        ), // Theme::dark
    ];
    let mut all_results = SnapshotResults::new();
    for (name, shape, style, lines) in combos {
        let mut h = face_harness(OwnedFace::sample(
            shape,
            style,
            lines,
            name.contains("dark"),
        ));
        h.run_ok();
        h.snapshot(format!("card_{name}"));
        all_results.extend_harness(&mut h);
    }
    all_results.unwrap();
}

#[test]
fn face_carries_the_spec_announcement() {
    let mut h = face_harness(OwnedFace::sample(
        CardShape::IndexCard,
        CardStyle::Paper,
        RuledLines::Natural,
        false,
    ));
    h.run_ok();
    h.get_by_label_contains("Card: 'Ideas want linking'");
}

#[test]
fn blank_face_while_body_loads_is_not_an_error() {
    let mut of = OwnedFace::sample(
        CardShape::IndexCard,
        CardStyle::Plain,
        RuledLines::None,
        false,
    );
    of.body = None;
    let mut h = face_harness(of);
    h.run_ok();
    h.get_by_label_contains("Card: '"); // node exists even with no body
}
