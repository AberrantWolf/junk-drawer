//! Whole-file view of a note: frontmatter + body, with the pure extractors
//! that turn a body into indexable metadata. Parsing is infallible — a file
//! that isn't note-shaped is still a valid body that round-trips untouched.

use std::collections::BTreeSet;
use std::ops::Range;
use std::path::Path;

use crate::frontmatter::{FmError, FrontmatterDoc};
use crate::id::NoteId;
use crate::note::{LinkRef, NoteMeta, Status};
use crate::tag::Tag;
use crate::time::Timestamp;

pub struct NoteDoc {
    pub fm: FrontmatterDoc,
    pub body: String,
}

impl NoteDoc {
    pub fn parse(input: &str) -> NoteDoc {
        match FrontmatterDoc::parse(input) {
            Ok((fm, consumed)) => NoteDoc {
                fm,
                body: input[consumed..].to_owned(),
            },
            Err(FmError::NoOpeningMarker) | Err(FmError::Unterminated) => NoteDoc {
                fm: FrontmatterDoc::empty(),
                body: input.to_owned(),
            },
        }
    }

    pub fn serialize(&self) -> String {
        let mut out = self.fm.serialize();
        out.push_str(&self.body);
        out
    }

    /// `id` comes from the caller: frontmatter if present, else assigned at scan.
    pub fn to_meta(&self, id: NoteId, rel_path: &Path, fs_modified: Timestamp) -> NoteMeta {
        let path_default = if rel_path.starts_with("inbox") {
            Status::Fleeting
        } else {
            Status::Permanent
        };
        let title = extract_title(&self.body).map(|(t, _)| t);
        let mut tags: BTreeSet<Tag> = self.fm.tags().into_iter().collect();
        tags.extend(extract_inline_tags(&self.body));
        NoteMeta {
            id,
            rel_path: rel_path.to_owned(),
            title,
            first_line: first_line(&self.body),
            status: self.fm.status().unwrap_or(path_default),
            kind: self.fm.kind(),
            source: self.fm.source(),
            created: self.fm.created().unwrap_or(fs_modified),
            modified: self.fm.modified().unwrap_or(fs_modified),
            tags,
            links_out: extract_links(&self.body),
            word_count: word_count(&self.body),
        }
    }
}

/// First non-empty line, trimmed, leading heading marks stripped.
fn first_line(body: &str) -> String {
    body.lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .map(|l| l.trim_start_matches('#').trim_start().to_owned())
        .unwrap_or_default()
}

/// Iterate body lines with their byte offsets, tracking fenced-code state.
/// `f(line, line_start_offset, in_fence)`.
fn for_each_line(body: &str, mut f: impl FnMut(&str, usize, bool)) {
    let mut offset = 0;
    let mut in_fence = false;
    for line in body.split_inclusive('\n') {
        let c = line.trim_end_matches('\n').trim_end_matches('\r');
        if c.trim_start().starts_with("```") {
            in_fence = !in_fence;
            f(c, offset, true); // fence marker lines themselves are "code"
        } else {
            f(c, offset, in_fence);
        }
        offset += line.len();
    }
}

/// Byte ranges of inline-code spans (`...`) within one line.
fn inline_code_ranges(line: &str) -> Vec<Range<usize>> {
    let mut ranges = Vec::new();
    let mut open: Option<usize> = None;
    for (i, ch) in line.char_indices() {
        if ch == '`' {
            match open.take() {
                Some(start) => ranges.push(start..i + 1),
                None => open = Some(i),
            }
        }
    }
    ranges
}

fn in_ranges(pos: usize, ranges: &[Range<usize>]) -> bool {
    ranges.iter().any(|r| r.contains(&pos))
}

pub fn extract_title(body: &str) -> Option<(String, Range<usize>)> {
    let mut found = None;
    for_each_line(body, |line, offset, in_fence| {
        if found.is_none()
            && !in_fence
            && let Some(rest) = line.strip_prefix("# ")
        {
            let text = rest.trim();
            if !text.is_empty() {
                let start =
                    offset + (line.len() - rest.len()) + (rest.len() - rest.trim_start().len());
                found = Some((text.to_owned(), start..start + text.len()));
            }
        }
    });
    found
}

