//! Per-card text undo with word-granularity grouping.
//!
//! # Grouping rules
//! Consecutive `record()` calls merge into one undo entry until one of:
//! (a) A word boundary is crossed — the edit appended whitespace after non-whitespace.
//! (b) 800 ms have elapsed since the previous edit.
//! (c) Edit direction flipped — insertion after deletion or vice versa.
//! (d) Cursor jumped (|new_cursor - expected_cursor| > 1).
//!
//! `now_ms` is sourced from `ui.input(|i| i.time)` (seconds → ms) — no
//! `std::time::Instant` in this pure type so unit tests can control the clock.

/// A snapshot of the buffer at a point in time.
#[derive(Debug, Clone, PartialEq)]
pub struct Snapshot {
    pub text: String,
    pub cursor: usize,
}

/// Direction of the most recent edit group.
#[derive(Debug, Clone, Copy, PartialEq)]
enum EditDir {
    Insert,
    Delete,
}

/// Per-card word-granularity undo stack.
pub struct TextUndo {
    /// Committed undo entries (most recent last).
    undo: Vec<Snapshot>,
    /// Redo entries (most recent last).
    redo: Vec<Snapshot>,
    /// The current in-flight group's starting snapshot (committed when the
    /// group closes).
    pending: Option<Snapshot>,
    /// The text at the end of the most recent record() call (used to detect
    /// the next edit's direction and expected cursor position).
    prev_text: String,
    /// Cursor position at the end of the most recent record() call.
    prev_cursor: usize,
    /// Direction of the current group.
    pending_dir: Option<EditDir>,
    /// Timestamp (ms) of the first record() call of the current group.
    group_start_ms: Option<u64>,
}

impl TextUndo {
    /// Create a new stack. The `initial` string is pushed as the bottom entry
    /// so undo can always return to it.
    pub fn new(initial: &str) -> TextUndo {
        TextUndo {
            undo: vec![Snapshot {
                text: initial.to_owned(),
                cursor: 0,
            }],
            redo: Vec::new(),
            pending: None,
            prev_text: initial.to_owned(),
            prev_cursor: 0,
            pending_dir: None,
            group_start_ms: None,
        }
    }

    /// Record the buffer state after an edit. Groups consecutive edits per the
    /// rules described in the module doc.
    pub fn record(&mut self, text: &str, cursor: usize, now_ms: u64) {
        // Any fresh edit clears the redo stack.
        self.redo.clear();

        // Determine edit direction for this call.
        let dir = if text.len() >= self.prev_text.len() {
            EditDir::Insert
        } else {
            EditDir::Delete
        };

        // Check each grouping rule.
        // `word_boundary` is special: the triggering char (whitespace) belongs to
        // the old group, so the new group's anchor is the current state (AFTER the
        // whitespace was inserted).  All other rules (time gap, dir flip, cursor
        // jump) mean the triggering edit belongs to the NEW group, so the anchor is
        // prev_text (BEFORE this edit).
        let (should_commit, new_group_anchor_is_current) = if self.pending.is_some() {
            let time_gap = self
                .group_start_ms
                .is_some_and(|t| now_ms.saturating_sub(t) >= 800);
            let dir_flip = self.pending_dir.is_some_and(|d| d != dir);
            let cursor_jump = cursor.abs_diff(self.prev_cursor) > 1;
            // Rule (a): word boundary — the newly-appended char is whitespace
            // after a non-whitespace char in prev_text.
            let word_boundary = dir == EditDir::Insert && {
                // Only fires for single-char inserts (byte length grows by small amount).
                text.len() > self.prev_text.len() && {
                    // Guard: prev_text is a valid prefix of text at byte boundary.
                    let prev_len = self.prev_text.len();
                    text.is_char_boundary(prev_len) && {
                        let inserted = &text[prev_len..];
                        let prev_ends_non_ws = self
                            .prev_text
                            .chars()
                            .last()
                            .is_some_and(|c| !c.is_whitespace());
                        prev_ends_non_ws && inserted.starts_with(char::is_whitespace)
                    }
                }
            };
            let commit = time_gap || dir_flip || cursor_jump || word_boundary;
            (
                commit,
                word_boundary && !time_gap && !dir_flip && !cursor_jump,
            )
        } else {
            (false, false)
        };

        if should_commit {
            // Commit the pending group's starting snapshot to the undo stack.
            if let Some(p) = self.pending.take() {
                self.undo.push(p);
            }
            // For word boundary: the triggering edit is the END of the old group,
            // so the new group starts at the current state (after the whitespace).
            // For all other breaks: the triggering edit is the FIRST edit of the
            // new group, so the new group's anchor is prev_text (before this edit).
            let (anchor_text, anchor_cursor) = if new_group_anchor_is_current {
                (text.to_owned(), cursor)
            } else {
                (self.prev_text.clone(), self.prev_cursor)
            };
            self.pending = Some(Snapshot {
                text: anchor_text,
                cursor: anchor_cursor,
            });
            self.group_start_ms = Some(now_ms);
            self.pending_dir = Some(dir);
        } else if self.pending.is_none() {
            // First edit: start the group from the state before this edit.
            self.pending = Some(Snapshot {
                text: self.prev_text.clone(),
                cursor: self.prev_cursor,
            });
            self.group_start_ms = Some(now_ms);
            self.pending_dir = Some(dir);
        }

        self.prev_text = text.to_owned();
        self.prev_cursor = cursor;
    }

