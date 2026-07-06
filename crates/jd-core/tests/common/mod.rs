//! Shared test helpers. `tempfile` is a rejected dependency (Appendix B);
//! this is the ~25-line in-house version.
#![allow(dead_code)] // not every test file uses every helper

use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};

pub struct TempDir(pub PathBuf);

impl TempDir {
    pub fn new() -> TempDir {
        static N: AtomicU32 = AtomicU32::new(0);
        let p = std::env::temp_dir().join(format!(
            "jd-it-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&p).unwrap();
        TempDir(p)
    }

    pub fn path(&self) -> &std::path::Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
