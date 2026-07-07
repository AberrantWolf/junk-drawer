//! Atomic writes and filename rules (spec §2, §3). Saves are temp + fsync +
//! rename; a crash at any point leaves the original intact (torture-tested).

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use crate::error::IoError;
use crate::id::NoteId;

const MAX_FILENAME_BYTES: usize = 120;
const FORBIDDEN: &[char] = &['/', '\\', ':', '*', '?', '"', '<', '>', '|'];

/// Test-only global failpoint for `atomic_save` (WP3 Task 1 seam).
///
/// Encoding: one atomic packs two u16 fields, `(skip << 16) | fail`.
///
/// API:
/// - `arm(skip, fail)` — let the next `skip` saves through, then fail the
///   next `fail` saves; after that, everything passes again.
/// - `disarm()` — reset to 0 (pass everything).
///
/// This is the narrowest seam consistent with the WP1d `atomic_save_with`
/// checkpoint pattern.  It lives entirely inside this file and is compiled
/// only under `cfg(test)` or the `test-failpoints` feature (enabled for this
/// crate's own tests via the self dev-dependency in Cargo.toml — never in
/// production builds).  Tests use it to force mid-batch or mid-loop rollback
/// failures in the worker.
///
/// **Concurrency note**: the counter is process-global, so tests that arm it
/// must not run concurrently with other saves; serialize such tests with a
/// shared mutex in the test binary.
#[cfg(any(test, feature = "test-failpoints"))]
pub mod failpoint {
    use std::sync::atomic::{AtomicI32, Ordering};

    /// skip (high 16) | fail (low 16) — both stored as i32 halves.
    static SAVE_FAILPOINT: AtomicI32 = AtomicI32::new(0);

    /// Arm: skip the next `skip` saves, then fail the next `fail` saves.
    pub fn arm(skip: u16, fail: u16) {
        SAVE_FAILPOINT.store(((skip as i32) << 16) | (fail as i32), Ordering::SeqCst);
    }

    /// Disarm: all subsequent saves pass through.
    pub fn disarm() {
        SAVE_FAILPOINT.store(0, Ordering::SeqCst);
    }

    pub(crate) fn check() -> std::io::Result<()> {
        loop {
            let v = SAVE_FAILPOINT.load(Ordering::SeqCst);
            let skip = (v >> 16) as u16;
            let fail = (v & 0xFFFF) as u16;

            if skip == 0 && fail == 0 {
                return Ok(());
            }

            let new_v = if skip > 0 {
                // Still skipping: decrement skip, keep fail
                (((skip - 1) as i32) << 16) | (fail as i32)
            } else {
                // Failing: skip=0, decrement fail
                fail as i32 - 1
            };

            match SAVE_FAILPOINT.compare_exchange(v, new_v, Ordering::SeqCst, Ordering::SeqCst) {
                Ok(_) => {
                    if skip > 0 {
                        return Ok(()); // this was a skip
                    } else {
                        return Err(std::io::Error::other("atomic_save failpoint triggered"));
                    }
                }
                Err(_) => continue, // raced; retry
            }
        }
    }
}

pub fn atomic_save(abs_path: &Path, content: &str) -> Result<(), IoError> {
    #[cfg(any(test, feature = "test-failpoints"))]
    if let Err(e) = failpoint::check() {
        return Err(IoError {
            path: abs_path.to_owned(),
            op: "save",
            source: e,
        });
    }
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
