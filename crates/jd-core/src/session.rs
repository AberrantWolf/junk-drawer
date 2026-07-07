//! Session state: desks, placed cards, current surface, open card.
//! File format is disposable — corrupt/missing → default, never an error.
//! Arch §2.13.

use crate::error::IoError;
use crate::geom::Vec2;
use crate::id::{IdGen, NoteId};
use crate::vault::Vault;
use crate::vault::io::atomic_save;

// ---------------------------------------------------------------------------
// Identifiers
// ---------------------------------------------------------------------------

/// A desk's identity — wraps the same ULID machinery as NoteId but is not a note.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct DeskId(pub NoteId);

impl DeskId {
    pub fn generate(r#gen: &mut IdGen) -> DeskId {
        DeskId(NoteId::generate(r#gen))
    }
}

impl std::fmt::Display for DeskId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

// ---------------------------------------------------------------------------
// Surface identifier
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SurfaceId {
    Desk(DeskId),
    Inbox,
    Drawer,
    Map,
    Trash,
}

// ---------------------------------------------------------------------------
// Viewport + PlacedCard
// ---------------------------------------------------------------------------

/// Camera state for a desk.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Viewport {
    pub center: Vec2,
    pub zoom: f32,
}

impl Default for Viewport {
    fn default() -> Self {
        Viewport {
            center: Vec2::default(),
            zoom: 1.0,
        }
    }
}

/// A note placed on a desk surface.
#[derive(Clone, PartialEq, Debug)]
pub struct PlacedCard {
    pub id: NoteId,
    pub pos: Vec2,
}

// ---------------------------------------------------------------------------
// Desk
// ---------------------------------------------------------------------------

#[derive(Clone, PartialEq, Debug)]
pub struct Desk {
    pub id: DeskId,
    pub name: String,
    pub viewport: Viewport,
    pub cards: Vec<PlacedCard>,
}

// ---------------------------------------------------------------------------
// SessionState
// ---------------------------------------------------------------------------

#[derive(Clone, PartialEq, Debug, Default)]
pub struct SessionState {
    pub desks: Vec<Desk>,
    pub current_surface: Option<SurfaceId>,
    pub open_card: Option<NoteId>,
}

// ---------------------------------------------------------------------------
// SessionOp
// ---------------------------------------------------------------------------

#[derive(Clone, PartialEq, Debug)]
pub enum SessionOp {
    Place {
        desk: DeskId,
        id: NoteId,
        pos: Vec2,
    },
    Move {
        desk: DeskId,
        id: NoteId,
        from: Vec2,
        to: Vec2,
    },
    PutAway {
        desk: DeskId,
        id: NoteId,
        was_at: Vec2,
    },
    CreateDesk {
        id: DeskId,
        name: String,
    },
    RenameDesk {
        id: DeskId,
        from: String,
        to: String,
    },
    ReorderDesk {
        id: DeskId,
        from_index: usize,
        to_index: usize,
    },
    /// Carries the full desk so the inverse can restore it.
    DeleteDesk {
        desk: Desk,
    },
}

// ---------------------------------------------------------------------------
// apply + inverse
// ---------------------------------------------------------------------------

impl SessionState {
    /// Apply an op and return its inverse.
    ///
    /// Lenient semantics for card ops:
    /// - Move: if card is found, moves it; if absent, inserts it at `to`
    ///   (upsert). The inverse is Move when found, PutAway when inserted.
    /// - PutAway: toggles — removes if present, inserts at `was_at` if absent.
    ///   This is its own inverse, enabling the round-trip contract.
    ///
    /// DeleteDesk is self-inverse: removes if present, inserts (restoring
    /// full desk+cards) if absent. Unknown desk ids are no-ops.
    pub fn apply(&mut self, op: &SessionOp) -> SessionOp {
        match op {
            SessionOp::Place { desk, id, pos } => {
                if let Some(d) = self.desks.iter_mut().find(|d| d.id == *desk) {
                    d.cards.push(PlacedCard { id: *id, pos: *pos });
                }
                SessionOp::PutAway {
                    desk: *desk,
                    id: *id,
                    was_at: *pos,
                }
            }

            SessionOp::Move {
                desk,
                id,
                from: _,
                to,
            } => {
                if let Some(d) = self.desks.iter_mut().find(|d| d.id == *desk) {
                    if let Some(card) = d.cards.iter_mut().find(|c| c.id == *id) {
                        // Card exists: move it; inverse is Move back.
                        let old = card.pos;
                        card.pos = *to;
                        return SessionOp::Move {
                            desk: *desk,
                            id: *id,
                            from: *to,
                            to: old,
                        };
                    } else {
                        // Card absent: insert at `to`; inverse is PutAway.
                        d.cards.push(PlacedCard { id: *id, pos: *to });
                    }
                }
                SessionOp::PutAway {
                    desk: *desk,
                    id: *id,
                    was_at: *to,
                }
            }

            SessionOp::PutAway { desk, id, was_at } => {
                if let Some(d) = self.desks.iter_mut().find(|d| d.id == *desk) {
                    if let Some(idx) = d.cards.iter().position(|c| c.id == *id) {
                        // Card present: remove it (forward direction).
                        d.cards.remove(idx);
                    } else {
                        // Card absent: insert at `was_at` (inverse/restore direction).
                        d.cards.push(PlacedCard {
                            id: *id,
                            pos: *was_at,
                        });
                    }
                }
                // PutAway is its own inverse.
                SessionOp::PutAway {
                    desk: *desk,
                    id: *id,
                    was_at: *was_at,
                }
            }

            SessionOp::CreateDesk { id, name } => {
                self.desks.push(Desk {
                    id: *id,
                    name: name.clone(),
                    viewport: Viewport::default(),
                    cards: Vec::new(),
                });
                // Inverse: delete the just-created (empty) desk
                SessionOp::DeleteDesk {
                    desk: self.desks.last().unwrap().clone(),
                }
            }

            SessionOp::RenameDesk { id, from: _, to } => {
                let actual_from = if let Some(d) = self.desks.iter_mut().find(|d| d.id == *id) {
                    let old = d.name.clone();
                    d.name = to.clone();
                    old
                } else {
                    to.clone()
                };
                SessionOp::RenameDesk {
                    id: *id,
                    from: to.clone(),
                    to: actual_from,
                }
            }

            SessionOp::ReorderDesk {
                id,
                from_index,
                to_index,
            } => {
                if *from_index < self.desks.len() {
                    let desk = self.desks.remove(*from_index);
                    let insert_at = (*to_index).min(self.desks.len());
                    self.desks.insert(insert_at, desk);
                }
                SessionOp::ReorderDesk {
                    id: *id,
                    from_index: *to_index,
                    to_index: *from_index,
                }
            }

            SessionOp::DeleteDesk { desk } => {
                match self.desks.iter().position(|d| d.id == desk.id) {
                    Some(idx) => {
                        // Desk exists: remove it; inverse re-inserts the full desk.
                        let removed = self.desks.remove(idx);
                        SessionOp::DeleteDesk { desk: removed }
                    }
                    None => {
                        // Desk absent: re-insert it (this is the inverse path).
                        self.desks.push(desk.clone());
                        SessionOp::DeleteDesk { desk: desk.clone() }
                    }
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// File path helper
// ---------------------------------------------------------------------------

fn session_path(vault: &Vault) -> std::path::PathBuf {
    vault.abs(std::path::Path::new(".junkdrawer/session/session.jd"))
}

// ---------------------------------------------------------------------------
// Serialise
// ---------------------------------------------------------------------------

fn write_surface(surface: SurfaceId) -> String {
    match surface {
        SurfaceId::Desk(d) => format!("desk {d}"),
        SurfaceId::Inbox => "inbox".to_owned(),
        SurfaceId::Drawer => "drawer".to_owned(),
        SurfaceId::Map => "map".to_owned(),
        SurfaceId::Trash => "trash".to_owned(),
    }
}

fn serialise(state: &SessionState) -> String {
    let mut out = String::new();
    out.push_str("jd-session 1\n");
    if let Some(surface) = state.current_surface {
        out.push_str(&format!("surface = {}\n", write_surface(surface)));
    }
    if let Some(card) = state.open_card {
        out.push_str(&format!("open = {card}\n"));
    }
    for desk in &state.desks {
        out.push_str(&format!("\n[desk {}]\n", desk.id));
        out.push_str(&format!("name = {}\n", desk.name));
        let vp = desk.viewport;
        out.push_str(&format!(
            "viewport = {} {} {}\n",
            vp.center.x, vp.center.y, vp.zoom
        ));
        for card in &desk.cards {
            out.push_str(&format!(
                "card = {} {} {}\n",
                card.id, card.pos.x, card.pos.y
            ));
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Parse
// ---------------------------------------------------------------------------

/// Lenient-all-or-nothing: any malformed line → return default.
fn parse(text: &str) -> Option<SessionState> {
    let mut lines = text.lines().peekable();

    // Header check
    let header = lines.next()?;
    if header.trim() != "jd-session 1" {
        return None;
    }

    let mut state = SessionState::default();
    let mut current_desk: Option<Desk> = None;

    for line in lines {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        // Section header: [desk <ulid>]
        if line.starts_with('[') && line.ends_with(']') {
            // Push previous desk
            if let Some(d) = current_desk.take() {
                state.desks.push(d);
            }
            let inner = &line[1..line.len() - 1];
            let mut parts = inner.splitn(2, ' ');
            let kind = parts.next()?;
            if kind != "desk" {
                return None;
            }
            let ulid = parts.next()?.trim();
            let note_id = NoteId::parse(ulid).ok()?;
            current_desk = Some(Desk {
                id: DeskId(note_id),
                name: String::new(),
                viewport: Viewport::default(),
                cards: Vec::new(),
            });
            continue;
        }

        // Key = value
        let (key, value) = if let Some(pos) = line.find('=') {
            let k = line[..pos].trim();
            let v = line[pos + 1..].trim();
            (k, v)
        } else {
            return None; // malformed line
        };

        if let Some(ref mut desk) = current_desk {
            match key {
                "name" => desk.name = value.to_owned(),
                "viewport" => {
                    let mut parts = value.split_ascii_whitespace();
                    let cx: f32 = parts.next()?.parse().ok()?;
                    let cy: f32 = parts.next()?.parse().ok()?;
                    let zoom: f32 = parts.next()?.parse().ok()?;
                    desk.viewport = Viewport {
                        center: Vec2 { x: cx, y: cy },
                        zoom,
                    };
                }
                "card" => {
                    let mut parts = value.split_ascii_whitespace();
                    let ulid = parts.next()?;
                    let note_id = NoteId::parse(ulid).ok()?;
                    let px: f32 = parts.next()?.parse().ok()?;
                    let py: f32 = parts.next()?.parse().ok()?;
                    desk.cards.push(PlacedCard {
                        id: note_id,
                        pos: Vec2 { x: px, y: py },
                    });
                }
                _ => return None, // unknown key = malformed
            }
        } else {
            match key {
                "surface" => {
                    state.current_surface = Some(parse_surface(value)?);
                }
                "open" => {
                    state.open_card = Some(NoteId::parse(value).ok()?);
                }
                _ => return None, // unknown key = malformed
            }
        }
    }

    // Push final desk
    if let Some(d) = current_desk {
        state.desks.push(d);
    }

    Some(state)
}

fn parse_surface(s: &str) -> Option<SurfaceId> {
    match s {
        "inbox" => Some(SurfaceId::Inbox),
        "drawer" => Some(SurfaceId::Drawer),
        "map" => Some(SurfaceId::Map),
        "trash" => Some(SurfaceId::Trash),
        other => {
            let mut parts = other.splitn(2, ' ');
            if parts.next()? != "desk" {
                return None;
            }
            let ulid = parts.next()?;
            let note_id = NoteId::parse(ulid).ok()?;
            Some(SurfaceId::Desk(DeskId(note_id)))
        }
    }
}

// ---------------------------------------------------------------------------
// load / save
// ---------------------------------------------------------------------------

impl SessionState {
    /// Load from `.junkdrawer/session/session.jd`.
    /// Missing or corrupt file → `SessionState::default()`, never an error.
    pub fn load(vault: &Vault) -> SessionState {
        let path = session_path(vault);
        let text = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            Err(_) => return SessionState::default(),
        };
        parse(&text).unwrap_or_default()
    }

    /// Atomically save to `.junkdrawer/session/session.jd`.
    pub fn save(&self, vault: &Vault) -> Result<(), IoError> {
        let path = session_path(vault);
        atomic_save(&path, &serialise(self))
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::id::{IdGen, NoteId};

    fn nid(n: u8) -> NoteId {
        NoteId([n; 16])
    }

    fn desk_with(r#gen: &mut IdGen, name: &str) -> (SessionState, DeskId) {
        let mut s = SessionState::default();
        let id = DeskId::generate(r#gen);
        s.apply(&SessionOp::CreateDesk {
            id,
            name: name.into(),
        });
        (s, id)
    }

    #[test]
    fn every_session_op_inverts_exactly() {
        let mut r#gen = IdGen::new();
        let (mut s, desk) = desk_with(&mut r#gen, "Work");
        let ops = vec![
            SessionOp::Place {
                desk,
                id: nid(1),
                pos: Vec2 { x: 10.0, y: 20.0 },
            },
            SessionOp::Move {
                desk,
                id: nid(1),
                from: Vec2 { x: 10.0, y: 20.0 },
                to: Vec2 { x: 50.0, y: 60.0 },
            },
            SessionOp::RenameDesk {
                id: desk,
                from: "Work".into(),
                to: "Deep Work".into(),
            },
            SessionOp::PutAway {
                desk,
                id: nid(1),
                was_at: Vec2 { x: 50.0, y: 60.0 },
            },
        ];
        for op in ops {
            let before = s.clone();
            let inverse = s.apply(&op);
            assert_ne!(s, before, "op must change state: {op:?}");
            s.apply(&inverse);
            assert_eq!(s, before, "inverse must restore state for {op:?}");
        }
    }

    #[test]
    fn create_and_delete_desk_invert() {
        let mut r#gen = IdGen::new();
        let mut s = SessionState::default();
        let id = DeskId::generate(&mut r#gen);
        let inv = s.apply(&SessionOp::CreateDesk {
            id,
            name: "Fresh".into(),
        });
        assert_eq!(s.desks.len(), 1);
        // populate, then delete carries the whole desk in the inverse
        s.apply(&SessionOp::Place {
            desk: id,
            id: nid(9),
            pos: Vec2::default(),
        });
        let full = s.desks[0].clone();
        let before = s.clone();
        let del_inv = s.apply(&SessionOp::DeleteDesk { desk: full });
        assert!(s.desks.is_empty());
        s.apply(&del_inv);
        assert_eq!(s, before, "delete restores desk with cards and position");
        let _ = inv;
    }

    #[test]
    fn reorder_inverts() {
        let mut r#gen = IdGen::new();
        let mut s = SessionState::default();
        let a = DeskId::generate(&mut r#gen);
        let b = DeskId::generate(&mut r#gen);
        let c = DeskId::generate(&mut r#gen);
        for (id, name) in [(a, "A"), (b, "B"), (c, "C")] {
            s.apply(&SessionOp::CreateDesk {
                id,
                name: name.into(),
            });
        }
        let before = s.clone();
        let inv = s.apply(&SessionOp::ReorderDesk {
            id: c,
            from_index: 2,
            to_index: 0,
        });
        assert_eq!(s.desks[0].id, c);
        s.apply(&inv);
        assert_eq!(s, before);
    }

    #[test]
    fn save_load_round_trips() {
        let t = crate::vault::testutil::TempDir::new();
        let vault = crate::vault::Vault::open(t.path()).unwrap();
        let mut r#gen = IdGen::new();
        let (mut s, desk) = desk_with(&mut r#gen, "Reading Notes");
        s.apply(&SessionOp::Place {
            desk,
            id: nid(1),
            pos: Vec2 { x: 100.0, y: 200.5 },
        });
        s.apply(&SessionOp::Place {
            desk,
            id: nid(2),
            pos: Vec2 { x: -40.0, y: 60.0 },
        });
        s.desks[0].viewport = Viewport {
            center: Vec2 { x: 120.5, y: -80.0 },
            zoom: 1.25,
        };
        s.current_surface = Some(SurfaceId::Desk(desk));
        s.open_card = Some(nid(2));
        s.save(&vault).unwrap();
        let loaded = SessionState::load(&vault);
        assert_eq!(loaded, s);
    }

    #[test]
    fn missing_or_corrupt_file_loads_default() {
        let t = crate::vault::testutil::TempDir::new();
        let vault = crate::vault::Vault::open(t.path()).unwrap();
        assert_eq!(SessionState::load(&vault), SessionState::default());
        std::fs::write(
            t.path().join(".junkdrawer/session/session.jd"),
            "garbage ]]] \0\n",
        )
        .unwrap();
        assert_eq!(SessionState::load(&vault), SessionState::default());
    }
}
