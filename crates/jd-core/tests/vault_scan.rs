//! Parallel startup scan (spec §3): every note parses or quarantines;
//! the scan itself never fails on file content.

mod common;

use common::TempDir;
use jd_core::vault::Vault;
use jd_core::vault::scan::{scan, synthetic_id};

fn vault_with(notes: &[(&str, &str)]) -> (TempDir, Vault) {
    let t = TempDir::new();
    let v = Vault::open(t.path()).unwrap();
    for (rel, content) in notes {
        let p = t.path().join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, content).unwrap();
    }
    (t, v)
}

#[test]
fn scans_both_dirs_and_extracts_meta() {
    let (_t, v) = vault_with(&[
        ("inbox/scrap.md", "a stray thought\n"),
        (
            "notes/Card.md",
            "---\nid: 01J8ZQ4KF3T9M2X7C5VBNAE8RD\nstatus: permanent\n---\n# Card\nBody [[Link]].\n",
        ),
    ]);
    let out = scan(&v, &|_, _| {}).unwrap();
    assert_eq!(out.metas.len(), 2);
    assert!(out.quarantined.is_empty());
    let card = out
        .metas
        .iter()
        .find(|(m, _)| m.title.as_deref() == Some("Card"))
        .unwrap();
    assert_eq!(card.0.id.to_string(), "01J8ZQ4KF3T9M2X7C5VBNAE8RD");
    assert_eq!(card.0.links_out.len(), 1);
    let scrap = out.metas.iter().find(|(m, _)| m.title.is_none()).unwrap();
    assert_eq!(scrap.0.status, jd_core::note::Status::Fleeting); // inbox path default
    assert_eq!(
        scrap.0.id,
        synthetic_id(std::path::Path::new("inbox/scrap.md"))
    );
}

#[test]
fn synthetic_ids_are_stable_and_distinct() {
    let a = synthetic_id(std::path::Path::new("inbox/a.md"));
    assert_eq!(a, synthetic_id(std::path::Path::new("inbox/a.md")));
    assert_ne!(a, synthetic_id(std::path::Path::new("inbox/b.md")));
}

#[test]
fn unreadable_files_quarantine_without_failing_the_scan() {
    let (_t, v) = vault_with(&[("notes/good.md", "fine\n")]);
    // invalid UTF-8 file
    std::fs::write(v.root().join("notes/bad.md"), [0xFF, 0xFE, 0x00, 0x01]).unwrap();
    let out = scan(&v, &|_, _| {}).unwrap();
    assert_eq!(out.metas.len(), 1);
    assert_eq!(out.quarantined.len(), 1);
    assert_eq!(
        out.quarantined[0].rel_path,
        std::path::Path::new("notes/bad.md")
    );
}

#[test]
fn skips_hidden_temp_and_non_md() {
    let (_t, v) = vault_with(&[
        ("notes/real.md", "x\n"),
        ("notes/.hidden.md", "x\n"),
        ("notes/.real.md.jd-tmp", "x\n"),
        ("notes/readme.txt", "x\n"),
    ]);
    let out = scan(&v, &|_, _| {}).unwrap();
    assert_eq!(out.metas.len(), 1);
}

#[test]
fn progress_reaches_total() {
    let (_t, v) = vault_with(&[
        ("notes/a.md", "a\n"),
        ("notes/b.md", "b\n"),
        ("inbox/c.md", "c\n"),
    ]);
    use std::sync::atomic::{AtomicUsize, Ordering};
    let max_done = AtomicUsize::new(0);
    let total_seen = AtomicUsize::new(0);
    scan(&v, &|done, total| {
        max_done.fetch_max(done, Ordering::Relaxed);
        total_seen.store(total, Ordering::Relaxed);
    })
    .unwrap();
    assert_eq!(max_done.load(Ordering::Relaxed), 3);
    assert_eq!(total_seen.load(Ordering::Relaxed), 3);
}

#[test]
fn subdirectories_are_scanned() {
    let (_t, v) = vault_with(&[("notes/sub/deep.md", "# Deep\n")]);
    let out = scan(&v, &|_, _| {}).unwrap();
    assert_eq!(out.metas.len(), 1);
    assert_eq!(
        out.metas[0].0.rel_path,
        std::path::Path::new("notes/sub/deep.md")
    );
}
