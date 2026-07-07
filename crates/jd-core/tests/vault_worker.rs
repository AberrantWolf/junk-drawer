//! Worker contract: serialized writes, echo suppression, conflict copies,
//! event flow. Uses real files + the real watcher.

mod common;

use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use common::TempDir;
use jd_core::command::{Dest, OpResult, OpSource, VaultOp};
use jd_core::id::NoteId;
use jd_core::note::{Kind, NewNote, Status};
use jd_core::vault::Vault;
use jd_core::worker::{VaultCommand, VaultEvent, VaultHandle, start};

fn boot(t: &TempDir) -> (VaultHandle, Arc<Mutex<u32>>) {
    let v = Vault::open(t.path()).unwrap();
    let wakes = Arc::new(Mutex::new(0u32));
    let w = wakes.clone();
    let h = start(v, Box::new(move || *w.lock().unwrap() += 1)).unwrap();
    (h, wakes)
}

fn drain_until<T>(h: &VaultHandle, mut pick: impl FnMut(&VaultEvent) -> Option<T>) -> T {
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if let Ok(ev) = h.events.recv_timeout(Duration::from_millis(50))
            && let Some(t) = pick(&ev)
        {
            return t;
        }
    }
    panic!("expected event never arrived");
}

fn scrap(body: &str) -> NewNote {
    NewNote {
        body: body.to_owned(),
        status: Status::Fleeting,
        kind: Kind::Note,
        source: None,
        tags: vec![],
    }
}

fn perm(body: &str) -> NewNote {
    NewNote {
        body: body.to_owned(),
        status: Status::Permanent,
        kind: Kind::Note,
        source: None,
        tags: vec![],
    }
}

fn send_op(h: &VaultHandle, op: VaultOp) -> OpResult {
    h.commands
        .send(VaultCommand::Op {
            op,
            source: OpSource::User,
        })
        .unwrap();
    drain_until(h, |e| match e {
        VaultEvent::OpDone { result, .. } => Some(result.clone()),
        _ => None,
    })
}

fn read_body(h: &VaultHandle, id: jd_core::id::NoteId) -> String {
    h.commands.send(VaultCommand::ReadBody { id }).unwrap();
    drain_until(h, |e| match e {
        VaultEvent::Body { id: bid, content } if *bid == id => Some(content.clone()),
        _ => None,
    })
}

#[test]
fn boot_scans_existing_notes_into_the_index() {
    let t = TempDir::new();
    {
        let v = Vault::open(t.path()).unwrap();
        std::fs::write(v.root().join("notes/Pre.md"), "# Pre\nexisting\n").unwrap();
    }
    let (h, wakes) = boot(&t);
    drain_until(&h, |e| {
        matches!(e, VaultEvent::ScanComplete { .. }).then_some(())
    });
    assert_eq!(h.index.read().unwrap().count(), 1);
    assert!(*wakes.lock().unwrap() >= 1, "wake must fire on events");
}

#[test]
fn create_writes_a_file_and_indexes_it() {
    let t = TempDir::new();
    let (h, _) = boot(&t);
    drain_until(&h, |e| {
        matches!(e, VaultEvent::ScanComplete { .. }).then_some(())
    });

    let result = send_op(
        &h,
        VaultOp::Create {
            seed: scrap("a fresh thought\n"),
            dest: Dest::Inbox,
        },
    );
    assert_eq!(result.created.len(), 1);
    let id = result.created[0];
    let meta = h.index.read().unwrap().get(id).unwrap().clone();

    assert!(meta.rel_path.starts_with("inbox"));
    assert_eq!(meta.status, Status::Fleeting);
    let on_disk = std::fs::read_to_string(t.path().join(&meta.rel_path)).unwrap();
    assert!(on_disk.contains("a fresh thought"));
    assert!(on_disk.starts_with("---\n"), "frontmatter synthesized");
    assert!(h.index.read().unwrap().get(meta.id).is_some());
}

