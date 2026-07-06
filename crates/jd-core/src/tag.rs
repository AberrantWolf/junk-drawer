//! Tags: flat, lowercase, plural-insensitive matching (spec §2).
//! The fold is a deliberate heuristic — "bus" folds to "bu", which is fine:
//! both sides of every comparison fold the same way.

/// Stored lowercase, `#` and surrounding whitespace stripped. No nesting.
#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Debug)]
pub struct Tag(String);

impl Tag {
    pub fn new(raw: &str) -> Option<Tag> {
        let s = raw.trim().trim_start_matches('#').trim();
        if s.is_empty() || s.chars().any(char::is_whitespace) {
            return None;
        }
        Some(Tag(s.to_lowercase()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Canonical singular-ish form used as the index bucket key.
    pub fn fold_key(&self) -> String {
        let s = &self.0;
        if s.len() > 2 && s.ends_with("es") {
            let stem = &s[..s.len() - 2];
            if stem.ends_with('s')
                || stem.ends_with('x')
                || stem.ends_with('z')
                || stem.ends_with("ch")
                || stem.ends_with("sh")
            {
                return stem.to_owned();
            }
        }
        if s.len() > 1 && s.ends_with('s') && !s.ends_with("ss") {
            return s[..s.len() - 1].to_owned();
        }
        s.clone()
    }

    pub fn matches(&self, other: &Tag) -> bool {
        self.fold_key() == other.fold_key()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tag(s: &str) -> Tag {
        Tag::new(s).unwrap()
    }

    #[test]
    fn normalizes_to_lowercase_and_strips_hash() {
        assert_eq!(tag("Rust").as_str(), "rust");
        assert_eq!(tag("#rust").as_str(), "rust");
        assert_eq!(tag("#Zettelkasten").as_str(), "zettelkasten");
        assert_eq!(tag("  method  ").as_str(), "method");
    }

    #[test]
    fn rejects_empty_and_whitespace() {
        assert!(Tag::new("").is_none());
        assert!(Tag::new("#").is_none());
        assert!(Tag::new("   ").is_none());
        assert!(Tag::new("two words").is_none());
    }

    #[test]
    fn plural_insensitive_matching() {
        // plain -s
        assert!(tag("book").matches(&tag("books")));
        assert!(tag("books").matches(&tag("book")));
        // -es after s/x/z/ch/sh stems
        assert!(tag("box").matches(&tag("boxes")));
        assert!(tag("class").matches(&tag("classes")));
        assert!(tag("branch").matches(&tag("branches")));
        // "notes" folds by the plain-s rule (stem "not" doesn't take -es)
        assert!(tag("note").matches(&tag("notes")));
    }

    #[test]
    fn ss_endings_do_not_fold() {
        assert_eq!(tag("boss").fold_key(), "boss");
        assert!(!tag("boss").matches(&tag("bos")));
    }

    #[test]
    fn identical_tags_match() {
        assert!(tag("rust").matches(&tag("rust")));
        assert!(!tag("rust").matches(&tag("python")));
    }

    #[test]
    fn fold_keys() {
        assert_eq!(tag("books").fold_key(), "book");
        assert_eq!(tag("boxes").fold_key(), "box");
        assert_eq!(tag("classes").fold_key(), "class");
        assert_eq!(tag("rust").fold_key(), "rust");
    }
}
