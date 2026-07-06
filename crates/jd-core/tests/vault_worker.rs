//! Worker contract: serialized writes, echo suppression, conflict copies,
//! event flow. Uses real files + the real watcher.

mod common;

use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use common::TempDir;
use jd_core::note::{Kind, NewNote, Status};
use jd_core::vault::Vault;
use jd_core::worker::{Dest, VaultCommand, VaultEvent, VaultHandle, start};

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

    h.commands
        .send(VaultCommand::Create {
            seed: scrap("a fresh thought\n"),
            dest: Dest::Inbox,
        })
        .unwrap();
    let meta = drain_until(&h, |e| match e {
        VaultEvent::Created { meta } => Some(meta.clone()),
        _ => None,
    });
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
    h.commands
        .send(VaultCommand::Create {
            seed: scrap("v1\n"),
            dest: Dest::Inbox,
        })
        .unwrap();
    let meta = drain_until(&h, |e| match e {
        VaultEvent::Created { meta } => Some(meta.clone()),
        _ => None,
    });

    h.commands
        .send(VaultCommand::SaveBody {
            id: meta.id,
            content: "v2 body\n".into(),
        })
        .unwrap();
    drain_until(&h, |e| {
        matches!(e, VaultEvent::Saved { id } if *id == meta.id).then_some(())
    });
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
    h.commands
        .send(VaultCommand::Create {
            seed: scrap("the body\n"),
            dest: Dest::Notes,
        })
        .unwrap();
    let meta = drain_until(&h, |e| match e {
        VaultEvent::Created { meta } => Some(meta.clone()),
        _ => None,
    });
    h.commands
        .send(VaultCommand::ReadBody { id: meta.id })
        .unwrap();
    let body = drain_until(&h, |e| match e {
        VaultEvent::Body { id, content } if *id == meta.id => Some(content.clone()),
        _ => None,
    });
    assert!(body.contains("the body"));
}

#[test]
fn our_own_saves_do_not_echo_as_external() {
    let t = TempDir::new();
    let (h, _) = boot(&t);
    drain_until(&h, |e| {
        matches!(e, VaultEvent::ScanComplete { .. }).then_some(())
    });
    h.commands
        .send(VaultCommand::Create {
            seed: scrap("mine\n"),
            dest: Dest::Inbox,
        })
        .unwrap();
    let meta = drain_until(&h, |e| match e {
        VaultEvent::Created { meta } => Some(meta.clone()),
        _ => None,
    });
    h.commands
        .send(VaultCommand::SaveBody {
            id: meta.id,
            content: "mine v2\n".into(),
        })
        .unwrap();
    drain_until(&h, |e| matches!(e, VaultEvent::Saved { .. }).then_some(()));

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
    h.commands
        .send(VaultCommand::Create {
            seed: scrap("watch me\n"),
            dest: Dest::Notes,
        })
        .unwrap();
    let meta = drain_until(&h, |e| match e {
        VaultEvent::Created { meta } => Some(meta.clone()),
        _ => None,
    });

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
    h.commands
        .send(VaultCommand::Create {
            seed: scrap("base\n"),
            dest: Dest::Notes,
        })
        .unwrap();
    let meta = drain_until(&h, |e| match e {
        VaultEvent::Created { meta } => Some(meta.clone()),
        _ => None,
    });

    // sneak an external change under the worker (bypassing its ledger),
    // ensuring a different mtime/len than the ledger recorded
    std::thread::sleep(Duration::from_millis(50));
    std::fs::write(
        t.path().join(&meta.rel_path),
        "theirs — changed externally\n",
    )
    .unwrap();

    h.commands
        .send(VaultCommand::SaveBody {
            id: meta.id,
            content: "ours\n".into(),
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
    h.commands
        .send(VaultCommand::Create {
            seed: scrap("base\n"),
            dest: Dest::Notes,
        })
        .unwrap();
    let meta = drain_until(&h, |e| match e {
        VaultEvent::Created { meta } => Some(meta.clone()),
        _ => None,
    });

    let mut copies = Vec::new();
    for round in 0..2 {
        std::thread::sleep(Duration::from_millis(30));
        std::fs::write(t.path().join(&meta.rel_path), format!("theirs {round}\n")).unwrap();
        h.commands
            .send(VaultCommand::SaveBody {
                id: meta.id,
                content: format!("ours {round}\n"),
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