#[test]
fn save_body_preserves_frontmatter_and_updates_modified() {
    let t = TempDir::new();
    let (h, _) = boot(&t);
    drain_until(&h, |e| {
        matches!(e, VaultEvent::ScanComplete { .. }).then_some(())
    });

    let result = send_op(
        &h,
        VaultOp::Create {
            seed: scrap("v1\n"),
            dest: Dest::Inbox,
        },
    );
    let id = result.created[0];
    let meta = h.index.read().unwrap().get(id).unwrap().clone();

    send_op(
        &h,
        VaultOp::SaveBody {
            id: meta.id,
            content: "v2 body\n".into(),
        },
    );
    let on_disk = std::fs::read_to_string(t.path().join(&meta.rel_path)).unwrap();
    assert!(on_disk.contains("v2 body"));
    assert!(
        on_disk.contains(&meta.id.to_string()),
        "id survives body saves"
    );
    assert!(!on_disk.contains("v1"));
}

#[test]
fn read_body_round_trips() {
    let t = TempDir::new();
    let (h, _) = boot(&t);
    drain_until(&h, |e| {
        matches!(e, VaultEvent::ScanComplete { .. }).then_some(())
    });

    let result = send_op(
        &h,
        VaultOp::Create {
            seed: scrap("the body\n"),
            dest: Dest::Notes,
        },
    );
    let id = result.created[0];
    let body = read_body(&h, id);
    assert!(body.contains("the body"));
}

#[test]
fn our_own_saves_do_not_echo_as_external() {
    let t = TempDir::new();
    let (h, _) = boot(&t);
    drain_until(&h, |e| {
        matches!(e, VaultEvent::ScanComplete { .. }).then_some(())
    });

    let result = send_op(
        &h,
        VaultOp::Create {
            seed: scrap("mine\n"),
            dest: Dest::Inbox,
        },
    );
    let id = result.created[0];
    let meta = h.index.read().unwrap().get(id).unwrap().clone();

    send_op(
        &h,
        VaultOp::SaveBody {
            id: meta.id,
            content: "mine v2\n".into(),
        },
    );

    // wait past the debounce window; no External event for our own write
    let deadline = Instant::now() + Duration::from_millis(800);
    while Instant::now() < deadline {
        if let Ok(ev) = h.events.recv_timeout(Duration::from_millis(50)) {
            assert!(
                !matches!(&ev, VaultEvent::External { changed, .. } if changed.contains(&meta.id)),
                "self-echo leaked: {ev:?}"
            );
        }
    }
}

#[test]
fn external_edits_reindex_and_emit() {
    let t = TempDir::new();
    let (h, _) = boot(&t);
    drain_until(&h, |e| {
        matches!(e, VaultEvent::ScanComplete { .. }).then_some(())
    });

    let result = send_op(
        &h,
        VaultOp::Create {
            seed: scrap("watch me\n"),
            dest: Dest::Notes,
        },
    );
    let id = result.created[0];
    let meta = h.index.read().unwrap().get(id).unwrap().clone();

    // an "external tool" rewrites the file
    std::thread::sleep(Duration::from_millis(300));
    std::fs::write(
        t.path().join(&meta.rel_path),
        "# Retitled\nexternally edited\n",
    )
    .unwrap();
    drain_until(&h, |e| {
        matches!(e, VaultEvent::External { changed, .. } if changed.contains(&meta.id))
            .then_some(())
    });
    let ix = h.index.read().unwrap();
    assert_eq!(ix.get(meta.id).unwrap().title.as_deref(), Some("Retitled"));
}

