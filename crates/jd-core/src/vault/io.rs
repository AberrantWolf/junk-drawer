//! Atomic writes and filename rules (spec §2, §3). Saves are temp + fsync +
//! rename; a crash at any point leaves the original intact (torture-tested).

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use crate::error::IoError;
use crate::id::NoteId;

const MAX_FILENAME_BYTES: usize = 120;
const FORBIDDEN: &[char] = &['/', '\\', ':', '*', '?', '"', '<', '>', '|'];

pub fn atomic_save(abs_path: &Path, content: &str) -> Result<(), IoError> {
    atomic_save_with(abs_path, content, &|_| Ok(()))
}

/// Failpoint-injectable core: `checkpoint("written")` fires after the temp
/// file is written and synced; `checkpoint("renamed")` after the rename.
/// Tests inject failures to simulate crashes between phases.
pub fn atomic_save_with(
    abs_path: &Path,
    content: &str,
    checkpoint: &dyn Fn(&str) -> std::io::Result<()>,
) -> Result<(), IoError> {
    let dir = abs_path.parent().unwrap_or_else(|| Path::new("."));
    let name = abs_path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let tmp = dir.join(format!(".{name}.jd-tmp"));
    let wrap = |op| IoError::wrap(op, abs_path);

    let mut f = fs::File::create(&tmp).map_err(wrap("save"))?;
    f.write_all(content.as_bytes()).map_err(wrap("save"))?;
    f.sync_all().map_err(wrap("save"))?;
    drop(f);
    checkpoint("written").map_err(wrap("save"))?;

    fs::rename(&tmp, abs_path).map_err(wrap("save"))?;
    #[cfg(unix)]
    {
        if let Ok(d) = fs::File::open(dir) {
            let _ = d.sync_all(); // best-effort directory durability
        }
    }
    checkpoint("renamed").map_err(wrap("save"))?;
    Ok(())
}

/// True for our own temp files — the watcher must ignore them.
pub fn is_our_tempfile(file_name: &str) -> bool {
    file_name.starts_with('.') && file_name.ends_with(".jd-tmp")
}

/// Strip path-hostile characters and control chars, trim dots/spaces,
/// cap at a char boundary. Empty results become "Untitled".
pub fn sanitize_filename(title: &str) -> String {
    let mut s: String = title
        .chars()
        .filter(|c| !FORBIDDEN.contains(c) && !c.is_control())
        .collect();
    s = s.trim().trim_end_matches(['.', ' ']).trim().to_owned();
    if s.len() > MAX_FILENAME_BYTES {
        let mut cut = MAX_FILENAME_BYTES;
        while !s.is_char_boundary(cut) {
            cut -= 1;
        }
        s.truncate(cut);
    }
    if s.is_empty() {
        "Untitled".to_owned()
    } else {
        s
    }
}

/// "Title.md", or "Title (01J8ZQ4K).md" when a different file already holds
/// the name (spec §2 collision rule).
pub fn filename_for(title: &str, id: NoteId, dir: &Path) -> PathBuf {
    let base = sanitize_filename(title);
    let plain = dir.join(format!("{base}.md"));
    if !plain.exists() {
        return plain;
    }
    dir.join(format!("{base} ({}).md", id.short()))
}
