//! UiState: session, body cache, journal, scan status, pending operations.
//! Geom conversion helpers (jd_core::geom ↔ egui).

use std::collections::{HashMap, HashSet};
use std::sync::mpsc::Sender;

use jd_core::id::NoteId;
use jd_core::journal::Journal;
use jd_core::session::SessionState;
use jd_core::worker::VaultCommand;

// ---------------------------------------------------------------------------
// Geom conversions
// ---------------------------------------------------------------------------

pub fn to_egui(v: jd_core::geom::Vec2) -> eframe::egui::Vec2 {
    eframe::egui::Vec2::new(v.x, v.y)
}

pub fn to_pos2(v: jd_core::geom::Vec2) -> eframe::egui::Pos2 {
    eframe::egui::Pos2::new(v.x, v.y)
}

pub fn from_egui(v: eframe::egui::Vec2) -> jd_core::geom::Vec2 {
    jd_core::geom::Vec2 { x: v.x, y: v.y }
}

// ---------------------------------------------------------------------------
// BodyCache
// ---------------------------------------------------------------------------

pub struct CachedBody {
    pub text: String,
}

/// Cache for note bodies with single-request discipline.
/// A missing id fires exactly ONE `ReadBody` command until either the body
/// arrives (`insert`) or the entry is invalidated (`invalidate`/`invalidate_all`).
#[derive(Default)]
pub struct BodyCache {
    map: HashMap<NoteId, CachedBody>,
    pending: HashSet<NoteId>,
}

impl BodyCache {
    /// Returns the cached body, or `None` after enqueueing ONE `ReadBody` request.
    /// If already pending, returns `None` without sending a duplicate.
    pub fn get_or_request(
        &mut self,
        id: NoteId,
        commands: &Sender<VaultCommand>,
    ) -> Option<&CachedBody> {
        if self.map.contains_key(&id) {
            return self.map.get(&id);
        }
        if !self.pending.contains(&id) {
            self.pending.insert(id);
            let _ = commands.send(VaultCommand::ReadBody { id });
        }
        None
    }

    /// Insert a received body, clearing its pending flag.
    pub fn insert(&mut self, id: NoteId, content: String) {
        self.pending.remove(&id);
        self.map.insert(id, CachedBody { text: content });
    }

    /// Returns the cached body without requesting it; `None` if not yet loaded.
    /// Used by kitests to poll for body load completion.
    pub fn get_cached(&self, id: NoteId) -> Option<&CachedBody> {
        self.map.get(&id)
    }

    /// Invalidate a single entry, forcing re-request on next access.
    pub fn invalidate(&mut self, id: NoteId) {
        self.map.remove(&id);
        self.pending.remove(&id);
    }

    /// Invalidate all entries.
    pub fn invalidate_all(&mut self) {
        self.map.clear();
        self.pending.clear();
    }
}

// ---------------------------------------------------------------------------
// PendingCreate
// ---------------------------------------------------------------------------

pub struct PendingCreate {
    pub at: jd_core::geom::Vec2,
    pub open_editor: bool,
}

// ---------------------------------------------------------------------------
// UiState
// ---------------------------------------------------------------------------

pub struct UiState {
    pub session: SessionState,
    pub session_dirty_at: Option<std::time::Instant>,
    pub focus: Option<NoteId>,
    pub editor: Option<crate::editor::EditorState>,
    pub bodies: BodyCache,
    pub journal: Journal,
    pub scan_done: bool,
    pub last_error: Option<String>,
    pub pending_create: Option<PendingCreate>,
    /// Per-card undo stacks that survive editor close/reopen within the session.
    pub text_undo: HashMap<NoteId, crate::text_undo::TextUndo>,
    /// WP3 Task 4: human-readable label for a compound vault op whose worker
    /// label (Batch's first-member label) is too generic. Set when dispatching
    /// a Batch([SaveBody, Promote]) from the close-editor path; consumed by the
    /// next matching OpDone in drain_events.
    pub pending_label: Option<String>,
    /// WP3 Task 4: when an editor is opened with pending_promotion=true but the
    /// body hasn't arrived yet (deferred open via drain_events Body event),
    /// stash the flag here so the Body handler can forward it.
    pub pending_open_promotion: bool,
    /// WP3 Task 5: delete-confirm pending for this NoteId (Permanent note only).
    /// Set when Del is pressed on a Permanent note; cleared by Enter (confirm)
    /// or Esc (cancel) in the confirm modal.
    pub pending_confirm: Option<NoteId>,
}

impl Default for UiState {
    fn default() -> Self {
        UiState {
            session: SessionState::default(),
            session_dirty_at: None,
            focus: None,
            editor: None,
            bodies: BodyCache::default(),
            journal: Journal::new(),
            scan_done: false,
            last_error: None,
            pending_create: None,
            text_undo: HashMap::new(),
            pending_label: None,
            pending_open_promotion: false,
            pending_confirm: None,
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use jd_core::worker::VaultCommand;
    use std::sync::mpsc;

    fn nid(n: u8) -> jd_core::id::NoteId {
        let s = format!("01ARZ3NDEKTSV4RRFFQ69G5F{n:02}");
        jd_core::id::NoteId::parse(&s).unwrap()
    }

    #[test]
    fn body_cache_requests_once_and_caches() {
        let (tx, rx) = mpsc::channel();
        let mut c = BodyCache::default();
        assert!(c.get_or_request(nid(1), &tx).is_none());
        assert!(c.get_or_request(nid(1), &tx).is_none()); // pending: no duplicate
        let sent: Vec<_> = rx.try_iter().collect();
        assert_eq!(sent.len(), 1);
        assert!(matches!(sent[0], VaultCommand::ReadBody { id } if id == nid(1)));
        c.insert(nid(1), "hello".into());
        assert_eq!(c.get_or_request(nid(1), &tx).unwrap().text, "hello");
        assert!(rx.try_iter().next().is_none());
        c.invalidate(nid(1));
        assert!(c.get_or_request(nid(1), &tx).is_none());
        assert_eq!(rx.try_iter().count(), 1);
    }
}