#[test]
fn concurrent_external_edit_diverts_to_conflict_copy() {
    let t = TempDir::new();
    let (h, _) = boot(&t);
    drain_until(&h, |e| {
        matches!(e, VaultEvent::ScanComplete { .. }).then_some(())
    });

    let result = send_op(
        &h,
        VaultOp::Create {
            seed: scrap("base\n"),
            dest: Dest::Notes,
        },
    );
    let id = result.created[0];
    let meta = h.index.read().unwrap().get(id).unwrap().clone();

    // sneak an external change under the worker (bypassing its ledger),
    // ensuring a different mtime/len than the ledger recorded
    std::thread::sleep(Duration::from_millis(50));
    std::fs::write(
        t.path().join(&meta.rel_path),
        "theirs — changed externally\n",
    )
    .unwrap();

    h.commands
        .send(VaultCommand::Op {
            op: VaultOp::SaveBody {
                id: meta.id,
                content: "ours\n".into(),
            },
            source: OpSource::User,
        })
        .unwrap();
    let copy = drain_until(&h, |e| match e {
        VaultEvent::Conflict { id, conflict_copy } if *id == meta.id => Some(conflict_copy.clone()),
        _ => None,
    });
    // both versions survive (spec §2: never silently clobber either side)
    assert_eq!(
        std::fs::read_to_string(t.path().join(&meta.rel_path)).unwrap(),
        "theirs — changed externally\n"
    );
    let ours = std::fs::read_to_string(t.path().join(&copy)).unwrap();
    assert!(ours.contains("ours"));
}

#[test]
fn same_minute_double_conflict_keeps_both_copies() {
    let t = TempDir::new();
    let (h, _) = boot(&t);
    drain_until(&h, |e| {
        matches!(e, VaultEvent::ScanComplete { .. }).then_some(())
    });

    let result = send_op(
        &h,
        VaultOp::Create {
            seed: scrap("base\n"),
            dest: Dest::Notes,
        },
    );
    let id = result.created[0];
    let meta = h.index.read().unwrap().get(id).unwrap().clone();

    let mut copies = Vec::new();
    for round in 0..2 {
        std::thread::sleep(Duration::from_millis(30));
        std::fs::write(t.path().join(&meta.rel_path), format!("theirs {round}\n")).unwrap();
        h.commands
            .send(VaultCommand::Op {
                op: VaultOp::SaveBody {
                    id: meta.id,
                    content: format!("ours {round}\n"),
                },
                source: OpSource::User,
            })
            .unwrap();
        let copy = drain_until(&h, |e| match e {
            VaultEvent::Conflict { id, conflict_copy } if *id == meta.id => {
                Some(conflict_copy.clone())
            }
            _ => None,
        });
        copies.push(copy);
    }
    assert_ne!(
        copies[0], copies[1],
        "same-minute conflicts must not share a path"
    );
    for (round, copy) in copies.iter().enumerate() {
        let content = std::fs::read_to_string(t.path().join(copy)).unwrap();
        assert!(
            content.contains(&format!("ours {round}")),
            "copy {round} clobbered: {content}"
        );
    }
}

#[test]
fn scan_progress_is_granular_on_boot() {
    let t = TempDir::new();
    {
        let v = Vault::open(t.path()).unwrap();
        for i in 0..130 {
            std::fs::write(
                v.root().join(format!("notes/n{i}.md")),
                format!("note {i}\n"),
            )
            .unwrap();
        }
    }
    let (h, _) = boot(&t);
    let mut progress_events = 0;
    let mut saw_final = false;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while std::time::Instant::now() < deadline && !saw_final {
        if let Ok(ev) = h.events.recv_timeout(std::time::Duration::from_millis(50)) {
            match ev {
                VaultEvent::ScanProgress { done, total } => {
                    progress_events += 1;
                    assert!(done <= total);
                    if done == total { /* fine */ }
                }
                VaultEvent::ScanComplete { .. } => saw_final = true,
                _ => {}
            }
        }
    }
    assert!(saw_final, "ScanComplete never arrived");
    assert!(
        progress_events >= 2,
        "expected granular progress (130 files / 64), got {progress_events}"
    );
}

#[test]
fn shutdown_is_clean() {
    let t = TempDir::new();
    let (h, _) = boot(&t);
    drain_until(&h, |e| {
        matches!(e, VaultEvent::ScanComplete { .. }).then_some(())
    });
    h.commands.send(VaultCommand::Shutdown).unwrap();
    // after shutdown the events channel eventually disconnects
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        match h.events.recv_timeout(Duration::from_millis(50)) {
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
            _ if Instant::now() > deadline => panic!("worker did not shut down"),
            _ => {}
        }
    }
}

