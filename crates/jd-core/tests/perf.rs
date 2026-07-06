//! Performance budgets (spec §13) — the tripwire that legally activates the
//! §3 snapshot escape hatch. Release-mode only: debug runs skip via ignore.

mod common;

use std::time::Instant;

use common::TempDir;
use jd_core::index::Index;
use jd_core::index::search::parse_query;
use jd_core::rng::Xorshift128;
use jd_core::vault::Vault;
use jd_core::vault::scan::{parse_note_file, scan};

const NOTES: usize = 20_000;

const WORDS: &[&str] = &[
    "zettelkasten",
    "method",
    "note",
    "thought",
    "link",
    "idea",
    "permanent",
    "fleeting",
    "structure",
    "argument",
    "claim",
    "evidence",
    "writing",
    "reading",
    "memory",
    "system",
    "practice",
    "review",
    "connect",
    "emerge",
    "context",
    "question",
    "answer",
    "draft",
];

fn build_synthetic_vault() -> (TempDir, Vault) {
    let t = TempDir::new();
    let v = Vault::open(t.path()).unwrap();
    let mut rng = Xorshift128::new(0x20_000);
    for i in 0..NOTES {
        let mut body = format!("# Note {i}\n\n");
        for _ in 0..80 {
            body.push_str(WORDS[rng.gen_range(0..WORDS.len() as u64) as usize]);
            body.push(' ');
        }
        body.push_str(&format!(
            "\n\nSee [[Note {}]] and [[Note {}]].\n#tag{} #shared\n",
            rng.gen_range(0..NOTES as u64),
            rng.gen_range(0..NOTES as u64),
            i % 50
        ));
        std::fs::write(t.path().join(format!("notes/Note {i}.md")), body).unwrap();
    }
    (t, v)
}

fn build_index(v: &Vault) -> Index {
    let out = scan(v, &|_, _| {}).unwrap();
    let mut ix = Index::new();
    for (meta, body) in out.metas {
        ix.upsert(meta, &body);
    }
    ix
}

#[test]
#[cfg_attr(debug_assertions, ignore = "perf budgets are release-mode only")]
fn cold_scan_under_one_second() {
    let (_t, v) = build_synthetic_vault();
    let start = Instant::now();
    let out = scan(&v, &|_, _| {}).unwrap();
    let elapsed = start.elapsed();
    assert_eq!(out.metas.len(), NOTES);
    assert!(
        elapsed.as_millis() < 1000,
        "cold scan took {elapsed:?} (budget 1s)"
    );
}

#[test]
#[cfg_attr(debug_assertions, ignore = "perf budgets are release-mode only")]
fn incremental_reindex_under_five_ms() {
    let (_t, v) = build_synthetic_vault();
    let mut ix = build_index(&v);
    let rel = std::path::Path::new("notes/Note 100.md");
    std::fs::write(v.root().join(rel), "# Note 100\nedited body #shared\n").unwrap();
    let start = Instant::now();
    let (meta, body) = parse_note_file(&v, rel).unwrap();
    ix.upsert(meta, &body);
    let elapsed = start.elapsed();
    assert!(
        elapsed.as_micros() < 5000,
        "reindex took {elapsed:?} (budget 5ms)"
    );
}

#[test]
#[cfg_attr(debug_assertions, ignore = "perf budgets are release-mode only")]
fn queries_under_ten_ms() {
    let (_t, v) = build_synthetic_vault();
    let ix = build_index(&v);
    for q in [
        "zettelkasten method",
        "\"permanent note\"",
        "writing -draft",
        "argu",
    ] {
        let start = Instant::now();
        let _ = ix.query(&parse_query(q), 20);
        let elapsed = start.elapsed();
        assert!(
            elapsed.as_micros() < 10_000,
            "query {q:?} took {elapsed:?} (budget 10ms)"
        );
    }
    // WP1c-flagged hot spots under the same budget:
    let start = Instant::now();
    let _ = ix.query(&parse_query("#shared zettelkasten"), 20); // ~20k-member tag filter
    assert!(
        start.elapsed().as_micros() < 10_000,
        "large-tag query blew the budget"
    );

    let any_id = ix.iter_meta().next().unwrap().id;
    let start = Instant::now();
    let _ = ix.similar(any_id, 8);
    assert!(
        start.elapsed().as_millis() < 50,
        "similar() took {:?} (soft 50ms bound)",
        start.elapsed()
    );
}
