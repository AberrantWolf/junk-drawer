//! The vault: one folder the user picks; `inbox/` + `notes/` are workflow
//! state made visible; `.junkdrawer/` is disposable machine state (spec §2).

// submodules land one per task: io (T2), scan (T3), trash+recovery (T4), watcher (T5)

pub mod io;
pub mod recovery;
pub mod scan;
pub mod trash;
pub mod watcher;

use std::path::{Path, PathBuf};

use crate::error::{IoError, VaultError};

pub struct Vault {
    root: PathBuf,
}

impl Vault {
    /// Creates the vault layout as needed; never touches existing notes.
    pub fn open(root: &Path) -> Result<Vault, VaultError> {
        if root.exists() && !root.is_dir() {
            return Err(VaultError::NotADirectory(root.to_owned()));
        }
        for sub in [
            "inbox",
            "notes",
            ".junkdrawer/trash",
            ".junkdrawer/recovery",
            ".junkdrawer/session",
        ] {
            let dir = root.join(sub);
            std::fs::create_dir_all(&dir)
                .map_err(IoError::wrap("create folder", &dir))
                .map_err(VaultError::Io)?;
        }
        Ok(Vault {
            root: root.to_owned(),
        })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn abs(&self, rel: &Path) -> PathBuf {
        self.root.join(rel)
    }

    /// Inverse of `abs`: None if the path isn't under this vault.
    pub fn rel(&self, abs: &Path) -> Option<PathBuf> {
        abs.strip_prefix(&self.root).ok().map(Path::to_owned)
    }
}

/// In-crate TempDir twin for unit tests (integration tests keep their own copy
/// in tests/common/mod.rs — Rust can't share across the crate boundary).
#[cfg(test)]
pub(crate) mod testutil {
    use std::path::Path;
    use std::sync::atomic::{AtomicU32, Ordering};

    pub struct TempDir(pub std::path::PathBuf);

    impl TempDir {
        pub fn new() -> TempDir {
            static N: AtomicU32 = AtomicU32::new(0);
            let p = std::env::temp_dir().join(format!(
                "jd-unit-{}-{}",
                std::process::id(),
                N.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir_all(&p).unwrap();
            TempDir(p)
        }

        pub fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TempDir(std::path::PathBuf);
    impl TempDir {
        fn new() -> TempDir {
            use std::sync::atomic::{AtomicU32, Ordering};
            static N: AtomicU32 = AtomicU32::new(0);
            let p = std::env::temp_dir().join(format!(
                "jd-vault-test-{}-{}",
                std::process::id(),
                N.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir_all(&p).unwrap();
            TempDir(p)
        }
    }
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn open_creates_the_layout() {
        let t = TempDir::new();
        let v = Vault::open(&t.0).unwrap();
        for sub in [
            "inbox",
            "notes",
            ".junkdrawer/trash",
            ".junkdrawer/recovery",
            ".junkdrawer/session",
        ] {
            assert!(t.0.join(sub).is_dir(), "{sub} missing");
        }
        assert_eq!(v.root(), t.0.as_path());
    }

    #[test]
    fn open_is_idempotent_and_preserves_content() {
        let t = TempDir::new();
        Vault::open(&t.0).unwrap();
        std::fs::write(t.0.join("notes/existing.md"), "# Keep me\n").unwrap();
        Vault::open(&t.0).unwrap();
        assert_eq!(
            std::fs::read_to_string(t.0.join("notes/existing.md")).unwrap(),
            "# Keep me\n"
        );
    }

    #[test]
    fn open_rejects_a_file_path() {
        let t = TempDir::new();
        let f = t.0.join("a-file");
        std::fs::write(&f, "x").unwrap();
        assert!(matches!(Vault::open(&f), Err(VaultError::NotADirectory(_))));
    }

    #[test]
    fn abs_and_rel_are_inverses() {
        let t = TempDir::new();
        let v = Vault::open(&t.0).unwrap();
        let rel = std::path::Path::new("notes/x.md");
        let abs = v.abs(rel);
        assert!(abs.starts_with(v.root()));
        assert_eq!(v.rel(&abs).unwrap(), rel);
        assert_eq!(v.rel(std::path::Path::new("/elsewhere/x.md")), None);
    }

    #[test]
    fn errors_render_human_sentences() {
        let e = crate::error::IoError {
            path: "notes/x.md".into(),
            op: "save",
            source: std::io::Error::new(std::io::ErrorKind::PermissionDenied, "denied"),
        };
        let msg = e.to_string();
        assert!(
            msg.contains("save") && msg.contains("notes/x.md"),
            "unhelpful: {msg}"
        );
    }
}