// ── 6 new tests ──────────────────────────────────────────────────────────────

#[test]
fn create_in_notes_dest_makes_permanent_note() {
    let t = TempDir::new();
    let (h, _) = boot(&t);
    drain_until(&h, |e| {
        matches!(e, VaultEvent::ScanComplete { .. }).then_some(())
    });

    let result = send_op(
        &h,
        VaultOp::Create {
            seed: perm("# A permanent note\nBody.\n"),
            dest: Dest::Notes,
        },
    );
    assert_eq!(result.created.len(), 1);
    let id = result.created[0];
    let meta = h.index.read().unwrap().get(id).unwrap().clone();

    assert!(
        meta.rel_path.starts_with("notes"),
        "permanent note should be in notes/"
    );
    assert_eq!(meta.status, Status::Permanent);
    // inverse should be a Delete
    assert!(matches!(result.inverse, Some(VaultOp::Delete { id: inv_id }) if inv_id == id));
}

#[test]
fn save_body_inverse_restores_old_content() {
    let t = TempDir::new();
    let (h, _) = boot(&t);
    drain_until(&h, |e| {
        matches!(e, VaultEvent::ScanComplete { .. }).then_some(())
    });
    let created = send_op(
        &h,
        VaultOp::Create {
            seed: scrap("v1\n"),
            dest: Dest::Inbox,
        },
    );
    let id = created.created[0];
    let saved = send_op(
        &h,
        VaultOp::SaveBody {
            id,
            content: "v2\n".into(),
        },
    );
    let inverse = saved.inverse.clone().unwrap();
    assert!(matches!(&inverse, VaultOp::SaveBody { content, .. } if content == "v1\n"));
    send_op(&h, inverse);
    h.commands.send(VaultCommand::ReadBody { id }).unwrap();
    let body = drain_until(&h, |e| match e {
        VaultEvent::Body { id: bid, content } if *bid == id => Some(content.clone()),
        _ => None,
    });
    assert_eq!(body, "v1\n");
}

#[test]
fn toss_restore_round_trip_via_inverses() {
    let t = TempDir::new();
    let (h, _) = boot(&t);
    drain_until(&h, |e| {
        matches!(e, VaultEvent::ScanComplete { .. }).then_some(())
    });
    let created = send_op(
        &h,
        VaultOp::Create {
            seed: scrap("doomed\n"),
            dest: Dest::Inbox,
        },
    );
    let id = created.created[0];
    let rel = h.index.read().unwrap().get(id).unwrap().rel_path.clone();

    let tossed = send_op(&h, VaultOp::Toss { id });
    assert!(tossed.label.starts_with("Toss scrap"), "{}", tossed.label);
    assert!(
        h.index.read().unwrap().get(id).is_none(),
        "tossed note leaves the index"
    );
    assert!(!t.path().join(&rel).exists());

    send_op(&h, tossed.inverse.unwrap()); // Restore
    let meta = h
        .index
        .read()
        .unwrap()
        .get(id)
        .cloned()
        .expect("restored to index");
    assert_eq!(meta.rel_path, rel);
    assert!(t.path().join(&rel).exists());
}

#[test]
fn promote_moves_file_and_demote_reverses() {
    let t = TempDir::new();
    let (h, _) = boot(&t);
    drain_until(&h, |e| {
        matches!(e, VaultEvent::ScanComplete { .. }).then_some(())
    });
    let created = send_op(
        &h,
        VaultOp::Create {
            seed: scrap("# A claim\nbody\n"),
            dest: Dest::Inbox,
        },
    );
    let id = created.created[0];

    let promoted = send_op(&h, VaultOp::Promote { id });
    let meta = h.index.read().unwrap().get(id).cloned().unwrap();
    assert_eq!(meta.status, Status::Permanent);
    assert!(meta.rel_path.starts_with("notes"), "{:?}", meta.rel_path);
    let on_disk = std::fs::read_to_string(t.path().join(&meta.rel_path)).unwrap();
    assert!(on_disk.contains("status: permanent"));

    send_op(&h, promoted.inverse.unwrap()); // Demote
    let meta = h.index.read().unwrap().get(id).cloned().unwrap();
    assert_eq!(meta.status, Status::Fleeting);
    assert!(meta.rel_path.starts_with("inbox"));
}

