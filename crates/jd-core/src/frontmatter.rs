//! Fixed-schema frontmatter with byte-identity round-trips (spec §2).
//! Mechanism: every original line is kept raw (terminator included); known
//! keys are tagged; accessors re-parse tagged lines on demand; setters
//! (Task 7) rewrite only their own line. `serialize` concatenates raw lines.

use crate::id::NoteId;
use crate::note::{Kind, Status};
use crate::tag::Tag;
use crate::time::Timestamp;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum KnownKey {
    Id,
    Created,
    Modified,
    Status,
    Kind,
    Source,
    Tags,
}

impl KnownKey {
    fn from_name(name: &str) -> Option<KnownKey> {
        match name {
            "id" => Some(KnownKey::Id),
            "created" => Some(KnownKey::Created),
            "modified" => Some(KnownKey::Modified),
            "status" => Some(KnownKey::Status),
            "kind" => Some(KnownKey::Kind),
            "source" => Some(KnownKey::Source),
            "tags" => Some(KnownKey::Tags),
            _ => None,
        }
    }

    pub(crate) fn name(&self) -> &'static str {
        match self {
            KnownKey::Id => "id",
            KnownKey::Created => "created",
            KnownKey::Modified => "modified",
            KnownKey::Status => "status",
            KnownKey::Kind => "kind",
            KnownKey::Source => "source",
            KnownKey::Tags => "tags",
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) enum LineRole {
    Marker,                 // the --- lines
    Key(KnownKey),          // a recognized `key: value` line
    Continuation(KnownKey), // `- item` under a known key with empty value
    Other,                  // unknown key, comment, anything — preserved raw
}

#[derive(Clone, Debug)]
pub(crate) struct FmLine {
    /// Full original line INCLUDING its terminator (\n or \r\n; last line may have none).
    pub(crate) raw: String,
    pub(crate) role: LineRole,
}

#[derive(Debug, PartialEq, Eq)]
pub enum FmError {
    NoOpeningMarker,
    Unterminated,
}

#[derive(Clone, Debug)]
pub struct FrontmatterDoc {
    pub(crate) lines: Vec<FmLine>, // covers opening marker..closing marker inclusive; empty = no block
}

/// Split into lines, each keeping its terminator.
fn lines_inclusive(s: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0;
    for (i, b) in s.bytes().enumerate() {
        if b == b'\n' {
            out.push(&s[start..=i]);
            start = i + 1;
        }
    }
    if start < s.len() {
        out.push(&s[start..]);
    }
    out
}

/// The line's content without its terminator.
fn content(raw: &str) -> &str {
    raw.trim_end_matches('\n').trim_end_matches('\r')
}

/// Strip one matching pair of single or double quotes.
fn unquote(s: &str) -> &str {
    let s = s.trim();
    if s.len() >= 2
        && ((s.starts_with('"') && s.ends_with('"')) || (s.starts_with('\'') && s.ends_with('\'')))
    {
        &s[1..s.len() - 1]
    } else {
        s
    }
}

/// `key: value` → (key, value) if key is a plain identifier at column 0.
fn split_key_line(line: &str) -> Option<(&str, &str)> {
    let colon = line.find(':')?;
    let key = &line[..colon];
    if key.is_empty()
        || !key.chars().next().unwrap().is_ascii_alphabetic()
        || !key
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return None;
    }
    Some((key, line[colon + 1..].trim()))
}

impl FrontmatterDoc {
    pub fn empty() -> FrontmatterDoc {
        FrontmatterDoc { lines: Vec::new() }
    }

