//! Note domain types. Lifecycle (`Status`) and what-it-is (`Kind`) are
//! orthogonal axes (spec §2). Bodies never live in these types — the index
//! holds metadata only.

use std::collections::BTreeSet;
use std::ops::Range;
use std::path::PathBuf;

use crate::id::NoteId;
use crate::tag::Tag;
use crate::time::Timestamp;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Status {
    Fleeting,
    Permanent,
}

impl Status {
    pub fn as_str(&self) -> &'static str {
        match self {
            Status::Fleeting => "fleeting",
            Status::Permanent => "permanent",
        }
    }

    pub fn parse(s: &str) -> Option<Status> {
        match s.to_ascii_lowercase().as_str() {
            "fleeting" => Some(Status::Fleeting),
            "permanent" => Some(Status::Permanent),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Kind {
    #[default]
    Note,
    Literature,
    Structure,
}

impl Kind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Kind::Note => "note",
            Kind::Literature => "literature",
            Kind::Structure => "structure",
        }
    }

    pub fn parse(s: &str) -> Option<Kind> {
        match s.to_ascii_lowercase().as_str() {
            "note" => Some(Kind::Note),
            "literature" => Some(Kind::Literature),
            "structure" => Some(Kind::Structure),
            _ => None,
        }
    }
}

/// A `[[wikilink]]` occurrence in a body.
#[derive(Clone, Debug, PartialEq)]
pub struct LinkRef {
    /// Raw title text inside the brackets, pipe part excluded.
    pub target: String,
    /// Text after `|`, if any.
    pub display: Option<String>,
    /// Byte range in the body, including the brackets.
    pub span: Range<usize>,
}

/// Everything the index holds about a note (spec §3: bodies are NOT here).
#[derive(Clone, Debug)]
pub struct NoteMeta {
    pub id: NoteId,
    /// Relative to the vault root, e.g. "notes/Egui tradeoffs.md".
    pub rel_path: PathBuf,
    /// First `# ` heading in the body; None for untitled scraps.
    pub title: Option<String>,
    /// First non-empty body line — scrap display and a11y announcements.
    pub first_line: String,
    pub status: Status,
    pub kind: Kind,
    pub source: Option<String>,
    pub created: Timestamp,
    pub modified: Timestamp,
    /// Union of the frontmatter list and #inline-tags.
    pub tags: BTreeSet<Tag>,
    pub links_out: Vec<LinkRef>,
    pub word_count: u32,
}

/// Seed for creating a note (capture paths, palette "New scrap", split).
#[derive(Clone, Debug)]
pub struct NewNote {
    pub body: String,
    pub status: Status,
    pub kind: Kind,
    pub source: Option<String>,
    pub tags: Vec<Tag>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_round_trips() {
        assert_eq!(Status::parse("fleeting"), Some(Status::Fleeting));
        assert_eq!(Status::parse("permanent"), Some(Status::Permanent));
        assert_eq!(Status::parse("Fleeting"), Some(Status::Fleeting)); // case-insensitive
        assert_eq!(Status::parse("draft"), None);
        assert_eq!(Status::Fleeting.as_str(), "fleeting");
        assert_eq!(Status::Permanent.as_str(), "permanent");
    }

    #[test]
    fn kind_round_trips_and_defaults() {
        assert_eq!(Kind::parse("note"), Some(Kind::Note));
        assert_eq!(Kind::parse("literature"), Some(Kind::Literature));
        assert_eq!(Kind::parse("structure"), Some(Kind::Structure));
        assert_eq!(Kind::parse("recipe"), None);
        assert_eq!(Kind::default(), Kind::Note);
        assert_eq!(Kind::Literature.as_str(), "literature");
    }
}
