//! WP3 Task 1: jd-core hardening — rollback failure surfacing and
//! path-stability pins.
//!
//! These tests use the `atomic_save` failpoint (vault::io::failpoint), which
//! is process-global.  They live in their own test binary so arming the
//! failpoint cannot disturb other suites, and they serialize among themselves
//! via FAILPOINT_LOCK.

mod common;

use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant};

use common::TempDir;
use jd_core::command::{Dest, OpResult, OpSource, VaultOp};
use jd_core::id::NoteId;
use jd_core::note::{Kind, NewNote, Status};
use jd_core::vault::Vault;
use jd_core::vault::io::failpoint;
use jd_core::worker::{VaultCommand, VaultEvent, VaultHandle, start};

/// The failpoint counter is process-global: every test in this binary (even
/// ones that don't arm it) must hold this lock so an armed failpoint only
/// ever sees the saves of the test that armed it.
static FAILPOINT_LOCK: Mutex<()> = Mutex::new(());

fn lock() -> MutexGuard<'static, ()> {
    FAILPOINT_LOCK.lock().unwrap_or_else(|p| p.into_inner())
}

fn boot(t: &TempDir) -> VaultHandle {
    let v = Vault::open(t.path()).unwrap();
    let h = start(v, Box::new(|| {})).unwrap();
    drain_until(&h, |e| {
        matches!(e, VaultEvent::ScanComplete { .. }).then_some(())
    });
    h
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

fn read_body(h: &VaultHandle, id: NoteId) -> String {
    h.commands.send(VaultCommand::ReadBody { id }).unwrap();
    drain_until(h, |e| match e {
        VaultEvent::Body { id: bid, content } if *bid == id => Some(content.clone()),
        _ => None,
    })
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

// ── Item 1: batch rollback failures are surfaced ────────────────────────────

/// Batch of [SaveBody(ok), Toss(bogus → fails)] where the SaveBody ROLLBACK
/// also fails (failpoint: skip the forward save, fail the rollback save).
/// The vault may be mixed-state — the worker must emit
/// VaultEvent::Error { context: "batch rollback" } naming the op that failed
/// to roll back, in addition to the usual OpFailed.
#[test]
fn batch_rollback_failure_is_surfaced_as_error_event() {
    let _g = lock();
    let t = TempDir::new();
    let h = boot(&t);

    let id = send_op(
        &h,
        VaultOp::Create {
            seed: scrap("# Stable\noriginal body\n"),
            dest: Dest::Inbox,
        },
    )
    .created[0];
    let bogus = NoteId([0xEE; 16]);

    // Save order inside the batch:
    //   1. SaveBody forward write        → skip (succeeds)
    //   2. SaveBody rollback write       → fail (Toss(bogus) fails without saving)
    failpoint::arm(1, 1);

    h.commands
        .send(VaultCommand::Op {
            op: VaultOp::Batch(vec![
                VaultOp::SaveBody {
                    id,
                    content: "# Stable\nchanged body\n".into(),
                },
                VaultOp::Toss { id: bogus }, // fails: unknown id, no save involved
            ]),
            source: OpSource::User,
        })
        .unwrap();

    // The rollback-failure Error must arrive (before/alongside OpFailed).
    let mut saw_op_failed = false;
    let message = drain_until(&h, |e| match e {
        VaultEvent::Error { context, message } if context == "batch rollback" => {
            Some(message.clone())
        }
        VaultEvent::OpFailed { .. } => {
            saw_op_failed = true;
            None
        }
        _ => None,
    });
    failpoint::disarm();

    assert!(
        message.contains("Edit"),
        "rollback error must name the op label that failed to roll back: {message}"
    );

    // The OpFailed for the batch itself still arrives.
    if !saw_op_failed {
        drain_until(&h, |e| {
            matches!(e, VaultEvent::OpFailed { .. }).then_some(())
        });
    }

    // Mixed state is expected here: the forward SaveBody landed and could not
    // be rolled back.  The point of this test is the *surfacing*, not repair.
    assert_eq!(read_body(&h, id), "# Stable\nchanged body\n");
}

// ── Item 2: RenameTitle rolls back on mid-loop referrer failure ─────────────

/// Force a referrer-rewrite failure mid-loop: with two referrers, let the
/// self-rename and the first referrer write succeed, fail the second.  The
/// worker must roll back the first referrer AND the self-rename, restoring
/// the vault exactly; the op fails with OpFailed and no Error event (the
/// rollback itself succeeded).
#[test]
fn rename_referrer_failure_mid_loop_rolls_back_self_and_referrers() {
    let _g = lock();
    let t = TempDir::new();
    let h = boot(&t);

    let target = send_op(
        &h,
        VaultOp::Create {
            seed: perm("# Alpha\ntarget body\n"),
            dest: Dest::Notes,
        },
    )
    .created[0];
    let ref1 = send_op(
        &h,
        VaultOp::Create {
            seed: perm("# Ref One\nsee [[Alpha]]\n"),
            dest: Dest::Notes,
        },
    )
    .created[0];
    let ref2 = send_op(
        &h,
        VaultOp::Create {
            seed: perm("# Ref Two\nalso [[Alpha]]\n"),
            dest: Dest::Notes,
        },
    )
    .created[0];

    let target_rel = h
        .index
        .read()
        .unwrap()
        .get(target)
        .unwrap()
        .rel_path
        .clone();
    let target_content_before = std::fs::read_to_string(t.path().join(&target_rel)).unwrap();

    // Save order inside RenameTitle:
    //   1. self-rename write             → skip (succeeds)
    //   2. first referrer write          → skip (succeeds)
    //   3. second referrer write         → fail  → triggers rollback
    //   4.+ rollback writes              → pass (fail window exhausted)
    failpoint::arm(2, 1);

    h.commands
        .send(VaultCommand::Op {
            op: VaultOp::RenameTitle {
                id: target,
                new_title: "Beta".into(),
            },
            source: OpSource::User,
        })
        .unwrap();
    let (label, message) = drain_until(&h, |e| match e {
        VaultEvent::OpFailed { label, message } => Some((label.clone(), message.clone())),
        VaultEvent::Error { context, message } => {
            panic!("rollback itself must succeed here, got Error({context}): {message}")
        }
        _ => None,
    });
    failpoint::disarm();

    assert!(label.starts_with("Rename card"), "{label}");
    assert!(message.contains("referrer"), "{message}");

    // Vault fully restored: self back under the old name with old content…
    assert_eq!(
        std::fs::read_to_string(t.path().join(&target_rel)).unwrap(),
        target_content_before,
        "self-rename must be rolled back"
    );
    let tmeta = h.index.read().unwrap().get(target).cloned().unwrap();
    assert_eq!(tmeta.title.as_deref(), Some("Alpha"));
    assert_eq!(tmeta.rel_path, target_rel);
    assert!(
        !t.path().join("notes/Beta.md").exists(),
        "renamed file must be removed on rollback"
    );

    // …and BOTH referrers still point at [[Alpha]].
    for rid in [ref1, ref2] {
        let rel = h.index.read().unwrap().get(rid).unwrap().rel_path.clone();
        let body = std::fs::read_to_string(t.path().join(&rel)).unwrap();
        assert!(body.contains("[[Alpha]]"), "referrer not restored: {body}");
        assert!(!body.contains("[[Beta]]"), "referrer kept new link: {body}");
    }
    let mut backlinks = h.index.read().unwrap().backlinks(target);
    backlinks.sort();
    let mut expect = vec![ref1, ref2];
    expect.sort();
    assert_eq!(backlinks, expect, "index backlinks must be restored");
}

/// Same mid-loop failure, but the rollback of the self-rename ALSO fails
/// (failpoint window covers it).  The vault is mixed-state — the worker must
/// surface a "batch rollback" Error event naming what failed to roll back.
#[test]
fn rename_rollback_failure_is_surfaced_as_error_event() {
    let _g = lock();
    let t = TempDir::new();
    let h = boot(&t);

    let target = send_op(
        &h,
        VaultOp::Create {
            seed: perm("# Alpha\ntarget body\n"),
            dest: Dest::Notes,
        },
    )
    .created[0];
    let _referrer = send_op(
        &h,
        VaultOp::Create {
            seed: perm("# Ref One\nsee [[Alpha]]\n"),
            dest: Dest::Notes,
        },
    )
    .created[0];

    // Save order inside RenameTitle:
    //   1. self-rename write             → skip (succeeds)
    //   2. referrer write                → fail  → triggers rollback
    //   3. self-rename rollback write    → fail  → mixed state, must surface
    failpoint::arm(1, 2);

    h.commands
        .send(VaultCommand::Op {
            op: VaultOp::RenameTitle {
                id: target,
                new_title: "Beta".into(),
            },
            source: OpSource::User,
        })
        .unwrap();
    let message = drain_until(&h, |e| match e {
        VaultEvent::Error { context, message } if context == "batch rollback" => {
            Some(message.clone())
        }
        _ => None,
    });
    failpoint::disarm();
    assert!(
        message.contains("self"),
        "must name what failed to roll back: {message}"
    );
    drain_until(&h, |e| {
        matches!(e, VaultEvent::OpFailed { .. }).then_some(())
    });
}

// ── Item 3: path-stability decision — drifted-but-correct undo (pinned) ─────

/// Undo RenameTitle after the old title has been re-claimed by another note:
/// the content restore is correct, but the file lands under a
/// collision-suffixed path (accept-and-document decision, 2026-07-07).
#[test]
fn rename_undo_after_reclaimed_title_restores_content_under_suffixed_path() {
    let _g = lock();
    let t = TempDir::new();
    let h = boot(&t);

    let a = send_op(
        &h,
        VaultOp::Create {
            seed: perm("# Alpha\nalpha body\n"),
            dest: Dest::Notes,
        },
    )
    .created[0];
    assert!(t.path().join("notes/Alpha.md").exists());

    let renamed = send_op(
        &h,
        VaultOp::RenameTitle {
            id: a,
            new_title: "Beta".into(),
        },
    );

    // Re-claim the old title with a different note.
    let squatter = send_op(
        &h,
        VaultOp::Create {
            seed: perm("# Alpha\nsquatter body\n"),
            dest: Dest::Notes,
        },
    )
    .created[0];
    let squatter_rel = h
        .index
        .read()
        .unwrap()
        .get(squatter)
        .unwrap()
        .rel_path
        .clone();
    assert_eq!(squatter_rel, std::path::PathBuf::from("notes/Alpha.md"));

    // Undo the rename: content restore is correct, path drifts to a suffix.
    send_op(&h, renamed.inverse.unwrap());

    let ameta = h.index.read().unwrap().get(a).cloned().unwrap();
    assert_eq!(ameta.title.as_deref(), Some("Alpha"), "title restored");
    assert!(read_body(&h, a).contains("alpha body"), "content restored");
    assert_ne!(
        ameta.rel_path, squatter_rel,
        "collision suffixing must prevent clobbering the squatter"
    );
    let name = ameta.rel_path.file_name().unwrap().to_string_lossy();
    assert!(
        name.starts_with("Alpha") && name.ends_with(".md"),
        "drifted path keeps the title stem: {name}"
    );

    // The squatter is untouched, and the index holds both notes.
    assert!(read_body(&h, squatter).contains("squatter body"));
    assert_eq!(h.index.read().unwrap().count(), 2);
}
