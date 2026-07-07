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
