//! Vault operation vocabulary (spec §9 command pattern).
//! Pure data types for mutations; execution lands in Tasks 4–6.

use crate::id::NoteId;
use crate::note::{Kind, NewNote};
use crate::tag::Tag;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Dest {
    Inbox,
    Notes,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum OpSource {
    User,
    UndoRedo,
}

/// The complete vocabulary of mutations the vault worker can execute.
///
/// # Path-stability decision (WP3, 2026-07-07)
///
/// Body-derived filenames make some undo paths rel_path-unstable.  Two known
/// cases:
///
/// 1. **Untitled-note Batch undo** — a note whose filename derives from its
///    body content gets a new SaveBody inverse; if the body changed the
///    filename between the original op and its undo, the undo file may land
///    under a different (collision-suffixed) path.
///
/// 2. **RenameTitle undo after old name re-claimed** — when a note is renamed
///    A → B and then another note is created with title A, undoing the rename
///    (B → A) cannot reclaim the original filename `A.md` because it is now
///    occupied.  The undo writes the content to `A (<short-id>).md` instead.
///
/// **Decision**: accept and document.  Collision suffixing (spec §2) already
/// guarantees no clobber; a path-drifted undo is still a correct content
/// restore — the note's id, body, and index entry are all restored correctly.
/// The suffix is a filesystem artefact, not a data loss.
#[derive(Clone, Debug, PartialEq)]
pub enum VaultOp {
    Create {
        seed: NewNote,
        dest: Dest,
    },
    SaveBody {
        id: NoteId,
        content: String,
    },
    RenameTitle {
        id: NoteId,
        new_title: String,
    },
    Promote {
        id: NoteId,
    },
    Demote {
        id: NoteId,
    },
    SetKind {
        id: NoteId,
        kind: Kind,
    },
    SetSource {
        id: NoteId,
        source: Option<String>,
    },
    SetTags {
        id: NoteId,
        tags: Vec<Tag>,
    },
    Toss {
        id: NoteId,
    },
    Delete {
        id: NoteId,
    },
    Restore {
        id: NoteId,
    },
    /// Split a note at a byte offset, extracting the tail into a new note and
    /// inserting a `[[link]]` reference in the host.
    ///
    /// # Undo label
    ///
    /// The op label for journaling and status display is **"Split card"** (or
    /// "Split scrap" for fleeting notes).  The WP3 status echo (Task 6 in
    /// jd-app) appends "(split-off card moved to trash)" when showing the
    /// undo of a Split — pin that suffix in jd-app, not here.
    ///
    /// # Undo behaviour
    ///
    /// The inverse is `Batch([SaveBody(original_host_body), Delete(new_id)])`.
    /// `Delete` moves the split-off note to trash (not permanent deletion),
    /// so the split-off content is always recoverable from the trash floor.
    Split {
        id: NoteId,
        at_byte: usize,
    },
    Batch(Vec<VaultOp>),
}

#[derive(Clone, Debug)]
pub struct OpResult {
    /// None only for ops that create no meaningful reversal (currently none —
    /// every listed op computes an inverse; the Option is for future ops).
    pub inverse: Option<VaultOp>,
    /// User-facing Edit-menu label, e.g. "Toss scrap 'egui layouter idea'".
    pub label: String,
    /// Ids created by this op (Create → 1, Split → 1, Batch → all).
    pub created: Vec<NoteId>,
}

/// Label vocabulary (spec §9/§11 voice): verb + kind-word + quoted display.
pub fn op_label(verb: &str, is_fleeting: bool, display: &str) -> String {
    let kind_word = if is_fleeting { "scrap" } else { "card" };
    format!("{verb} {kind_word} '{display}'")
}

const LABEL_MAX_CHARS: usize = 40;

/// Truncate a display line for labels at a char boundary (~40 chars + …).
pub fn label_display(line: &str) -> String {
    let line = line.trim();
    if line.chars().count() <= LABEL_MAX_CHARS {
        return line.to_owned();
    }
    let mut out: String = line.chars().take(LABEL_MAX_CHARS).collect();
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn op_labels_match_the_spec_voice() {
        assert_eq!(
            op_label("Toss", true, "egui layouter idea"),
            "Toss scrap 'egui layouter idea'"
        );
        assert_eq!(
            op_label("Promote", true, "titles are claims"),
            "Promote scrap 'titles are claims'"
        );
        assert_eq!(
            op_label("Rename", false, "Egui tradeoffs"),
            "Rename card 'Egui tradeoffs'"
        );
    }

    #[test]
    fn label_display_truncates_at_char_boundaries() {
        assert_eq!(label_display("short"), "short");
        let long = "x".repeat(60);
        let out = label_display(&long);
        assert!(out.chars().count() <= 41 && out.ends_with('…'));
        let multi = "é".repeat(60);
        let out = label_display(&multi);
        assert!(out.ends_with('…') && out.chars().all(|c| c == 'é' || c == '…'));
    }

    #[test]
    fn ops_are_cloneable_and_comparable() {
        let op = VaultOp::Batch(vec![
            VaultOp::Promote {
                id: crate::id::NoteId([1; 16]),
            },
            VaultOp::SaveBody {
                id: crate::id::NoteId([1; 16]),
                content: "x".into(),
            },
        ]);
        assert_eq!(op.clone(), op);
    }
}