#[test]
fn set_ops_carry_old_values_in_inverses() {
    let t = TempDir::new();
    let (h, _) = boot(&t);
    drain_until(&h, |e| {
        matches!(e, VaultEvent::ScanComplete { .. }).then_some(())
    });
    let id = send_op(
        &h,
        VaultOp::Create {
            seed: scrap("# T\nx\n"),
            dest: Dest::Notes,
        },
    )
    .created[0];

    let r1 = send_op(
        &h,
        VaultOp::SetSource {
            id,
            source: Some("Ahrens (2017)".into()),
        },
    );
    assert!(matches!(
        r1.inverse,
        Some(VaultOp::SetSource { source: None, .. })
    ));
    let r2 = send_op(
        &h,
        VaultOp::SetSource {
            id,
            source: Some("Luhmann".into()),
        },
    );
    assert!(
        matches!(&r2.inverse, Some(VaultOp::SetSource { source: Some(s), .. }) if s == "Ahrens (2017)")
    );

    let r3 = send_op(
        &h,
        VaultOp::SetKind {
            id,
            kind: Kind::Literature,
        },
    );
    assert!(matches!(
        r3.inverse,
        Some(VaultOp::SetKind {
            kind: Kind::Note,
            ..
        })
    ));
    let meta = h.index.read().unwrap().get(id).cloned().unwrap();
    assert_eq!(meta.kind, Kind::Literature);
    assert_eq!(meta.source.as_deref(), Some("Luhmann"));
}

#[test]
fn batch_rolls_back_on_member_failure() {
    let t = TempDir::new();
    let (h, _) = boot(&t);
    drain_until(&h, |e| {
        matches!(e, VaultEvent::ScanComplete { .. }).then_some(())
    });
    let id = send_op(
        &h,
        VaultOp::Create {
            seed: scrap("body\n"),
            dest: Dest::Inbox,
        },
    )
    .created[0];
    let bogus = NoteId([0xEE; 16]);

    h.commands
        .send(VaultCommand::Op {
            op: VaultOp::Batch(vec![
                VaultOp::SaveBody {
                    id,
                    content: "changed\n".into(),
                },
                VaultOp::Promote { id: bogus }, // fails: unknown id
            ]),
            source: OpSource::User,
        })
        .unwrap();
    let label = drain_until(&h, |e| match e {
        VaultEvent::OpFailed { label, .. } => Some(label.clone()),
        _ => None,
    });
    assert_ne!(
        label, "Operation",
        "OpFailed must carry the failing op's label"
    );

    h.commands.send(VaultCommand::ReadBody { id }).unwrap();
    let body = drain_until(&h, |e| match e {
        VaultEvent::Body { id: bid, content } if *bid == id => Some(content.clone()),
        _ => None,
    });
    assert_eq!(
        body, "body\n",
        "failed batch must roll back completed members"
    );
}

