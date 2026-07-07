//! Session-long undo/redo journal (spec §2.11).
//! 200-entry cap with oldest eviction. Redo is cleared by push (new user op),
//! but preserved by push_undo_from_redo (redo-chain case).

use crate::command::VaultOp;
use crate::id::NoteId;
use crate::session::{DeskId, SessionOp};

#[derive(Clone, Debug, PartialEq)]
pub enum InverseAction {
    Vault(VaultOp),
    Session(SessionOp),
}

#[derive(Clone, Copy, PartialEq, Debug, Default)]
pub struct OpContext {
    pub desk: Option<DeskId>,
    pub note: Option<NoteId>,
}

#[derive(Clone, Debug)]
pub struct JournalEntry {
    pub label: String,
    pub inverse: InverseAction,
    pub context: OpContext,
}

pub const JOURNAL_CAP: usize = 200;

#[derive(Default)]
pub struct Journal {
    undo: Vec<JournalEntry>,
    redo: Vec<JournalEntry>,
}

impl Journal {
    pub fn new() -> Journal {
        Journal::default()
    }

    /// Push a new entry to undo stack, clearing redo and evicting oldest past CAP.
    pub fn push(&mut self, e: JournalEntry) {
        self.redo.clear();
        self.undo.push(e);
        if self.undo.len() > JOURNAL_CAP {
            self.undo.remove(0);
        }
    }

    /// Undo label of the most recent entry on the undo stack.
    pub fn undo_label(&self) -> Option<&str> {
        self.undo.last().map(|e| e.label.as_str())
    }

    /// Redo label of the most recent entry on the redo stack.
    pub fn redo_label(&self) -> Option<&str> {
        self.redo.last().map(|e| e.label.as_str())
    }

    /// Pop the most recent entry from undo stack.
    /// Caller executes its inverse, then calls push_redo.
    pub fn pop_undo(&mut self) -> Option<JournalEntry> {
        self.undo.pop()
    }

    /// Push an entry to the redo stack (after caller has executed the inverse).
    pub fn push_redo(&mut self, e: JournalEntry) {
        self.redo.push(e);
    }

    /// Pop the most recent entry from redo stack.
    /// Caller executes it, then calls push_undo_from_redo (not push!).
    pub fn pop_redo(&mut self) -> Option<JournalEntry> {
        self.redo.pop()
    }

    /// Push to undo WITHOUT clearing redo (redo-chain case).
    /// Used when caller pops from redo, executes it, then needs to push the inverse.
    pub fn push_undo_from_redo(&mut self, e: JournalEntry) {
        self.undo.push(e);
        if self.undo.len() > JOURNAL_CAP {
            self.undo.remove(0);
        }
    }

    pub fn len(&self) -> usize {
        self.undo.len()
    }

    pub fn is_empty(&self) -> bool {
        self.undo.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::command::VaultOp;
    use crate::id::NoteId;

    fn entry(label: &str) -> JournalEntry {
        JournalEntry {
            label: label.to_owned(),
            inverse: InverseAction::Vault(VaultOp::Promote {
                id: NoteId([1; 16]),
            }),
            context: OpContext::default(),
        }
    }

    #[test]
    fn push_pop_labels() {
        let mut j = Journal::new();
        assert!(j.undo_label().is_none());
        j.push(entry("Toss scrap 'a'"));
        j.push(entry("Promote scrap 'b'"));
        assert_eq!(j.undo_label(), Some("Promote scrap 'b'"));
        let e = j.pop_undo().unwrap();
        assert_eq!(e.label, "Promote scrap 'b'");
        assert_eq!(j.undo_label(), Some("Toss scrap 'a'"));
    }

    #[test]
    fn redo_flow_and_new_push_clears_redo() {
        let mut j = Journal::new();
        j.push(entry("one"));
        j.push(entry("two"));
        let e = j.pop_undo().unwrap();
        j.push_redo(e);
        assert_eq!(j.redo_label(), Some("two"));
        // redo it: pop_redo then push_undo_from_redo keeps deeper redo intact
        let e = j.pop_redo().unwrap();
        j.push_undo_from_redo(e);
        assert_eq!(j.undo_label(), Some("two"));
        assert!(j.redo_label().is_none());
        // a NEW user op clears redo
        let e = j.pop_undo().unwrap();
        j.push_redo(e);
        assert_eq!(j.redo_label(), Some("two"));
        j.push(entry("three"));
        assert!(j.redo_label().is_none(), "new op clears redo");
    }

    #[test]
    fn cap_evicts_oldest() {
        let mut j = Journal::new();
        for i in 0..(JOURNAL_CAP + 10) {
            j.push(entry(&format!("op {i}")));
        }
        assert_eq!(j.len(), JOURNAL_CAP);
        // oldest evicted: popping everything ends at "op 10"
        let mut last = None;
        while let Some(e) = j.pop_undo() {
            last = Some(e.label);
        }
        assert_eq!(last.as_deref(), Some("op 10"));
    }
}
