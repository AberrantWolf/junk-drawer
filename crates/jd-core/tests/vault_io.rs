//! Atomic-save torture (spec §13): a kill between temp-write and rename must
//! leave the original byte-intact. Plus filename sanitization/collision.

mod common;

use common::TempDir;
use jd_core::id::NoteId;
use jd_core::vault::io::{atomic_save, filename_for, sanitize_filename};

#[test]
fn atomic_save_writes_and_replaces() {
    let t = TempDir::new();
    let f = t.path().join("note.md");
    atomic_save(&f, "first").unwrap();
    assert_eq!(std::fs::read_to_string(&f).unwrap(), "first");
    atomic_save(&f, "second").unwrap();
    assert_eq!(std::fs::read_to_string(&f).unwrap(), "second");
    // no temp litter after success
    let leftovers: Vec<_> = std::fs::read_dir(t.path())
        .unwrap()
        .filter(|e| {
            e.as_ref()
                .unwrap()
                .file_name()
                .to_string_lossy()
                .contains("jd-tmp")
        })
        .collect();
    assert!(leftovers.is_empty());
}

#[test]
fn torture_kill_before_rename_leaves_original_intact() {
    let t = TempDir::new();
    let f = t.path().join("note.md");
    atomic_save(&f, "precious original").unwrap();

    // simulate a crash after the temp file is written+synced but before rename
    let killed = jd_core::vault::io::atomic_save_with(&f, "half-baked", &|phase| {
        if phase == "written" {
            Err(std::io::Error::other("simulated kill"))
        } else {
            Ok(())
        }
    });
    assert!(killed.is_err());
    assert_eq!(
        std::fs::read_to_string(&f).unwrap(),
        "precious original",
        "the original must survive a crash mid-save"
    );

    // and the next save recovers cleanly, overwriting any stale temp
    atomic_save(&f, "fresh").unwrap();
    assert_eq!(std::fs::read_to_string(&f).unwrap(), "fresh");
}

#[test]
fn sanitize_strips_forbidden_and_caps_length() {
    assert_eq!(
        sanitize_filename("Egui: immediate/mode <tradeoffs>?"),
        "Egui immediatemode tradeoffs"
    );
    assert_eq!(sanitize_filename("  trailing dots... "), "trailing dots");
    assert_eq!(sanitize_filename(""), "Untitled");
    assert_eq!(sanitize_filename("///"), "Untitled");
    let long = "x".repeat(500);
    assert!(sanitize_filename(&long).len() <= 120);
    // multibyte-safe cap
    let long_multi = "é".repeat(300);
    let s = sanitize_filename(&long_multi);
    assert!(s.len() <= 120 && s.chars().all(|c| c == 'é'));
}

#[test]
fn filename_for_suffixes_on_collision() {
    let t = TempDir::new();
    let id = NoteId::parse("01J8ZQ4KF3T9M2X7C5VBNAE8RD").unwrap();
    let first = filename_for("My Note", id, t.path());
    assert_eq!(first.file_name().unwrap().to_str().unwrap(), "My Note.md");
    std::fs::write(&first, "occupied").unwrap();
    let second = filename_for("My Note", id, t.path());
    assert_eq!(
        second.file_name().unwrap().to_str().unwrap(),
        "My Note (01J8ZQ4K).md"
    );
}
