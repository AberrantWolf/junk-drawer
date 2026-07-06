//! Typed errors rendering human sentences with context (spec §3 error
//! posture). No `anyhow`, no `Box<dyn Error>` in public APIs.

use std::fmt;
use std::path::PathBuf;

use crate::frontmatter::FmError;
use crate::id::IdError;
use crate::time::TimeError;

/// A filesystem operation that failed, with enough context for the UI to
/// render "Couldn't save 'x' — permission denied. [Retry]".
#[derive(Debug)]
pub struct IoError {
    pub path: PathBuf,
    /// Verb phrase: "save", "read", "move to trash", …
    pub op: &'static str,
    pub source: std::io::Error,
}

impl fmt::Display for IoError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "couldn't {} '{}': {}",
            self.op,
            self.path.display(),
            self.source
        )
    }
}

impl IoError {
    pub(crate) fn wrap<'a>(
        op: &'static str,
        path: &'a std::path::Path,
    ) -> impl FnOnce(std::io::Error) -> IoError + 'a {
        move |source| IoError {
            path: path.to_owned(),
            op,
            source,
        }
    }
}

#[derive(Debug)]
pub enum VaultError {
    NotADirectory(PathBuf),
    Io(IoError),
}

impl fmt::Display for VaultError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            VaultError::NotADirectory(p) => write!(f, "'{}' isn't a folder", p.display()),
            VaultError::Io(e) => e.fmt(f),
        }
    }
}

#[derive(Debug)]
pub enum WatchError {
    Init(String),
}

impl fmt::Display for WatchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            WatchError::Init(s) => write!(f, "couldn't watch the vault for changes: {s}"),
        }
    }
}

#[derive(Debug)]
pub enum CoreError {
    Io(IoError),
    Vault(VaultError),
    Watch(WatchError),
    Parse(FmError),
    Time(TimeError),
    Id(IdError),
}

impl fmt::Display for CoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CoreError::Io(e) => e.fmt(f),
            CoreError::Vault(e) => e.fmt(f),
            CoreError::Watch(e) => e.fmt(f),
            CoreError::Parse(e) => write!(f, "couldn't read the note's header: {e:?}"),
            CoreError::Time(e) => write!(f, "couldn't read a timestamp: {e:?}"),
            CoreError::Id(e) => write!(f, "couldn't read a note id: {e:?}"),
        }
    }
}

impl From<IoError> for CoreError {
    fn from(e: IoError) -> Self {
        CoreError::Io(e)
    }
}
impl From<VaultError> for CoreError {
    fn from(e: VaultError) -> Self {
        CoreError::Vault(e)
    }
}
impl From<WatchError> for CoreError {
    fn from(e: WatchError) -> Self {
        CoreError::Watch(e)
    }
}
