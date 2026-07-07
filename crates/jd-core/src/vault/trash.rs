//! .junkdrawer/trash/: <ULID>.md (the note bytes) + <ULID>.meta (3 lines:
//! original rel path / deleted-at RFC3339 / display line). Disposable state.

use std::path::{Path, PathBuf};

use crate::error::IoError;
use crate::id::NoteId;
use crate::note::NoteMeta;
use crate::time::Timestamp;
use crate::vault::Vault;
use crate::vault::io::filename_for;

pub struct TrashEntry {
    pub id: NoteId,
    pub title_or_first_line: String,
    pub deleted: Timestamp,
}

fn trash_dir(vault: &Vault) -> PathBuf {
    vault.abs(Path::new(".junkdrawer/trash"))
}

/// Move a note into the trash directory.
///
/// pub(crate): callable only via the vault worker's `VaultOp`s — enforced structurally since WP1e.
pub(crate) fn trash_note(vault: &Vault, meta: &NoteMeta) -> Result<(), IoError> {
    let dir = trash_dir(vault);
    let src = vault.abs(&meta.rel_path);
    let dst = dir.join(format!("{}.md", meta.id));
    std::fs::rename(&src, &dst).map_err(IoError::wrap("move to trash", &src))?;
    let display = meta
        .title
        .clone()
        .unwrap_or_else(|| meta.first_line.clone());
    let sidecar = format!(
        "{}\n{}\n{}\n",
        meta.rel_path.display(),
        Timestamp::now().to_rfc3339(),
        display
    );
    let side_path = dir.join(format!("{}.meta", meta.id));
    // If this sidecar write fails after the rename above, the note's bytes are
    // safe in trash/ but invisible to list_trash (which reads .meta files).
    // Data is never lost on partial failure; orphaned .md files are manually
    // recoverable and harmless.
    std::fs::write(&side_path, sidecar).map_err(IoError::wrap("record trash entry", &side_path))
}

pub fn list_trash(vault: &Vault) -> Vec<TrashEntry> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(trash_dir(vault)) else {
        return out;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().is_some_and(|e| e == "meta") {
            let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
                continue;
            };
            let Ok(id) = NoteId::parse(stem) else {
                continue;
            };
            let Ok(side) = std::fs::read_to_string(&path) else {
                continue;
            };
            let mut lines = side.lines();
            let _orig = lines.next();
            let deleted = lines
                .next()
                .and_then(|l| Timestamp::parse_rfc3339(l).ok())
                .unwrap_or(Timestamp(0));
            let display = lines.next().unwrap_or("").to_owned();
            out.push(TrashEntry {
                id,
                title_or_first_line: display,
                deleted,
            });
        }
    }
    out.sort_by_key(|e| std::cmp::Reverse((e.deleted, e.id)));
    out
}

/// Restore a note from trash to its original directory.
///
/// pub(crate): callable only via the vault worker's `VaultOp`s — enforced structurally since WP1e.
pub(crate) fn restore(vault: &Vault, id: NoteId) -> Result<PathBuf, IoError> {
    let dir = trash_dir(vault);
    let side_path = dir.join(format!("{id}.meta"));
    let side = std::fs::read_to_string(&side_path)
        .map_err(IoError::wrap("read trash entry", &side_path))?;
    let orig_rel = PathBuf::from(side.lines().next().unwrap_or_default());
    let orig_dir = orig_rel.parent().unwrap_or_else(|| Path::new("notes"));
    let stem = orig_rel
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("Untitled");
    let dst_abs = filename_for(stem, id, &vault.abs(orig_dir));
    let src = dir.join(format!("{id}.md"));
    std::fs::rename(&src, &dst_abs).map_err(IoError::wrap("restore from trash", &src))?;
    // Best-effort: a stale .meta (if this remove fails) lists a ghost entry,
    // but the restored note itself is already safe at its destination.
    let _ = std::fs::remove_file(&side_path);
    Ok(vault.rel(&dst_abs).unwrap_or(orig_rel))
}

/// Purge trash entries older than `days` days. `None` means manual-only (never purge).
/// Returns how many notes were purged.
///
/// pub(crate): callable only via the vault worker's `VaultOp`s — enforced structurally since WP1e.
pub(crate) fn purge_older_than(vault: &Vault, days: Option<u32>) -> Result<usize, IoError> {
    let Some(days) = days else { return Ok(0) };
    let cutoff = Timestamp(Timestamp::now().0 - i64::from(days) * 86_400_000);
    let mut purged = 0;
    let dir = trash_dir(vault);
    for entry in list_trash(vault) {
        if entry.deleted <= cutoff {
            let _ = std::fs::remove_file(dir.join(format!("{}.md", entry.id)));
            let _ = std::fs::remove_file(dir.join(format!("{}.meta", entry.id)));
            purged += 1;
        }
    }
    Ok(purged)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::note::{Kind, NoteMeta, Status};
    use crate::time::Timestamp;
    use crate::vault::testutil::TempDir;

    fn meta_for(v: &Vault, rel: &str, body: &str) -> NoteMeta {
        std::fs::write(v.root().join(rel), body).unwrap();
        let id = crate::vault::scan::synthetic_id(std::path::Path::new(rel));
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
}
