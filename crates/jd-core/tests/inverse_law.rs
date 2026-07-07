//! Inverse-law test suite: for every vault operation, execute the op, capture
//! its inverse, execute the inverse, and assert the vault state is restored to
//! exactly what it was before.

mod common;

use std::path::PathBuf;
use std::time::{Duration, Instant};

use common::TempDir;
use jd_core::command::{Dest, OpResult, OpSource, VaultOp};
use jd_core::id::NoteId;
use jd_core::note::{Kind, NewNote, Status};
use jd_core::vault::Vault;
use jd_core::worker::{VaultCommand, VaultEvent, VaultHandle, start};

// ── helpers ──────────────────────────────────────────────────────────────────

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

/// (id, rel_path, status, kind, title, sorted_tags, source, body)
type NoteSnap = (
    NoteId,
    PathBuf,
    Status,
    Kind,
    Option<String>,
    Vec<String>,
    Option<String>,
    String,
);

fn collect_md_files(dir: &std::path::Path, root: &std::path::Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                files.extend(collect_md_files(&path, root));
            } else if path.extension().and_then(|e| e.to_str()) == Some("md")
                && let Ok(rel) = path.strip_prefix(root)
            {
                files.push(rel.to_owned());
            }
        }
    }
    files
}

fn snapshot(h: &VaultHandle, root: &std::path::Path) -> (Vec<NoteSnap>, Vec<PathBuf>) {
    // Collect all metadata first under one lock, then release before calling read_body
    let meta_list: Vec<_> = {
        let ix = h.index.read().unwrap();
        ix.iter_meta()
            .map(|m| {
                let tags: Vec<String> = m.tags.iter().map(|t| t.as_str().to_owned()).collect();
                (
                    m.id,
                    m.rel_path.clone(),
                    m.status,
                    m.kind,
                    m.title.clone(),
                    tags,
                    m.source.clone(),
                )
            })
            .collect()
    };

    let mut snaps: Vec<NoteSnap> = meta_list
        .into_iter()
        .map(|(id, rel_path, status, kind, title, tags, source)| {
            let body = read_body(h, id);
            (id, rel_path, status, kind, title, tags, source, body)
        })
        .collect();
    snaps.sort_by_key(|s| s.0);

    // Collect .md files under inbox/ and notes/
    let mut files = Vec::new();
    for sub in ["inbox", "notes"] {
        let subdir = root.join(sub);
        files.extend(collect_md_files(&subdir, root));
    }
    files.sort();

    (snaps, files)
}