#[test]
fn rename_title_rewrites_referrers_and_inverts() {
    let t = TempDir::new();
    let (h, _) = boot(&t);
    drain_until(&h, |e| {
        matches!(e, VaultEvent::ScanComplete { .. }).then_some(())
    });
    let target = send_op(
        &h,
        VaultOp::Create {
            seed: perm("# Old Name\nbody\n"),
            dest: Dest::Notes,
        },
    )
    .created[0];
    let referrer = send_op(
        &h,
        VaultOp::Create {
            seed: perm("# Referrer\nsee [[Old Name]] and [[old name|shown]]\n"),
            dest: Dest::Notes,
        },
    )
    .created[0];

    let renamed = send_op(
        &h,
        VaultOp::RenameTitle {
            id: target,
            new_title: "New Name".into(),
        },
    );
    let tmeta = h.index.read().unwrap().get(target).cloned().unwrap();
    assert_eq!(tmeta.title.as_deref(), Some("New Name"));
    assert!(tmeta.rel_path.to_string_lossy().contains("New Name"));
    let ref_body = std::fs::read_to_string(
        t.path()
            .join(&h.index.read().unwrap().get(referrer).unwrap().rel_path),
    )
    .unwrap();
    assert!(ref_body.contains("[[New Name]]"), "{ref_body}");
    assert!(
        ref_body.contains("[[New Name|shown]]"),
        "display preserved: {ref_body}"
    );
    assert_eq!(
        h.index.read().unwrap().backlinks(target),
        vec![referrer],
        "links stay resolved"
    );

    send_op(&h, renamed.inverse.unwrap());
    let tmeta = h.index.read().unwrap().get(target).cloned().unwrap();
    assert_eq!(tmeta.title.as_deref(), Some("Old Name"));
}

#[test]
fn rename_untitled_fails_cleanly() {
    let t = TempDir::new();
    let (h, _) = boot(&t);
    drain_until(&h, |e| {
        matches!(e, VaultEvent::ScanComplete { .. }).then_some(())
    });
    let id = send_op(
        &h,
        VaultOp::Create {
            seed: scrap("no heading here\n"),
            dest: Dest::Inbox,
        },
    )
    .created[0];
    h.commands
        .send(VaultCommand::Op {
            op: VaultOp::RenameTitle {
                id,
                new_title: "X".into(),
            },
            source: OpSource::User,
        })
        .unwrap();
    drain_until(&h, |e| {
        matches!(e, VaultEvent::OpFailed { .. }).then_some(())
    });
}

#[test]
fn split_creates_linked_note_and_inverse_unsplits() {
    let t = TempDir::new();
    let (h, _) = boot(&t);
    drain_until(&h, |e| {
        matches!(e, VaultEvent::ScanComplete { .. }).then_some(())
    });
    let body = "# Host\nintro text\n# Second Idea\ntail text\n";
    let id = send_op(
        &h,
        VaultOp::Create {
            seed: perm(body),
            dest: Dest::Notes,
        },
    )
    .created[0];
    let at = body.find("# Second Idea").unwrap();

    let split = send_op(&h, VaultOp::Split { id, at_byte: at });
    assert_eq!(split.created.len(), 1);
    let new_id = split.created[0];
    let new_meta = h.index.read().unwrap().get(new_id).cloned().unwrap();
    assert_eq!(new_meta.title.as_deref(), Some("Second Idea"));
    assert_eq!(new_meta.status, Status::Permanent);
    let host_body = read_body(&h, id);
    assert!(host_body.contains("[[Second Idea]]"), "{host_body}");
    assert!(!host_body.contains("tail text"));

    send_op(&h, split.inverse.unwrap()); // Batch: restore body + delete new note
    assert_eq!(read_body(&h, id), body);
    assert!(h.index.read().unwrap().get(new_id).is_none());
}

#[test]
fn split_of_untitled_tail_makes_a_scrap() {
    let t = TempDir::new();
    let (h, _) = boot(&t);
    drain_until(&h, |e| {
        matches!(e, VaultEvent::ScanComplete { .. }).then_some(())
    });
    let body = "# Host\nkeep this\nand split from here onward\n";
    let id = send_op(
        &h,
        VaultOp::Create {
            seed: perm(body),
            dest: Dest::Notes,
        },
    )
    .created[0];
    let at = body.find("and split").unwrap();
    let split = send_op(&h, VaultOp::Split { id, at_byte: at });
    let new_meta = h
        .index
        .read()
        .unwrap()
        .get(split.created[0])
        .cloned()
        .unwrap();
    assert_eq!(new_meta.status, Status::Fleeting);
    assert!(new_meta.rel_path.starts_with("inbox"));
    assert!(read_body(&h, id).contains("[[and split from here onward]]"));
}