    /// Undo one group. Returns the snapshot to restore, or `None` if already
    /// at the initial state. The current state is pushed onto the redo stack.
    pub fn undo(&mut self, current_text: &str) -> Option<Snapshot> {
        // Commit any in-flight group first.
        if let Some(p) = self.pending.take() {
            self.undo.push(p);
            self.group_start_ms = None;
            self.pending_dir = None;
        }

        // Save current state as the top of redo.
        let current = Snapshot {
            text: current_text.to_owned(),
            cursor: self.prev_cursor,
        };

        // Pop from undo stack (the bottom entry stays — it's the initial state).
        if self.undo.len() <= 1 {
            // Already at the bottom; push nothing onto redo, return None.
            return None;
        }

        let snap = self.undo.pop().unwrap();
        self.redo.push(current);

        // Update prev so subsequent record() / redo logic starts from here.
        self.prev_text = snap.text.clone();
        self.prev_cursor = snap.cursor;

        Some(snap)
    }

    /// Redo one group. Returns the snapshot to restore, or `None` if the redo
    /// stack is empty.
    pub fn redo(&mut self) -> Option<Snapshot> {
        let snap = self.redo.pop()?;
        self.undo.push(Snapshot {
            text: snap.text.clone(),
            cursor: snap.cursor,
        });
        self.prev_text = snap.text.clone();
        self.prev_cursor = snap.cursor;
        Some(snap)
    }
}

