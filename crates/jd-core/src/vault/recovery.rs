//! .junkdrawer/recovery/: journaled unsaved buffers so a crash loses nothing,
//! including the autosave debounce window (spec §3).

use std::path::{Path, PathBuf};

use crate::error::IoError;
use crate::id::NoteId;
use crate::vault::Vault;
use crate::vault::io::atomic_save;

fn buffer_path(vault: &Vault, id: NoteId) -> PathBuf {
    vault
        .abs(Path::new(".junkdrawer/recovery"))
        .join(format!("{id}.md"))
}

pub fn journal_buffer(vault: &Vault, id: NoteId, content: &str) -> Result<(), IoError> {
    atomic_save(&buffer_path(vault, id), content)
}

pub fn clear_buffer(vault: &Vault, id: NoteId) {
    let _ = std::fs::remove_file(buffer_path(vault, id));
}

/// Checked at startup: buffers that outlived their session.
pub fn pending_recoveries(vault: &Vault) -> Vec<(NoteId, String)> {
    let dir = vault.abs(Path::new(".junkdrawer/recovery"));
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return out;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        let Ok(id) = NoteId::parse(stem) else {
            continue;
        };
        if let Ok(content) = std::fs::read_to_string(&path) {
            out.push((id, content));
        }
    }
    out.sort_by_key(|(id, _)| *id);
    out
}