pub fn extract_links(body: &str) -> Vec<LinkRef> {
    let mut links = Vec::new();
    for_each_line(body, |line, offset, in_fence| {
        if in_fence {
            return;
        }
        let code = inline_code_ranges(line);
        let mut at = 0;
        while let Some(open) = line[at..].find("[[") {
            let open = at + open;
            let Some(close) = line[open + 2..].find("]]") else {
                break;
            };
            let close = open + 2 + close;
            at = close + 2;
            if in_ranges(open, &code) {
                continue;
            }
            let inner = &line[open + 2..close];
            let (target, display) = match inner.split_once('|') {
                Some((t, d)) => (t.trim(), Some(d.trim().to_owned())),
                None => (inner.trim(), None),
            };
            if target.is_empty() {
                continue;
            }
            links.push(LinkRef {
                target: target.to_owned(),
                display,
                span: offset + open..offset + close + 2,
            });
        }
    });
    links
}

pub fn extract_inline_tags(body: &str) -> Vec<Tag> {
    let mut tags = Vec::new();
    for_each_line(body, |line, _offset, in_fence| {
        if in_fence {
            return;
        }
        let code = inline_code_ranges(line);
        let mut prev: Option<char> = None;
        let mut chars = line.char_indices().peekable();
        while let Some((i, ch)) = chars.next() {
            if ch == '#'
                && prev.is_none_or(char::is_whitespace)
                && !in_ranges(i, &code)
                && chars.peek().is_some_and(|(_, c)| c.is_alphanumeric())
            {
                let start = i + 1;
                let mut end = start;
                while let Some(&(j, c)) = chars.peek() {
                    if c.is_alphanumeric() || c == '-' || c == '_' {
                        end = j + c.len_utf8();
                        chars.next();
                    } else {
                        break;
                    }
                }
                if let Some(t) = Tag::new(&line[start..end]) {
                    tags.push(t);
                }
                prev = Some('x'); // non-whitespace
                continue;
            }
            prev = Some(ch);
        }
    });
    tags
}

