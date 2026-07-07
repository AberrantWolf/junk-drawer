//! Parallel startup scan (spec §3): the index is rebuilt from disk truth.
//! A file that fails to READ is quarantined; file CONTENT never fails
//! (NoteDoc::parse is infallible).

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use crate::doc::NoteDoc;
use crate::error::{IoError, VaultError};
use crate::id::NoteId;
use crate::note::NoteMeta;
use crate::time::Timestamp;
use crate::vault::Vault;
use crate::vault::io::is_our_tempfile;

pub struct QuarantinedFile {
    pub rel_path: PathBuf,
    pub error: String,
}

pub struct ScanOutcome {
    /// Meta + body per note; the caller feeds these to Index::upsert and
    /// drops the bodies (they are never retained — spec §3).
    pub metas: Vec<(NoteMeta, String)>,
    pub quarantined: Vec<QuarantinedFile>,
}

/// Deterministic ID for files without a frontmatter id (decision #1):
/// 128-bit FNV-1a over the rel path, two offset bases. Stable across
/// rescans; becomes persistent when the worker first rewrites frontmatter.
///
/// The path is normalized to forward-slash separators before hashing, so
/// the same note in a synced vault produces the same id on all platforms
/// (Windows `inbox\scrap.md` and UNIX `inbox/scrap.md` are identical).
pub fn synthetic_id(rel: &Path) -> NoteId {
    fn fnv64(bytes: &[u8], mut hash: u64) -> u64 {
        for &b in bytes {
            hash ^= b as u64;
            hash = hash.wrapping_mul(0x0000_0100_0000_01B3);
        }
        hash
    }
    let s = rel
        .components()
        .map(|c| c.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/");
    let a = fnv64(s.as_bytes(), 0xcbf2_9ce4_8422_2325);
    let b = fnv64(s.as_bytes(), 0x9e37_79b9_7f4a_7c15);
    let mut bytes = [0u8; 16];
    bytes[..8].copy_from_slice(&a.to_be_bytes());
    bytes[8..].copy_from_slice(&b.to_be_bytes());
    NoteId(bytes)
}

/// Rel paths of every note file under inbox/ and notes/, recursive,
/// skipping dot-files, our temp files, and non-.md files.
pub(crate) fn note_files(vault: &Vault) -> Result<Vec<PathBuf>, VaultError> {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) -> std::io::Result<()> {
        for entry in std::fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') || is_our_tempfile(&name) {
                continue;
            }
            if path.is_dir() {
                walk(&path, out)?;
            } else if path.extension().is_some_and(|e| e == "md") {
                out.push(path);
            }
        }
        Ok(())
    }
    let mut abs = Vec::new();
    for top in ["inbox", "notes"] {
        let dir = vault.abs(Path::new(top));
        walk(&dir, &mut abs)
            .map_err(IoError::wrap("scan", &dir))
            .map_err(VaultError::Io)?;
    }
    Ok(abs.into_iter().filter_map(|p| vault.rel(&p)).collect())
}

/// Read + parse one note. Err(reason) means unreadable → quarantine.
///
/// Read-only: safe from any thread; mutation stays worker-only.
pub fn parse_note_file(vault: &Vault, rel: &Path) -> Result<(NoteMeta, String), String> {
    let abs = vault.abs(rel);
    let src = std::fs::read_to_string(&abs).map_err(|e| e.to_string())?;
    let fs_modified = abs
        .metadata()
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| Timestamp(d.as_millis() as i64))
        .unwrap_or_else(Timestamp::now);
    let doc = NoteDoc::parse(&src);
    let id = doc.fm.id().unwrap_or_else(|| synthetic_id(rel));
    let meta = doc.to_meta(id, rel, fs_modified);
    Ok((meta, doc.body))
}

pub fn scan(
    vault: &Vault,
    progress: &(dyn Fn(usize, usize) + Sync),
) -> Result<ScanOutcome, VaultError> {
    let files = note_files(vault)?;
    let total = files.len();
    let done = &AtomicUsize::new(0);

    let workers = std::thread::available_parallelism().map_or(4, |n| n.get());
    let chunk_size = files.len().div_ceil(workers).max(1);

    // Convert into chunks as owned vectors to avoid borrow issues
    let chunks: Vec<Vec<PathBuf>> = files.chunks(chunk_size).map(|c| c.to_vec()).collect();

    let mut all_metas = Vec::with_capacity(total);
    let mut all_quarantined: Vec<QuarantinedFile> = Vec::new();

    std::thread::scope(|s| {
        let handles: Vec<_> = chunks
            .into_iter()
            .map(|chunk| {
                s.spawn(move || {
                    let mut local_metas: Vec<(NoteMeta, String)> = Vec::new();
                    let mut local_quarantined: Vec<QuarantinedFile> = Vec::new();
                    for rel in chunk {
                        match parse_note_file(vault, &rel) {
                            Ok(pair) => local_metas.push(pair),
                            Err(error) => local_quarantined.push(QuarantinedFile {
                                rel_path: rel,
                                error,
                            }),
                        }
                        progress(done.fetch_add(1, Ordering::Relaxed) + 1, total);
                    }
                    (local_metas, local_quarantined)
                })
            })
            .collect();

        for handle in handles {
            let (m, q) = handle.join().expect("scan worker panicked");
            all_metas.extend(m);
            all_quarantined.extend(q);
        }
    });

    Ok(ScanOutcome {
        metas: all_metas,
        quarantined: all_quarantined,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn synthetic_id_is_separator_independent() {
        use std::path::PathBuf;
        let forward = synthetic_id(std::path::Path::new("inbox/scrap.md"));
        let joined = synthetic_id(&PathBuf::from("inbox").join("scrap.md"));
        assert_eq!(forward, joined);
    }
}
