//! The load-bearing invariant (spec §13): parse → serialize is byte-identical
//! for everything not deliberately changed. Exception: a leading UTF-8 BOM
//! (`EF BB BF`) is dropped on serialize — the one sanctioned normalization.

use jd_core::doc::NoteDoc;

fn golden_dir() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests-data/golden")
}

#[test]
fn golden_corpus_round_trips_byte_identical() {
    let mut checked = 0;
    for entry in std::fs::read_dir(golden_dir()).expect("tests-data/golden exists") {
        let path = entry.unwrap().path();
        if path.extension().is_none_or(|e| e != "md") {
            continue;
        }
        let src = std::fs::read_to_string(&path).unwrap();
        let doc = NoteDoc::parse(&src);
        let expected = src.strip_prefix('\u{feff}').unwrap_or(&src);
        assert_eq!(
            doc.serialize(),
            expected,
            "round-trip failed for {}",
            path.display()
        );
        checked += 1;
    }
    assert!(
        checked >= 14,
        "corpus has {checked} files; expected at least 14"
    );
}

#[test]
fn golden_corpus_extracts_sane_metadata() {
    // spot-checks that parsing (not just round-tripping) works on foreign files
    let src = std::fs::read_to_string(golden_dir().join("02-obsidian.md")).unwrap();
    let doc = NoteDoc::parse(&src);
    let tags: Vec<String> = doc
        .fm
        .tags()
        .iter()
        .map(|t| t.as_str().to_owned())
        .collect();
    assert_eq!(tags, vec!["imported", "from-obsidian"]);

    let src = std::fs::read_to_string(golden_dir().join("09-duplicate-keys.md")).unwrap();
    let doc = NoteDoc::parse(&src);
    assert_eq!(
        doc.fm.status(),
        Some(jd_core::note::Status::Permanent),
        "first key wins"
    );

    // 05-bom.md has a leading UTF-8 BOM; frontmatter behind it must be parsed
    let src = std::fs::read_to_string(golden_dir().join("05-bom.md")).unwrap();
    let doc = NoteDoc::parse(&src);
    assert_eq!(
        doc.fm.status(),
        Some(jd_core::note::Status::Fleeting),
        "BOM-prefixed frontmatter must be visible after normalization"
    );
}