// ---------------------------------------------------------------------------
// Unit tests (TDD — written before implementation)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// Simulate typing "hello world" one char at a time, 30 ms apart.
    /// Rule (a) fires when the space is appended after 'o': "hello" is one group,
    /// " world" is the next.
    ///
    /// Stack after all 11 chars:
    ///   undo[0] = ""    (initial)
    ///   undo[1] = ""    (base of "hello" group — actually this IS undo[0])
    ///   pending = Snapshot{ "hello ", 6 }  ("hello " is the start of the second group)
    ///
    /// undo₁ → commits pending ("hello ") onto undo, pops it → returns "hello "
    /// undo₂ → pops undo[0] = "" → returns ""
    /// redo₁ → returns "hello "
    /// redo₂ → returns "hello world"
    #[test]
    fn word_boundary_grouping_hello_world() {
        let mut u = TextUndo::new("");
        let chars: Vec<char> = "hello world".chars().collect();
        let mut buf = String::new();
        for (i, &c) in chars.iter().enumerate() {
            buf.push(c);
            u.record(&buf, buf.chars().count(), (i as u64) * 30);
        }
        // After typing all 11 chars, buf == "hello world".
        assert_eq!(buf, "hello world");

        // undo₁ → should yield "hello " (start of second group)
        let s1 = u.undo(&buf).expect("undo₁ must yield a snapshot");
        assert_eq!(s1.text, "hello ", "undo₁ must return 'hello '");

        // undo₂ → should yield "" (initial)
        let s2 = u.undo(&s1.text).expect("undo₂ must yield a snapshot");
        assert_eq!(s2.text, "", "undo₂ must return ''");

        // undo₃ → should return None (already at the bottom)
        assert!(u.undo(&s2.text).is_none(), "undo₃ must return None");

        // redo₁ → should yield "hello "
        let r1 = u.redo().expect("redo₁ must yield a snapshot");
        assert_eq!(r1.text, "hello ", "redo₁ must return 'hello '");

        // redo₂ → should yield "hello world"
        let r2 = u.redo().expect("redo₂ must yield a snapshot");
        assert_eq!(r2.text, "hello world", "redo₂ must return 'hello world'");
    }

    /// An 800 ms gap starts a new group.
    #[test]
    fn time_gap_starts_new_group() {
        let mut u = TextUndo::new("");
        u.record("ab", 2, 0);
        u.record("abc", 3, 900); // 900 ms gap → new group
        u.record("abcd", 4, 910);

        // undo₁ → back to "abc" (start of second group was "ab")
        // Wait: after commit at 900ms, pending = Snapshot{"ab", 2}.
        // After record("abcd", 4, 910), pending is still Snapshot{"ab", 2}.
        // undo() commits pending → undo = ["", "ab"], then pops "ab" → returns "ab".
        let s1 = u.undo("abcd").expect("undo₁");
        assert_eq!(s1.text, "ab", "undo after time gap must return 'ab'");

        // undo₂ → back to "" (initial)
        let s2 = u.undo(&s1.text).expect("undo₂");
        assert_eq!(s2.text, "", "undo₂ must return initial ''");
    }

    /// Deletion after insertion starts a new group.
    #[test]
    fn deletion_starts_new_group() {
        let mut u = TextUndo::new("");
        u.record("ab", 2, 0); // insert
        u.record("a", 1, 10); // delete → direction flip → new group

        // undo₁ → returns "ab" (start of deletion group)
        let s = u.undo("a").expect("undo₁");
        assert_eq!(s.text, "ab", "undo after deletion must return 'ab'");
    }

    /// Fresh edit after undo clears the redo stack.
    #[test]
    fn fresh_edit_clears_redo() {
        let mut u = TextUndo::new("");
        u.record("a", 1, 0);
        let _ = u.undo("a"); // undo → redo stack has "a"
        u.record("b", 1, 100); // fresh edit → redo cleared
        assert!(u.redo().is_none(), "redo must be cleared after fresh edit");
    }

    /// undo returns None when already at the initial state.
    #[test]
    fn undo_returns_none_at_bottom() {
        let mut u = TextUndo::new("x");
        assert!(
            u.undo("x").is_none(),
            "undo must return None at initial state"
        );
    }

    /// Cursor jump (> 1) commits the group.
    #[test]
    fn cursor_jump_commits_group() {
        let mut u = TextUndo::new("");
        u.record("a", 1, 0); // cursor expected next at 2
        u.record("ab", 10, 10); // cursor jumped from 1 to 10 → new group

        // undo₁ → returns "a" (start of second group)
        let s = u.undo("ab").expect("undo₁");
        assert_eq!(s.text, "a", "cursor jump must commit group; undo₁ → 'a'");
    }

    /// redo returns None when stack is empty.
    #[test]
    fn redo_returns_none_when_empty() {
        let mut u = TextUndo::new("");
        assert!(u.redo().is_none());
    }
}