    pub fn parse(input: &str) -> Result<(FrontmatterDoc, usize), FmError> {
        let all = lines_inclusive(input);
        let first = all.first().ok_or(FmError::NoOpeningMarker)?;
        if content(first) != "---" {
            return Err(FmError::NoOpeningMarker);
        }

        let mut lines = vec![FmLine {
            raw: first.to_string(),
            role: LineRole::Marker,
        }];
        let mut consumed = first.len();
        let mut open_list_key: Option<KnownKey> = None;
        let mut closed = false;

        for raw in &all[1..] {
            let c = content(raw);
            consumed += raw.len();
            if c == "---" {
                lines.push(FmLine {
                    raw: raw.to_string(),
                    role: LineRole::Marker,
                });
                closed = true;
                break;
            }
            let role = if let Some(key) = open_list_key.filter(|_| c.trim_start().starts_with("- "))
            {
                LineRole::Continuation(key)
            } else if let Some((name, value)) = split_key_line(c) {
                match KnownKey::from_name(name) {
                    Some(k) => {
                        open_list_key = (k == KnownKey::Tags && value.is_empty()).then_some(k);
                        LineRole::Key(k)
                    }
                    None => {
                        open_list_key = None;
                        LineRole::Other
                    }
                }
            } else {
                if !c.trim_start().starts_with("- ") {
                    open_list_key = None;
                }
                LineRole::Other
            };
            lines.push(FmLine {
                raw: raw.to_string(),
                role,
            });
        }

        if !closed {
            return Err(FmError::Unterminated);
        }
        Ok((FrontmatterDoc { lines }, consumed))
    }

    pub fn synthesize(id: NoteId, created: Timestamp, status: Status) -> FrontmatterDoc {
        let text = format!(
            "---\nid: {id}\ncreated: {c}\nmodified: {c}\nstatus: {s}\n---\n",
            c = created.to_rfc3339(),
            s = status.as_str(),
        );
        FrontmatterDoc::parse(&text)
            .expect("synthesized block always parses")
            .0
    }

    pub fn serialize(&self) -> String {
        self.lines.iter().map(|l| l.raw.as_str()).collect()
    }

    /// The unquoted scalar value of a known key's line, if present.
    fn value_of(&self, key: KnownKey) -> Option<String> {
        self.lines.iter().find_map(|l| match l.role {
            LineRole::Key(k) if k == key => {
                let (_, v) = split_key_line(content(&l.raw))?;
                Some(unquote(v).to_owned())
            }
            _ => None,
        })
    }

    pub fn id(&self) -> Option<NoteId> {
        NoteId::parse(&self.value_of(KnownKey::Id)?).ok()
    }

    pub fn created(&self) -> Option<Timestamp> {
        Timestamp::parse_rfc3339(&self.value_of(KnownKey::Created)?).ok()
    }

    pub fn modified(&self) -> Option<Timestamp> {
        Timestamp::parse_rfc3339(&self.value_of(KnownKey::Modified)?).ok()
    }

    pub fn status(&self) -> Option<Status> {
        Status::parse(&self.value_of(KnownKey::Status)?)
    }

    /// Absent or unrecognized means `Kind::Note` (spec §2).
    pub fn kind(&self) -> Kind {
        self.value_of(KnownKey::Kind)
            .and_then(|v| Kind::parse(&v))
            .unwrap_or_default()
    }

    pub fn source(&self) -> Option<String> {
        self.value_of(KnownKey::Source).filter(|s| !s.is_empty())
    }

    pub fn tags(&self) -> Vec<Tag> {
        // inline form: tags: [a, b] — or a bare scalar for a single tag
        if let Some(v) = self.value_of(KnownKey::Tags)
            && !v.is_empty()
        {
            let inner = v
                .strip_prefix('[')
                .and_then(|s| s.strip_suffix(']'))
                .unwrap_or(&v);
            return inner
                .split(',')
                .filter_map(|item| Tag::new(unquote(item)))
                .collect();
        }
        // block form: continuation lines "- item"
        self.lines
            .iter()
            .filter_map(|l| match l.role {
                LineRole::Continuation(KnownKey::Tags) => {
                    let c = content(&l.raw).trim_start();
                    Tag::new(unquote(c.strip_prefix("- ")?))
                }
                _ => None,
            })
            .collect()
    }