fn assert_restored(before: (Vec<NoteSnap>, Vec<PathBuf>), after: (Vec<NoteSnap>, Vec<PathBuf>)) {
    assert_eq!(before.1, after.1, "on-disk .md file list differs");
    assert_eq!(before.0.len(), after.0.len(), "note count differs");
    for (b, a) in before.0.iter().zip(after.0.iter()) {
        assert_eq!(b.0, a.0, "id mismatch");
        assert_eq!(b.1, a.1, "rel_path mismatch for {:?}", b.0);
        assert_eq!(b.2, a.2, "status mismatch for {:?}", b.0);
        assert_eq!(b.3, a.3, "kind mismatch for {:?}", b.0);
        assert_eq!(b.4, a.4, "title mismatch for {:?}", b.0);
        assert_eq!(b.5, a.5, "tags mismatch for {:?}", b.0);
        assert_eq!(b.6, a.6, "source mismatch for {:?}", b.0);
        assert_eq!(b.7, a.7, "body mismatch for {:?}", b.0);
    }
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

// ── tests ─────────────────────────────────────────────────────────────────────

#[test]
fn inverse_create() {
    let t = TempDir::new();
    let h = boot(&t);
    let before = snapshot(&h, t.path());
    let result = send_op(
        &h,
        VaultOp::Create {
            seed: scrap("hello\n"),
            dest: Dest::Inbox,
        },
    );
    let inv = result.inverse.unwrap();
    send_op(&h, inv);
    let after = snapshot(&h, t.path());
    // The inverse of Create is Delete, which moves the file to trash. The
    // snapshot covers only inbox/ and notes/, so trash residue is invisible —
    // and that is CORRECT: trash is the safety floor, not vault state.
    assert_restored(before, after);
}

#[test]
fn inverse_save_body() {
    let t = TempDir::new();
    let h = boot(&t);
    let result = send_op(
        &h,
        VaultOp::Create {
            seed: scrap("original\n"),
            dest: Dest::Inbox,
        },
    );
    let id = result.created[0];
    let before = snapshot(&h, t.path());
    let result = send_op(
        &h,
        VaultOp::SaveBody {
            id,
            content: "changed\n".into(),
        },
    );
    let inv = result.inverse.unwrap();
    send_op(&h, inv);
    let after = snapshot(&h, t.path());
    assert_restored(before, after);
}

#[test]
fn inverse_rename_title() {
    let t = TempDir::new();
    let h = boot(&t);
    let result = send_op(
        &h,
        VaultOp::Create {
            seed: perm("# Old Title\nbody\n"),
            dest: Dest::Notes,
        },
    );
    let id = result.created[0];
    let _ = send_op(
        &h,
        VaultOp::Create {
            seed: scrap("see [[Old Title]]\n"),
            dest: Dest::Inbox,
        },
    );
    let before = snapshot(&h, t.path());
    let result = send_op(
        &h,
        VaultOp::RenameTitle {
            id,
            new_title: "New Title".into(),
        },
    );
    let inv = result.inverse.unwrap();
    send_op(&h, inv);
    let after = snapshot(&h, t.path());
    assert_restored(before, after);
}

#[test]
fn inverse_promote() {
    let t = TempDir::new();
    let h = boot(&t);
    let result = send_op(
        &h,
        VaultOp::Create {
            seed: scrap("fleeting\n"),
            dest: Dest::Inbox,
        },
    );
    let id = result.created[0];
    let before = snapshot(&h, t.path());
    let result = send_op(&h, VaultOp::Promote { id });
    let inv = result.inverse.unwrap();
    send_op(&h, inv);
    let after = snapshot(&h, t.path());
    assert_restored(before, after);
}

#[test]
fn inverse_demote() {
    let t = TempDir::new();
    let h = boot(&t);
    let result = send_op(
        &h,
        VaultOp::Create {
            seed: perm("permanent\n"),
            dest: Dest::Notes,
        },
    );
    let id = result.created[0];
    let before = snapshot(&h, t.path());
    let result = send_op(&h, VaultOp::Demote { id });
    let inv = result.inverse.unwrap();
    send_op(&h, inv);
    let after = snapshot(&h, t.path());
    assert_restored(before, after);
}

#[test]
fn inverse_set_kind() {
    let t = TempDir::new();
    let h = boot(&t);
    let result = send_op(
        &h,
        VaultOp::Create {
            seed: scrap("some note\n"),
            dest: Dest::Inbox,
        },
    );
    let id = result.created[0];
    let before = snapshot(&h, t.path());
    let result = send_op(
        &h,
        VaultOp::SetKind {
            id,
            kind: Kind::Literature,
        },
    );
    let inv = result.inverse.unwrap();
    send_op(&h, inv);
    let after = snapshot(&h, t.path());
    assert_restored(before, after);
}

#[test]
fn inverse_set_source() {
    let t = TempDir::new();
    let h = boot(&t);
    let result = send_op(
        &h,
        VaultOp::Create {
            seed: scrap("note\n"),
            dest: Dest::Inbox,
        },
    );
    let id = result.created[0];
    let before = snapshot(&h, t.path());
    let result = send_op(
        &h,
        VaultOp::SetSource {
            id,
            source: Some("Ahrens".into()),
        },
    );
    let inv = result.inverse.unwrap();
    send_op(&h, inv);
    let after = snapshot(&h, t.path());
    assert_restored(before, after);
}

#[test]
fn inverse_set_tags() {
    use jd_core::tag::Tag;
    let t = TempDir::new();
    let h = boot(&t);
    let result = send_op(
        &h,
        VaultOp::Create {
            seed: scrap("tagged\n"),
            dest: Dest::Inbox,
        },
    );
    let id = result.created[0];
    send_op(
        &h,
        VaultOp::SetTags {
            id,
            tags: vec![Tag::new("zettelkasten").unwrap()],
        },
    );
    let before = snapshot(&h, t.path());
    let result = send_op(
        &h,
        VaultOp::SetTags {
            id,
            tags: vec![Tag::new("rust").unwrap(), Tag::new("tools").unwrap()],
        },
    );
    let inv = result.inverse.unwrap();
    send_op(&h, inv);
    let after = snapshot(&h, t.path());
    assert_restored(before, after);
}

#[test]
fn inverse_toss() {
    let t = TempDir::new();
    let h = boot(&t);
    let result = send_op(
        &h,
        VaultOp::Create {
            seed: scrap("toss me\n"),
            dest: Dest::Inbox,
        },
    );
    let id = result.created[0];
    let before = snapshot(&h, t.path());
    let result = send_op(&h, VaultOp::Toss { id });
    let inv = result.inverse.unwrap();
    send_op(&h, inv);
    let after = snapshot(&h, t.path());
    assert_restored(before, after);
}

#[test]
fn inverse_delete() {
    let t = TempDir::new();
    let h = boot(&t);
    let result = send_op(
        &h,
        VaultOp::Create {
            seed: scrap("delete me\n"),
            dest: Dest::Inbox,
        },
    );
    let id = result.created[0];
    let before = snapshot(&h, t.path());
    let result = send_op(&h, VaultOp::Delete { id });
    let inv = result.inverse.unwrap();
    send_op(&h, inv);
    let after = snapshot(&h, t.path());
    assert_restored(before, after);
}

#[test]
fn inverse_restore() {
    let t = TempDir::new();
    let h = boot(&t);
    let result = send_op(
        &h,
        VaultOp::Create {
            seed: scrap("restore me\n"),
            dest: Dest::Inbox,
        },
    );
    let id = result.created[0];
    send_op(&h, VaultOp::Toss { id });
    let before = snapshot(&h, t.path());
    let result = send_op(&h, VaultOp::Restore { id });
    let inv = result.inverse.unwrap();
    send_op(&h, inv);
    let after = snapshot(&h, t.path());
    assert_restored(before, after);
}

#[test]
fn inverse_split() {
    let t = TempDir::new();
    let h = boot(&t);
    let body = "# Host\nintro\n# Second Part\ntail\n";
    let result = send_op(
        &h,
        VaultOp::Create {
            seed: perm(body),
            dest: Dest::Notes,
        },
    );
    let id = result.created[0];
    let before = snapshot(&h, t.path());
    let at_byte = body.find("# Second Part").unwrap();
    let result = send_op(&h, VaultOp::Split { id, at_byte });
    let inv = result.inverse.unwrap();
    send_op(&h, inv);
    let after = snapshot(&h, t.path());
    assert_restored(before, after);
}

#[test]
fn inverse_batch() {
    let t = TempDir::new();
    let h = boot(&t);
    // Use a headed scrap so filename comes from the heading (stable across body changes).
    let result = send_op(
        &h,
        VaultOp::Create {
            seed: scrap("# Stable Title\noriginal body\n"),
            dest: Dest::Inbox,
        },
    );
    let id = result.created[0];
    let before = snapshot(&h, t.path());
    let result = send_op(
        &h,
        VaultOp::Batch(vec![
            VaultOp::SaveBody {
                id,
                content: "# Stable Title\nchanged body\n".into(),
            },
            VaultOp::SetKind {
                id,
                kind: Kind::Literature,
            },
            VaultOp::Promote { id },
        ]),
    );
    let inv = result.inverse.unwrap();
    send_op(&h, inv);
    let after = snapshot(&h, t.path());
    assert_restored(before, after);
}

#[test]
fn setter_source_quote_substitution() {
    let t = TempDir::new();
    let h = boot(&t);
    let result = send_op(
        &h,
        VaultOp::Create {
            seed: scrap("note\n"),
            dest: Dest::Inbox,
        },
    );
    let id = result.created[0];
    let result = send_op(
        &h,
        VaultOp::SetSource {
            id,
            source: Some(r#"say "hi""#.into()),
        },
    );
    // Documented lossy behavior: `"` is substituted with `'` on write, so the
    // stored value is `say 'hi'`, not the original `say "hi"`.
    let meta = h.index.read().unwrap().get(id).unwrap().clone();
    assert_eq!(meta.source.as_deref(), Some("say 'hi'"));
    // Inverse of the first set restores the pre-op value (None).
    let inv = result.inverse.clone().unwrap();
    assert_eq!(inv, VaultOp::SetSource { id, source: None });

    // Overwrite the source, then undo: the inverse carries — and restores —
    // the SUBSTITUTED value `say 'hi'`, NOT the original `say "hi"`.
    // The substitution is one-way; undo cannot resurrect the double quote.
    let result = send_op(
        &h,
        VaultOp::SetSource {
            id,
            source: Some("other".into()),
        },
    );
    let inv = result.inverse.unwrap();
    assert_eq!(
        inv,
        VaultOp::SetSource {
            id,
            source: Some("say 'hi'".into()),
        }
    );
    send_op(&h, inv);
    let meta_after = h.index.read().unwrap().get(id).unwrap().clone();
    assert_eq!(meta_after.source.as_deref(), Some("say 'hi'"));
}

#[test]
fn setter_set_tags_empty_removes_line() {
    use jd_core::tag::Tag;
    let t = TempDir::new();
    let h = boot(&t);
    let result = send_op(
        &h,
        VaultOp::Create {
            seed: scrap("tagged\n"),
            dest: Dest::Inbox,
        },
    );
    let id = result.created[0];
    send_op(
        &h,
        VaultOp::SetTags {
            id,
            tags: vec![Tag::new("rust").unwrap()],
        },
    );
    send_op(&h, VaultOp::SetTags { id, tags: vec![] });
    let rel = h.index.read().unwrap().get(id).unwrap().rel_path.clone();
    let raw = std::fs::read_to_string(t.path().join(&rel)).unwrap();
    assert!(!raw.contains("tags:"), "tags line must be removed: {raw}");
}

#[test]
fn setter_bare_scalar_tag_scans() {
    use jd_core::tag::Tag;
    let t = TempDir::new();
    // Open vault first to create directory structure
    let v = Vault::open(t.path()).unwrap();
    // Write a file with bare scalar tags: solo
    std::fs::write(
        v.root().join("notes/Solo.md"),
        "---\nid: 01JAAAAAAAAAAAAAAAAAAAAAA1\nstatus: permanent\ntags: solo\n---\n# Solo\nbody\n",
    )
    .unwrap();
    drop(v); // close before booting worker
    let h = boot(&t);
    let ids = h
        .index
        .read()
        .unwrap()
        .notes_with_tag(&Tag::new("solo").unwrap());
    assert_eq!(ids.len(), 1);
}