/// Regression: split at a byte offset past multibyte (non-ASCII) characters must
/// not land mid-codepoint or drift.  The split-off body must be exactly the suffix
/// after the split point, and the host body must end with a link to that suffix.
#[test]
fn split_non_ascii_body_byte_offset_correct() {
    let t = TempDir::new();
    let (h, _) = boot(&t);
    drain_until(&h, |e| {
        matches!(e, VaultEvent::ScanComplete { .. }).then_some(())
    });
    // Body has multibyte chars ("é" = 2 bytes, "ö" = 2 bytes) before the split point.
    let body = "# Héllo wörld\nsecond line\n";
    let id = send_op(
        &h,
        VaultOp::Create {
            seed: perm(body),
            dest: Dest::Notes,
        },
    )
    .created[0];

    // Split at the start of "second line" — byte offset immediately after the '\n'.
    // "# Héllo wörld\n" is 16 bytes (h=1, é=2, l=1, l=1, o=1, ' '=1, w=1, ö=2, r=1,
    // l=1, d=1, \n=1 → "# " (2) + "H" (1) + "é" (2) + "llo" (3) + " " (1) + "w" (1)
    // + "ö" (2) + "rld" (3) + "\n" (1) = 16 bytes).
    let at_byte = body
        .find("second line")
        .expect("second line must be in body");

    // Verify the byte offset is on a char boundary (defensive assertion).
    assert!(
        body.is_char_boundary(at_byte),
        "test setup: at_byte {at_byte} must be on a char boundary"
    );

    let split = send_op(&h, VaultOp::Split { id, at_byte });
    assert_eq!(
        split.created.len(),
        1,
        "split must produce exactly one new note"
    );
    let new_id = split.created[0];

    // The split-off body must be exactly the suffix starting at at_byte.
    let expected_suffix = &body[at_byte..];
    let split_off_body = read_body(&h, new_id);
    assert_eq!(
        split_off_body, expected_suffix,
        "split-off body must be exactly the suffix after the split point"
    );

    // The host body must contain only the prefix (up to at_byte) and the wiki-link.
    let host_body = read_body(&h, id);
    let expected_prefix = &body[..at_byte];
    assert!(
        host_body.starts_with(expected_prefix),
        "host body must start with the prefix before the split point; \
         got: {host_body:?}, expected prefix: {expected_prefix:?}"
    );
    // Host body must not contain the raw suffix text as a bare line (it moved to
    // the split-off); it will contain the link text inside [[…]] which is fine.
    // We check that "second line" only appears inside a wiki-link, not as raw text.
    assert!(
        !host_body.contains("\nsecond line\n"),
        "host body must not contain bare 'second line' text; got: {host_body:?}"
    );
    // Host body must contain a wiki-link to the split-off.
    assert!(
        host_body.contains("[["),
        "host body must contain a wiki-link to the split-off; got: {host_body:?}"
    );
}

#[test]
fn external_edit_preserves_created_timestamp() {
    let t = TempDir::new();
    let (h, _) = boot(&t);
    drain_until(&h, |e| {
        matches!(e, VaultEvent::ScanComplete { .. }).then_some(())
    });
    let id = send_op(
        &h,
        VaultOp::Create {
            seed: scrap("original\n"),
            dest: Dest::Notes,
        },
    )
    .created[0];
    let created_before = h.index.read().unwrap().get(id).unwrap().created;

    std::thread::sleep(Duration::from_millis(1100)); // ensure fs mtime differs at second granularity
    let rel = h.index.read().unwrap().get(id).unwrap().rel_path.clone();
    std::fs::write(
        t.path().join(&rel),
        "externally rewritten, no frontmatter\n",
    )
    .unwrap();
    drain_until(&h, |e| {
        matches!(e, VaultEvent::External { changed, .. } if changed.contains(&id)).then_some(())
    });
    let created_after = h.index.read().unwrap().get(id).unwrap().created;
    assert_eq!(
        created_after, created_before,
        "WP1d handoff: created carries forward"
    );
}