    /// The dominant terminator for appended lines (borrow the closing marker's).
    fn block_terminator(&self) -> &'static str {
        match self.lines.last().map(|l| l.raw.ends_with("\r\n")) {
            Some(true) => "\r\n",
            _ => "\n",
        }
    }

    /// Terminator of an existing line, defaulting to the block's.
    fn terminator_of(raw: &str, fallback: &'static str) -> &'static str {
        if raw.ends_with("\r\n") {
            "\r\n"
        } else if raw.ends_with('\n') {
            "\n"
        } else {
            fallback
        }
    }

    /// Rewrite key's line with `key: value`, or insert before the closing marker.
    /// `value = None` removes the line. Continuation lines of that key are always removed.
    fn set_raw(&mut self, key: KnownKey, value: Option<String>) {
        assert!(
            !self.lines.is_empty(),
            "cannot set fields on an empty frontmatter block"
        );
        let fallback = self.block_terminator();
        self.lines
            .retain(|l| !matches!(l.role, LineRole::Continuation(k) if k == key));
        let existing = self
            .lines
            .iter()
            .position(|l| matches!(l.role, LineRole::Key(k) if k == key));
        match (existing, value) {
            (Some(i), Some(v)) => {
                let term = Self::terminator_of(&self.lines[i].raw, fallback);
                self.lines[i].raw = format!("{}: {}{}", key.name(), v, term);
            }
            (Some(i), None) => {
                self.lines.remove(i);
            }
            (None, Some(v)) => {
                let closing = self.lines.len() - 1; // the closing marker
                self.lines.insert(
                    closing,
                    FmLine {
                        raw: format!("{}: {}{}", key.name(), v, fallback),
                        role: LineRole::Key(key),
                    },
                );
            }
            (None, None) => {}
        }
    }

    pub fn set_status(&mut self, s: Status) {
        self.set_raw(KnownKey::Status, Some(s.as_str().to_owned()));
    }

    pub fn set_kind(&mut self, k: Kind) {
        let v = (k != Kind::Note).then(|| k.as_str().to_owned());
        self.set_raw(KnownKey::Kind, v);
    }

    pub fn set_source(&mut self, src: Option<&str>) {
        self.set_raw(
            KnownKey::Source,
            src.map(|s| format!("\"{}\"", s.replace('"', "'"))),
        );
    }

    pub fn set_modified(&mut self, t: Timestamp) {
        self.set_raw(KnownKey::Modified, Some(t.to_rfc3339()));
    }

    pub fn set_tags(&mut self, tags: &[Tag]) {
        if tags.is_empty() {
            self.set_raw(KnownKey::Tags, None);
        } else {
            let list = tags.iter().map(Tag::as_str).collect::<Vec<_>>().join(", ");
            self.set_raw(KnownKey::Tags, Some(format!("[{list}]")));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::note::{Kind, Status};

    const SPEC_EXAMPLE: &str = "---\n\
id: 01J8ZQ4KF3T9M2X7C5VBNAE8RD\n\
created: 2026-07-03T10:22:00Z\n\
modified: 2026-07-04T09:10:00Z\n\
status: permanent\n\
kind: literature\n\
source: \"Ahrens, How to Take Smart Notes (2017)\"\n\
tags: [zettelkasten, method]\n\
---\n";

    #[test]
    fn parses_the_spec_example() {
        let (fm, consumed) = FrontmatterDoc::parse(SPEC_EXAMPLE).unwrap();
        assert_eq!(consumed, SPEC_EXAMPLE.len());
        assert_eq!(fm.id().unwrap().to_string(), "01J8ZQ4KF3T9M2X7C5VBNAE8RD");
        assert_eq!(fm.status(), Some(Status::Permanent));
        assert_eq!(fm.kind(), Kind::Literature);
        assert_eq!(
            fm.source().as_deref(),
            Some("Ahrens, How to Take Smart Notes (2017)")
        );
        assert_eq!(
            fm.tags()
                .iter()
                .map(|t| t.as_str().to_owned())
                .collect::<Vec<_>>(),
            vec!["zettelkasten", "method"]
        );
        assert_eq!(
            fm.created().unwrap(),
            crate::time::Timestamp::parse_rfc3339("2026-07-03T10:22:00Z").unwrap()
        );
    }

    #[test]
    fn serialize_is_byte_identical() {
        for input in [
            SPEC_EXAMPLE,
            "---\nid: 01J8ZQ4KF3T9M2X7C5VBNAE8RD\n---\n",
            // unknown keys, weird spacing, comments — all preserved verbatim
            "---\nid: 01J8ZQ4KF3T9M2X7C5VBNAE8RD\nobsidian-ui-mode: preview\naliases: [a, b]\n  weird indent line\n---\n",
            // CRLF terminators
            "---\r\nid: 01J8ZQ4KF3T9M2X7C5VBNAE8RD\r\nstatus: fleeting\r\n---\r\n",
            // single-quoted values, extra whitespace around colon
            "---\nsource: 'single quoted'\nstatus:   fleeting\n---\n",
        ] {
            let (fm, consumed) = FrontmatterDoc::parse(input).unwrap();
            assert_eq!(consumed, input.len());
            assert_eq!(fm.serialize(), input, "round-trip of {input:?}");
        }
    }

    #[test]
    fn consumed_stops_at_closing_marker() {
        let input = "---\nstatus: fleeting\n---\nBody text here.\n";
        let (fm, consumed) = FrontmatterDoc::parse(input).unwrap();
        assert_eq!(&input[consumed..], "Body text here.\n");
        assert_eq!(fm.serialize(), &input[..consumed]);
    }

    #[test]
    fn block_list_tags_parse() {
        let input = "---\ntags:\n  - zettelkasten\n  - method\n---\n";
        let (fm, _) = FrontmatterDoc::parse(input).unwrap();
        assert_eq!(
            fm.tags()
                .iter()
                .map(|t| t.as_str().to_owned())
                .collect::<Vec<_>>(),
            vec!["zettelkasten", "method"]
        );
        assert_eq!(fm.serialize(), input);
    }

    #[test]
    fn missing_and_absent_fields() {
        let (fm, _) = FrontmatterDoc::parse("---\nstatus: fleeting\n---\n").unwrap();
        assert_eq!(fm.id(), None);
        assert_eq!(fm.kind(), Kind::Note); // absent means note
        assert_eq!(fm.source(), None);
        assert!(fm.tags().is_empty());
        assert_eq!(fm.created(), None);
    }

    #[test]
    fn garbage_values_read_as_none_but_preserve() {
        let input = "---\nid: not-a-ulid\nstatus: draft\nkind: recipe\n---\n";
        let (fm, _) = FrontmatterDoc::parse(input).unwrap();
        assert_eq!(fm.id(), None);
        assert_eq!(fm.status(), None);
        assert_eq!(fm.kind(), Kind::Note);
        assert_eq!(fm.serialize(), input);
    }

    #[test]
    fn errors() {
        assert!(matches!(
            FrontmatterDoc::parse("# Just a body\n"),
            Err(FmError::NoOpeningMarker)
        ));
        assert!(matches!(
            FrontmatterDoc::parse("---\nstatus: fleeting\n"),
            Err(FmError::Unterminated)
        ));
    }

    #[test]
    fn empty_doc_serializes_to_nothing() {
        assert_eq!(FrontmatterDoc::empty().serialize(), "");
    }

    #[test]
    fn synthesize_produces_canonical_block() {
        let id = crate::id::NoteId::parse("01J8ZQ4KF3T9M2X7C5VBNAE8RD").unwrap();
        let t = crate::time::Timestamp::parse_rfc3339("2026-07-03T10:22:00Z").unwrap();
        let fm = FrontmatterDoc::synthesize(id, t, Status::Fleeting);
        assert_eq!(
            fm.serialize(),
            "---\n\
id: 01J8ZQ4KF3T9M2X7C5VBNAE8RD\n\
created: 2026-07-03T10:22:00Z\n\
modified: 2026-07-03T10:22:00Z\n\
status: fleeting\n\
---\n"
        );
    }

    const SETTER_BASE: &str = "---\n\
id: 01J8ZQ4KF3T9M2X7C5VBNAE8RD\n\
x-custom: keep me\n\
status: fleeting\n\
---\n";

    #[test]
    fn set_status_rewrites_only_its_line() {
        let (mut fm, _) = FrontmatterDoc::parse(SETTER_BASE).unwrap();
        fm.set_status(Status::Permanent);
        assert_eq!(
            fm.serialize(),
            "---\n\
id: 01J8ZQ4KF3T9M2X7C5VBNAE8RD\n\
x-custom: keep me\n\
status: permanent\n\
---\n"
        );
    }

    #[test]
    fn set_preserves_crlf_terminator_of_the_line() {
        let input = "---\r\nstatus: fleeting\r\n---\r\n";
        let (mut fm, _) = FrontmatterDoc::parse(input).unwrap();
        fm.set_status(Status::Permanent);
        assert_eq!(fm.serialize(), "---\r\nstatus: permanent\r\n---\r\n");
    }

    #[test]
    fn set_missing_key_appends_before_closing_marker() {
        let (mut fm, _) = FrontmatterDoc::parse(SETTER_BASE).unwrap();
        fm.set_source(Some("Ahrens (2017)"));
        assert_eq!(
            fm.serialize(),
            "---\n\
id: 01J8ZQ4KF3T9M2X7C5VBNAE8RD\n\
x-custom: keep me\n\
status: fleeting\n\
source: \"Ahrens (2017)\"\n\
---\n"
        );
    }

    #[test]
    fn set_kind_note_removes_the_line() {
        let input = "---\nkind: literature\nstatus: fleeting\n---\n";
        let (mut fm, _) = FrontmatterDoc::parse(input).unwrap();
        fm.set_kind(Kind::Note);
        assert_eq!(fm.serialize(), "---\nstatus: fleeting\n---\n");
        // and setting a non-default kind on a doc without the line adds it
        fm.set_kind(Kind::Structure);
        assert_eq!(
            fm.serialize(),
            "---\nstatus: fleeting\nkind: structure\n---\n"
        );
    }

    #[test]
    fn set_source_none_removes() {
        let input = "---\nsource: \"x\"\nstatus: fleeting\n---\n";
        let (mut fm, _) = FrontmatterDoc::parse(input).unwrap();
        fm.set_source(None);
        assert_eq!(fm.serialize(), "---\nstatus: fleeting\n---\n");
    }

    #[test]
    fn set_tags_replaces_block_list_with_inline() {
        let input = "---\ntags:\n  - old-one\n  - old-two\nstatus: fleeting\n---\n";
        let (mut fm, _) = FrontmatterDoc::parse(input).unwrap();
        fm.set_tags(&[Tag::new("rust").unwrap(), Tag::new("egui").unwrap()]);
        assert_eq!(
            fm.serialize(),
            "---\ntags: [rust, egui]\nstatus: fleeting\n---\n"
        );
    }

    #[test]
    fn set_modified_updates_timestamp() {
        let (mut fm, _) = FrontmatterDoc::parse(SETTER_BASE).unwrap();
        let t = crate::time::Timestamp::parse_rfc3339("2026-07-06T08:00:00Z").unwrap();
        fm.set_modified(t);
        assert_eq!(fm.modified(), Some(t));
        assert!(fm.serialize().contains("modified: 2026-07-06T08:00:00Z\n"));
        // untouched lines still verbatim
        assert!(fm.serialize().contains("x-custom: keep me\n"));
    }
}
