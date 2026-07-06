//! Trash lifecycle (spec §7): tossed notes are recoverable per retention;
//! recovery journal survives the autosave debounce window (spec §3).

mod common;

use common::TempDir;
use jd_core::note::{Kind, NoteMeta, Status};
use jd_core::time::Timestamp;
use jd_core::vault::Vault;
use jd_core::vault::recovery::{clear_buffer, journal_buffer, pending_recoveries};
use jd_core::vault::trash::{list_trash, purge_older_than, restore, trash_note};

fn meta_for(v: &Vault, rel: &str, body: &str) -> NoteMeta {
    std::fs::write(v.root().join(rel), body).unwrap();
    let id = jd_core::vault::scan::synthetic_id(std::path::Path::new(rel));
    NoteMeta {
        id,
        rel_path: rel.into(),
        title: None,
        first_line: body.lines().next().unwrap_or("").to_owned(),
        status: Status::Fleeting,
        kind: Kind::Note,
        source: None,
        created: Timestamp(0),
        modified: Timestamp(0),
        tags: Default::default(),
        links_out: vec![],
        word_count: 1,
    }
}

#[test]
fn trash_restore_round_trip() {
    let t = TempDir::new();
    let v = Vault::open(t.path()).unwrap();
    let meta = meta_for(&v, "inbox/tossme.md", "a doomed thought\n");
    trash_note(&v, &meta).unwrap();
    assert!(!t.path().join("inbox/tossme.md").exists());

    let listed = list_trash(&v);
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].id, meta.id);
    assert_eq!(listed[0].title_or_first_line, "a doomed thought");

    let back = restore(&v, meta.id).unwrap();
    assert_eq!(back, std::path::Path::new("inbox/tossme.md"));
    assert_eq!(
        std::fs::read_to_string(t.path().join("inbox/tossme.md")).unwrap(),
        "a doomed thought\n"
    );
    assert!(list_trash(&v).is_empty());
}

#[test]
fn restore_recollides_when_the_name_is_retaken() {
    let t = TempDir::new();
    let v = Vault::open(t.path()).unwrap();
    let meta = meta_for(&v, "notes/Taken.md", "original\n");
    trash_note(&v, &meta).unwrap();
    std::fs::write(t.path().join("notes/Taken.md"), "usurper\n").unwrap();
    let back = restore(&v, meta.id).unwrap();
    assert_ne!(back, std::path::Path::new("notes/Taken.md"));
    assert_eq!(
        std::fs::read_to_string(t.path().join("notes/Taken.md")).unwrap(),
        "usurper\n"
    );
    assert_eq!(
        std::fs::read_to_string(v.root().join(&back)).unwrap(),
        "original\n"
    );
}

#[test]
fn purge_respects_retention_and_manual_mode() {
    let t = TempDir::new();
    let v = Vault::open(t.path()).unwrap();
    let meta = meta_for(&v, "inbox/old.md", "old scrap\n");
    trash_note(&v, &meta).unwrap();

    // manual-only never purges
    assert_eq!(purge_older_than(&v, None).unwrap(), 0);
    assert_eq!(list_trash(&v).len(), 1);
    // 0-day retention purges everything deleted before "now"
    assert_eq!(purge_older_than(&v, Some(0)).unwrap(), 1);
    assert!(list_trash(&v).is_empty());
}

#[test]
fn recovery_journal_round_trip() {
    let t = TempDir::new();
    let v = Vault::open(t.path()).unwrap();
    let id = jd_core::vault::scan::synthetic_id(std::path::Path::new("inbox/x.md"));
    journal_buffer(&v, id, "unsaved keystrokes").unwrap();
    let pending = pending_recoveries(&v);
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0], (id, "unsaved keystrokes".to_owned()));
    clear_buffer(&v, id);
    assert!(pending_recoveries(&v).is_empty());
}
