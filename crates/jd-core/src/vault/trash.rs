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

pub fn trash_note(vault: &Vault, meta: &NoteMeta) -> Result<(), IoError> {
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

pub fn restore(vault: &Vault, id: NoteId) -> Result<PathBuf, IoError> {
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
    let _ = std::fs::remove_file(&side_path);
    Ok(vault.rel(&dst_abs).unwrap_or(orig_rel))
}

/// None = manual only (never purge). Returns how many notes were purged.
pub fn purge_older_than(vault: &Vault, days: Option<u32>) -> Result<usize, IoError> {
    let Some(days) = days else { return Ok(0) };
    let cutoff = Timestamp(Timestamp::now().0 - i64::from(days) * 86_400_000);
    let mut purged = 0;
    for entry in list_trash(vault) {
        if entry.deleted <= cutoff {
            let dir = trash_dir(vault);
            let _ = std::fs::remove_file(dir.join(format!("{}.md", entry.id)));
            let _ = std::fs::remove_file(dir.join(format!("{}.meta", entry.id)));
            purged += 1;
        }
    }
    Ok(purged)
}
