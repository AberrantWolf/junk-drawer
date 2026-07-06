//! Randomized round-trip: 1000 adversarial documents, fixed seed (spec §13).

use jd_core::doc::NoteDoc;
use jd_core::rng::Xorshift128;

const KNOWN_KEYS: &[&str] = &[
    "id", "created", "modified", "status", "kind", "source", "tags",
];
const UNKNOWN_KEYS: &[&str] = &["aliases", "x-custom", "publish", "weird_key", "UPPER"];
const VALUES: &[&str] = &[
    "plain",
    "\"double quoted\"",
    "'single'",
    "[a, b, c]",
    "",
    "  spaced  ",
    "01J8ZQ4KF3T9M2X7C5VBNAE8RD",
    "2026-07-03T10:22:00Z",
    "fleeting",
    "with: colon",
];
const BODY_FRAGMENTS: &[&str] = &[
    "# Heading\n",
    "## Sub\n",
    "plain text line\n",
    "",
    "\n",
    "[[Link]]\n",
    "[[Link|display]]\n",
    "[[unclosed\n",
    "#tag mid #tag-two\n",
    "```\ncode [[x]] #y\n```\n",
    "`inline [[z]]`\n",
    "- list item\n",
    "1. numbered\n",
    "> quote\n",
    "**bold** *it* ~~strike~~\n",
    "日本語テキスト 🎉\n",
    "نص عربي\n",
    "| a | b |\n",
    "trailing spaces   \n",
    "\t\ttabs\n",
    "--- \n",
    "----\n",
    "---x\n",
];

fn pick<'a>(rng: &mut Xorshift128, pool: &[&'a str]) -> &'a str {
    pool[rng.gen_range(0..pool.len() as u64) as usize]
}

fn gen_document(rng: &mut Xorshift128) -> String {
    let mut out = String::new();
    let crlf = rng.gen_range(0..4) == 0;
    let term = if crlf { "\r\n" } else { "\n" };
    if rng.gen_range(0..5) > 0 {
        // 80%: with frontmatter
        out.push_str("---");
        out.push_str(term);
        for _ in 0..rng.gen_range(0..8) {
            let key = if rng.gen_range(0..2) == 0 {
                pick(rng, KNOWN_KEYS)
            } else {
                pick(rng, UNKNOWN_KEYS)
            };
            out.push_str(key);
            out.push_str(": ");
            out.push_str(pick(rng, VALUES));
            out.push_str(term);
            if rng.gen_range(0..6) == 0 {
                out.push_str("  - block item");
                out.push_str(term);
            }
        }
        if rng.gen_range(0..10) > 0 {
            // 10%: leave the block unterminated
            out.push_str("---");
            out.push_str(term);
        }
    }
    for _ in 0..rng.gen_range(0..20) {
        // body fragments use \n even in crlf mode — mixed line endings are a
        // deliberately adversarial case and must still round-trip
        out.push_str(pick(rng, BODY_FRAGMENTS));
    }
    out
}

#[test]
fn randomized_documents_round_trip() {
    let mut rng = Xorshift128::new(0x_5EED_CAFE);
    for i in 0..1000 {
        let doc_src = gen_document(&mut rng);
        let doc = NoteDoc::parse(&doc_src);
        assert_eq!(
            doc.serialize(),
            doc_src,
            "round-trip failed on iteration {i}; input: {doc_src:?}"
        );
    }
}

#[test]
fn randomized_metadata_extraction_never_panics() {
    let mut rng = Xorshift128::new(0x00DD_BA11);
    let id = jd_core::id::NoteId([1; 16]);
    let t = jd_core::time::Timestamp(0);
    for _ in 0..1000 {
        let doc_src = gen_document(&mut rng);
        let doc = NoteDoc::parse(&doc_src);
        let _ = doc.to_meta(id, std::path::Path::new("notes/x.md"), t);
    }
}