/// Maximal alphanumeric runs.
pub fn word_count(body: &str) -> u32 {
    let mut count = 0u32;
    let mut in_word = false;
    for ch in body.chars() {
        if ch.is_alphanumeric() {
            if !in_word {
                count += 1;
                in_word = true;
            }
        } else {
            in_word = false;
        }
    }
    count
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::note::{Kind, Status};
    use crate::time::Timestamp;
    use std::path::Path;

    #[test]
    fn parse_without_frontmatter_is_all_body() {
        let doc = NoteDoc::parse("# Just a note\n\nText.\n");
        assert_eq!(doc.body, "# Just a note\n\nText.\n");
        assert_eq!(doc.serialize(), "# Just a note\n\nText.\n");
    }

    #[test]
    fn parse_splits_frontmatter_and_body() {
        let input = "---\nstatus: fleeting\n---\n# Title\nBody.\n";
        let doc = NoteDoc::parse(input);
        assert_eq!(doc.body, "# Title\nBody.\n");
        assert_eq!(doc.serialize(), input);
    }

    #[test]
    fn broken_frontmatter_is_body_not_error() {
        // unterminated block: treat the whole file as body; round-trip untouched
        let input = "---\nstatus: fleeting\nno closing marker\n";
        let doc = NoteDoc::parse(input);
        assert_eq!(doc.serialize(), input);
    }

    #[test]
    fn extract_title_cases() {
        assert_eq!(
            extract_title("# The claim is the title\nbody"),
            Some(("The claim is the title".to_owned(), 2..24))
        );
        assert_eq!(extract_title("no heading here"), None);
        assert_eq!(
            extract_title("## h2 is not the title\n# but this is\n")
                .unwrap()
                .0,
            "but this is"
        );
        // heading inside a code fence doesn't count
        assert_eq!(
            extract_title("```\n# not a title\n```\n# real title\n")
                .unwrap()
                .0,
            "real title"
        );
        // first `# ` wins even if later ones exist
        assert_eq!(
            extract_title("intro line\n# First\n# Second\n").unwrap().0,
            "First"
        );
    }

    #[test]
    fn extract_links_cases() {
        let body = "See [[Alpha]] and [[Beta|the beta note]].\n";
        let links = extract_links(body);
        assert_eq!(links.len(), 2);
        assert_eq!(links[0].target, "Alpha");
        assert_eq!(links[0].display, None);
        assert_eq!(&body[links[0].span.clone()], "[[Alpha]]");
        assert_eq!(links[1].target, "Beta");
        assert_eq!(links[1].display.as_deref(), Some("the beta note"));
        assert_eq!(&body[links[1].span.clone()], "[[Beta|the beta note]]");
    }

    #[test]
    fn links_skip_code_and_malformed() {
        assert!(extract_links("`[[not a link]]`").is_empty());
        assert!(extract_links("```\n[[not a link]]\n```\n").is_empty());
        assert!(extract_links("[[unclosed\n").is_empty());
        assert!(extract_links("[[]]").is_empty()); // empty target
        // spans across lines don't count
        assert!(extract_links("[[first\nsecond]]").is_empty());
    }

    #[test]
    fn extract_inline_tags_cases() {
        let tags: Vec<String> =
            extract_inline_tags("Uses #rust and #egui-widgets.\n#linestart too\n")
                .iter()
                .map(|t| t.as_str().to_owned())
                .collect();
        assert_eq!(tags, vec!["rust", "egui-widgets", "linestart"]);
        // heading is not a tag; code is skipped; mid-word # is not a tag
        assert!(extract_inline_tags("# Heading\n").is_empty());
        assert!(extract_inline_tags("`#code`\n").is_empty());
        assert!(extract_inline_tags("C# is a language\n").is_empty());
    }

    #[test]
    fn word_count_cases() {
        assert_eq!(word_count(""), 0);
        assert_eq!(word_count("hello, world!"), 2);
        assert_eq!(word_count("héllo wörld"), 2);
        assert_eq!(word_count("# heading and [[link text]]"), 4);
    }

    #[test]
    fn to_meta_defaults_by_path_and_falls_back_to_fs_time() {
        let fs_t = Timestamp::parse_rfc3339("2026-07-05T00:00:00Z").unwrap();
        let id = crate::id::NoteId::parse("01J8ZQ4KF3T9M2X7C5VBNAE8RD").unwrap();

        // no frontmatter, in inbox/ → fleeting, fs timestamps
        let doc = NoteDoc::parse("a stray thought\n");
        let meta = doc.to_meta(id, Path::new("inbox/stray.md"), fs_t);
        assert_eq!(meta.status, Status::Fleeting);
        assert_eq!(meta.kind, Kind::Note);
        assert_eq!(meta.created, fs_t);
        assert_eq!(meta.modified, fs_t);
        assert_eq!(meta.title, None);
        assert_eq!(meta.first_line, "a stray thought");

        // notes/ default is permanent; frontmatter overrides win
        let doc = NoteDoc::parse("---\nstatus: fleeting\ntags: [zettel]\n---\n# T\nBody #inline\n");
        let meta = doc.to_meta(id, Path::new("notes/T.md"), fs_t);
        assert_eq!(meta.status, Status::Fleeting); // frontmatter wins over path default
        assert_eq!(meta.title.as_deref(), Some("T"));
        assert_eq!(meta.first_line, "T");
        let tags: Vec<String> = meta.tags.iter().map(|t| t.as_str().to_owned()).collect();
        assert_eq!(tags, vec!["inline", "zettel"]); // BTreeSet order; union of both sources

        let doc = NoteDoc::parse("body\n");
        let meta = doc.to_meta(id, Path::new("notes/x.md"), fs_t);
        assert_eq!(meta.status, Status::Permanent);
    }
}
