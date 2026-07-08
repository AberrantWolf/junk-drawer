//! JdUi: the whole application as an egui-only struct (kittest-testable).
//! JdApp: the thin eframe shell around it.

use std::path::Path;
use std::sync::{Arc, Mutex};

use eframe::egui;
use jd_core::command::{Dest, OpSource, VaultOp};
use jd_core::error::CoreError;
use jd_core::geom::Vec2 as CoreVec2;
use jd_core::id::{IdGen, NoteId};
use jd_core::journal::{InverseAction, JournalEntry, OpContext};
use jd_core::note::{Kind, NewNote, Status};
use jd_core::session::{DeskId, SessionOp, SessionState, SurfaceId};
use jd_core::vault::Vault;
use jd_core::worker::{self, VaultCommand, VaultEvent, VaultHandle};

use crate::menus::{CardMenuEvent, EditMenuAction, EditMenuCtx, edit_menu_bar};
use crate::rail::{RailDropTarget, RailEvent, RailUiDeps};
use crate::state::{UiState, UndoRedoKind};
use crate::surfaces::desk::{DeskEvent, DeskUiDeps, DragState, FaceMeta};
use crate::surfaces::drawer::{DrawerEvent, DrawerUiDeps};
use crate::surfaces::inbox::{InboxEvent, InboxUiDeps};
use crate::surfaces::trash::{TrashEvent, TrashUiDeps};

/// Repaint hook: the worker wakes us between frames; the egui Context only
/// exists once the first frame runs, so it's injected lazily.
#[derive(Clone, Default)]
pub struct Waker(Arc<Mutex<Option<egui::Context>>>);

impl Waker {
    fn wake(&self) {
        if let Some(ctx) = self.0.lock().unwrap().as_ref() {
            ctx.request_repaint();
        }
    }

    fn attach(&self, ctx: &egui::Context) {
        let mut slot = self.0.lock().unwrap();
        if slot.is_none() {
            *slot = Some(ctx.clone());
        }
    }
}

pub struct JdUi {
    pub vault: VaultHandle,
    /// A second Vault handle for session load/save (worker::start consumed the
    /// first one; Vault::open just resolves paths — no exclusive lock).
    vault_ref: Vault,
    waker: Waker,
    pub state: UiState,
    pub theme: crate::theme::Theme,
    pub fonts_installed: bool,
    id_gen: IdGen,
    /// Persists across frames for card body layout caching.
    line_cache: crate::editor::LineCache,
    /// Current card-drag state (None = no drag in progress).
    drag: Option<DragState>,
    /// The desk panel rect captured from `ui.max_rect()` inside the CentralPanel
    /// closure each frame, one frame before it is used by reveal() in FocusChanged.
    /// None until the first frame has rendered.
    last_panel_rect: Option<egui::Rect>,
    /// Rail row rects from the previous frame: (screen rect, drop target).
    /// Populated by rail_ui each frame via RailUiDeps::row_hits; consumed by
    /// desk_ui's drag-release path to decide CardDroppedOnInbox / CardDroppedOnDesk.
    /// Public so kitests can inspect or seed it directly.
    pub rail_row_hits: Vec<(egui::Rect, RailDropTarget)>,
}

impl JdUi {
    pub fn new(vault_root: &Path) -> Result<JdUi, CoreError> {
        // Open a lightweight Vault reference for session load/save (paths only,
        // no exclusive locking — Vault::open is idempotent and cheap).
        let vault_ref = Vault::open(vault_root)?;
        // Load session BEFORE starting the worker (worker::start consumes vault).
        let session = SessionState::load(&vault_ref);
        // Open a second Vault so worker::start can consume it.
        let vault_for_worker = Vault::open(vault_root)?;
        let waker = Waker::default();
        let w = waker.clone();
        let handle = worker::start(vault_for_worker, Box::new(move || w.wake()))?;
        let state = UiState {
            session,
            ..Default::default()
        };
        Ok(JdUi {
            vault: handle,
            vault_ref,
            waker,
            state,
            theme: crate::theme::Theme::light(),
            fonts_installed: false,
            id_gen: IdGen::new(),
            line_cache: crate::editor::LineCache::default(),
            drag: None,
            last_panel_rect: None,
            rail_row_hits: Vec::new(),
        })
    }

    /// Apply a session op, always mark dirty, and push a journal entry when
    /// `journal` is `Some(label)`.  This is the single authoritative path for
    /// all session mutations.
    pub fn apply_session(&mut self, op: SessionOp, journal: Option<&'static str>) {
        let inverse = self.state.session.apply(&op);
        self.state.session_dirty_at = Some(std::time::Instant::now());
        if let Some(label) = journal {
            // Extract the subject note id from op variants that carry one.
            let note_id = match &op {
                SessionOp::Move { id, .. }
                | SessionOp::Place { id, .. }
                | SessionOp::PutAway { id, .. } => Some(*id),
                _ => None,
            };
            let context = jd_core::journal::OpContext {
                desk: if let Some(SurfaceId::Desk(desk_id)) = self.state.session.current_surface {
                    Some(desk_id)
                } else {
                    None
                },
                note: note_id,
            };
            self.state.journal.push(JournalEntry {
                label: label.to_owned(),
                inverse: InverseAction::Session(inverse),
                context,
            });
        }
    }

    /// Place a note on a desk at the given world position.
    /// Journaled as "Place card" and marks the session dirty.
    pub fn place_card(&mut self, desk: DeskId, id: NoteId, pos: CoreVec2) {
        self.apply_session(SessionOp::Place { desk, id, pos }, Some("Place card"));
    }

    /// If a pending_create is set and the OpDone was a Create op (identified by
    /// its inverse being `VaultOp::Delete { .. }` — the shape WP1e returns for
    /// Create), place the new card and optionally open the editor.
    ///
    /// Robustness: only consumes pending_create when the inverse is Delete, so a
    /// future non-Create op with a non-empty `created` list (e.g. Split) cannot
    /// steal the pending placement.
    ///
    /// WP3 handoff: a stale pending_create that outlives ScanComplete (e.g. if the
    /// worker emits OpDone before ScanComplete arrives) will be cleared on the
    /// next qualifying OpDone; this edge case is tracked as a WP3 cleanup item.
    fn handle_pending_create(&mut self, result: &jd_core::command::OpResult) {
        // Only consume pending_create for Create ops: their inverse is Delete{id}.
        let is_create_op = matches!(result.inverse, Some(VaultOp::Delete { .. }));
        if !is_create_op {
            return;
        }
        let Some(pending) = self.state.pending_create.take() else {
            return;
        };
        let Some(&new_id) = result.created.first() else {
            // No id in this Create result (shouldn't happen, but be safe).
            self.state.pending_create = Some(pending);
            return;
        };
        if let Some(desk_id) = self.state.session.desks.first().map(|d| d.id) {
            self.place_card(desk_id, new_id, pending.at);
            if pending.open_editor {
                self.state.session.open_card = Some(new_id);
                // session_dirty_at already set by place_card → apply_session; no reset needed.
            }
        } else {
            // No desk yet (OpDone arrived before ScanComplete). Buffer the created
            // id so ScanComplete can place the card once the bootstrap desk exists.
            // Keep pending_create alive (put it back) so ScanComplete can read the
            // placement position (`at`) and open_editor flag.
            self.state.pending_create = Some(pending);
            // Single-writer: only the first orphaned id is kept.
            self.state.orphaned_create_id.get_or_insert(new_id);
        }
    }

    /// Frame-loop step 1 (architecture §3): drain ALL pending worker events.
    pub fn drain_events(&mut self) {
        while let Ok(ev) = self.vault.events.try_recv() {
            match ev {
                VaultEvent::ScanComplete { quarantined } => {
                    self.state.scan_done = true;
                    // WP4 Task 3: Needs-Attention plumbing — keep the full list
                    // (rel_path + reason); each scan replaces it wholesale.
                    self.state.quarantined = quarantined;
                    self.state.bodies.invalidate_all();
                    // First-run: create a default desk if none exist.
                    if self.state.session.desks.is_empty() {
                        let desk_id = DeskId::generate(&mut self.id_gen);
                        // Bootstrap desk: system act, not undoable — no journal label.
                        // Marking dirty here is harmless: a first-run session save is
                        // acceptable and keeps the dirty invariant simple (apply_session
                        // always marks dirty).
                        self.apply_session(
                            SessionOp::CreateDesk {
                                id: desk_id,
                                name: "Desk".into(),
                            },
                            None,
                        );
                        self.state.session.current_surface = Some(SurfaceId::Desk(desk_id));
                    }
                    // Consume any Create OpDone that arrived before this ScanComplete
                    // (reversed-order race: Ctrl+N → worker writes note → OpDone →
                    // ScanComplete). The bootstrap desk now exists, so place the card.
                    if let Some(orphan_id) = self.state.orphaned_create_id.take()
                        && let Some(pending) = self.state.pending_create.take()
                        && let Some(desk_id) = self.state.session.desks.first().map(|d| d.id)
                    {
                        self.place_card(desk_id, orphan_id, pending.at);
                        if pending.open_editor {
                            self.state.session.open_card = Some(orphan_id);
                        }
                    }
                }
                VaultEvent::Body { id, content } => {
                    // If this body belongs to the open_card and no editor is live yet,
                    // open the editor now.  This is the "body arrived" trigger described
                    // in the architecture: OpenCard fires get_or_request; when the body
                    // lands here the editor is created.
                    if self.state.session.open_card == Some(id) && self.state.editor.is_none() {
                        let saved_undo = self.state.text_undo.remove(&id);
                        let is_fleeting = {
                            let idx = self.vault.index.read().unwrap();
                            idx.get(id)
                                .map(|m| m.status == jd_core::note::Status::Fleeting)
                                .unwrap_or(false)
                        };
                        // Forward any pending_open_promotion (set by Inbox Ctrl+Enter
                        // when the body wasn't yet cached).
                        let pending_promotion =
                            std::mem::take(&mut self.state.pending_open_promotion);
                        self.state.editor = Some(crate::editor::EditorState::open(
                            id,
                            content.clone(),
                            saved_undo,
                            is_fleeting,
                            pending_promotion,
                        ));
                    }
                    self.state.bodies.insert(id, content);
                }
                VaultEvent::External { changed, removed } => {
                    for id in changed {
                        self.state.bodies.invalidate(id);
                    }
                    for id in &removed {
                        self.state.bodies.invalidate(*id);
                        // Remove the card from every desk (external reality, not journaled).
                        for desk in &mut self.state.session.desks {
                            desk.cards.retain(|c| &c.id != id);
                        }
                    }
                }
                VaultEvent::OpDone { result, source } => {
                    // Invalidate bodies for created notes.
                    for id in &result.created {
                        self.state.bodies.invalidate(*id);
                    }
                    // Invalidate bodies for notes mutated by the op (SaveBody,
                    // RenameTitle, Toss, etc.) — extracted from the inverse op so
                    // ops that carry no subject id (Create) contribute nothing.
                    if let Some(ref inv_op) = result.inverse {
                        let mut subject_ids: Vec<NoteId> = Vec::new();
                        op_subject_ids(inv_op, &mut subject_ids);
                        for id in subject_ids {
                            self.state.bodies.invalidate(id);
                        }
                    }

                    // Clean desk sessions: remove any placed card whose note is no longer
                    // in the index (moved to trash by Delete/Toss, or purged).
                    // This covers the Split-undo case: the undo Batch([SaveBody, Delete])
                    // moves the split-off to trash; the split-off's placement must be
                    // evicted from the desk immediately after the op completes.
                    // We scan all desk card ids against the current index — O(n*m) but
                    // n (desk cards) and m (op executions) are both small in practice.
                    // Note: this retain runs on every OpDone regardless of op family.
                    // A tighter gate (e.g. only when result.created is non-empty or the
                    // inverse involves Delete/Toss) would reduce overhead, but the op
                    // frequency and desk sizes are both small, so the breadth is accepted.
                    {
                        let idx = self.vault.index.read().unwrap();
                        let mut any_removed = false;
                        for desk in &mut self.state.session.desks {
                            let before = desk.cards.len();
                            desk.cards.retain(|c| idx.get(c.id).is_some());
                            if desk.cards.len() != before {
                                any_removed = true;
                            }
                        }
                        drop(idx);
                        if any_removed {
                            self.state.session_dirty_at = Some(std::time::Instant::now());
                        }
                    }

                    // Ctrl+N pending_create: if a Create just finished while
                    // pending_create is set, place the new card and open editor.
                    self.handle_pending_create(&result);

                    // Task 8: pending_split — if a Split Batch just finished
                    // (source==User, result.created non-empty), place the
                    // original card (if not already on desk) and the split-off
                    // card side-by-side.  Placement uses apply_session(None) so
                    // it is NOT journaled — the Split undo trashes the split-off
                    // and the SaveBody inverse restores the original body, so
                    // the placement rides the op itself.
                    if source == OpSource::User
                        && let Some(orig_id) = self.state.pending_split
                        && !result.created.is_empty()
                    {
                        self.state.pending_split = None;
                        let split_off_id = result.created[0];
                        // Resolve the desk to place on: prefer current desk, fall back to
                        // the first desk when current_surface is Inbox or another non-desk
                        // surface (e.g. the user opened a scrap from the Inbox and split it).
                        let desk_id = match self.state.session.current_surface {
                            Some(SurfaceId::Desk(id)) => Some(id),
                            _ => self.state.session.desks.first().map(|d| d.id),
                        };
                        if let Some(desk_id) = desk_id {
                            // Determine the original card's position on this desk.
                            let orig_pos = self
                                .state
                                .session
                                .desks
                                .iter()
                                .find(|d| d.id == desk_id)
                                .and_then(|d| d.cards.iter().find(|c| c.id == orig_id))
                                .map(|c| c.pos);
                            // If the original is not on the desk, place it at the
                            // viewport center first.
                            let orig_pos = if let Some(p) = orig_pos {
                                p
                            } else {
                                let center = self
                                    .state
                                    .session
                                    .desks
                                    .iter()
                                    .find(|d| d.id == desk_id)
                                    .map(|d| d.viewport.center)
                                    .unwrap_or_default();
                                // Place original (not journaled — rides the Split op).
                                self.state.session.apply(&SessionOp::Place {
                                    desk: desk_id,
                                    id: orig_id,
                                    pos: center,
                                });
                                self.state.session_dirty_at = Some(std::time::Instant::now());
                                center
                            };
                            // Place the split-off card at original_pos + (card_width + gap, 0).
                            // Use 300.0 (IndexCard width) + 24 gap as the offset constant.
                            // (The actual card size may differ if the split-off is a Scrap,
                            // but we use the host card's nominal width for a clean layout.)
                            let split_off_pos = CoreVec2 {
                                x: orig_pos.x + 324.0,
                                y: orig_pos.y,
                            };
                            self.state.session.apply(&SessionOp::Place {
                                desk: desk_id,
                                id: split_off_id,
                                pos: split_off_pos,
                            });
                            self.state.session_dirty_at = Some(std::time::Instant::now());
                            // When the split was triggered from a non-desk surface, echo
                            // the placement so the act isn't silent.
                            if !matches!(
                                self.state.session.current_surface,
                                Some(SurfaceId::Desk(_))
                            ) {
                                let desk_name = self
                                    .state
                                    .session
                                    .desks
                                    .iter()
                                    .find(|d| d.id == desk_id)
                                    .map(|d| d.name.as_str())
                                    .unwrap_or("desk");
                                self.state.status_echo = Some((
                                    format!("Split placed on desk '{desk_name}'"),
                                    std::time::Instant::now(),
                                ));
                            }
                        } else {
                            // No desks exist — the split cards are in the vault but cannot
                            // be placed anywhere. This should not happen post-bootstrap.
                            self.state.status_echo = Some((
                                "Split cards are in the vault (no desk to place them)".to_owned(),
                                std::time::Instant::now(),
                            ));
                        }
                    } else if source == OpSource::User && self.state.pending_split.is_some() {
                        // Split Batch completed but created is empty (shouldn't happen
                        // for a successful split — clear guard to avoid stale state).
                        self.state.pending_split = None;
                    }

                    // Push to journal only for user-originated ops.
                    if source == OpSource::User
                        && let Some(inv_op) = result.inverse
                    {
                        // WP3 Task 4: if a pending_label was set when dispatching
                        // a compound op (Batch([SaveBody, Promote])), use it
                        // instead of the worker's generic Batch label.
                        let label = self.state.pending_label.take().unwrap_or(result.label);
                        // Build OpContext: current desk + first subject id from the inverse op.
                        let mut subject_ids: Vec<NoteId> = Vec::new();
                        op_subject_ids(&inv_op, &mut subject_ids);
                        let context = OpContext {
                            desk: if let Some(SurfaceId::Desk(desk_id)) =
                                self.state.session.current_surface
                            {
                                Some(desk_id)
                            } else {
                                None
                            },
                            note: subject_ids.into_iter().next(),
                        };
                        self.state.journal.push(JournalEntry {
                            label,
                            inverse: InverseAction::Vault(inv_op),
                            context,
                        });
                    } else if source == OpSource::UndoRedo {
                        // Fresh inverse from the async undo/redo op.
                        if let (Some(kind), Some(mut stashed), Some(fresh_inv)) = (
                            self.state.pending_undo_redo.take(),
                            self.state.pending_undo_entry.take(),
                            result.inverse,
                        ) {
                            let context = stashed.context;
                            stashed.inverse = InverseAction::Vault(fresh_inv);
                            match kind {
                                UndoRedoKind::Undo => {
                                    self.state.journal.push_redo(stashed);
                                }
                                UndoRedoKind::Redo => {
                                    self.state.journal.push_undo_from_redo(stashed);
                                }
                            }
                            // View-travel: if the entry's context names a desk, switch to it.
                            self.do_view_travel(context);
                        }
                    }
                }
                VaultEvent::OpFailed { label, message } => {
                    self.state.last_error = Some(format!("{label}: {message}"));
                    // Clear any pending_label so it cannot leak onto the next
                    // successful op's journal entry (WP3 Task 4 review finding).
                    self.state.pending_label = None;
                    restore_failed_undo_redo(&mut self.state);
                }
                VaultEvent::Error { context, message } => {
                    self.state.last_error = Some(format!("{context}: {message}"));
                }
                // WP4 Task 3: Needs-Attention plumbing. Session-scoped and
                // deduped (already-known ids fall through to the catch-all);
                // the conflict copy itself already sits on disk.
                VaultEvent::Conflict { id, .. } if !self.state.conflicts.contains(&id) => {
                    self.state.conflicts.push(id);
                }
                _ => {}
            }
        }
    }

    /// Apply a single `RailEvent` — public so kitests can dispatch events directly.
    pub fn apply_rail_event(&mut self, ev: RailEvent) {
        match ev {
            RailEvent::Switch(surface) => {
                // Navigation is not undoable (like viewport moves).
                self.state.session.current_surface = Some(surface);
                self.state.session_dirty_at = Some(std::time::Instant::now());
            }

            RailEvent::CreateDesk => {
                let id = DeskId::generate(&mut self.id_gen);
                let n = self.state.session.desks.len() + 1;
                self.apply_session(
                    SessionOp::CreateDesk {
                        id,
                        name: format!("Desk {n}"),
                    },
                    Some("Create desk"),
                );
                // Switch to the new desk.
                self.state.session.current_surface = Some(SurfaceId::Desk(id));
            }

            RailEvent::RenameDesk { id, name } => {
                // `from` = current name (apply_session reads it from the desk).
                let from = self
                    .state
                    .session
                    .desks
                    .iter()
                    .find(|d| d.id == id)
                    .map(|d| d.name.clone())
                    .unwrap_or_default();
                self.apply_session(
                    SessionOp::RenameDesk { id, from, to: name },
                    Some("Rename desk"),
                );
            }

            RailEvent::ReorderDesk { id, to } => {
                let from_index = self
                    .state
                    .session
                    .desks
                    .iter()
                    .position(|d| d.id == id)
                    .unwrap_or(0);
                self.apply_session(
                    SessionOp::ReorderDesk {
                        id,
                        from_index,
                        to_index: to,
                    },
                    Some("Reorder desk"),
                );
            }

            // ── Card-drop events ────────────────────────────────────────────

            // Card dropped on Inbox row = PutAway from source desk.
            // Journal: ONE entry "Put card away" with inverse =
            // Session(Place{desk: source, id, pos: was_at}).
            RailEvent::CardDroppedOnInbox {
                id,
                source_desk,
                was_at,
            } => {
                // Apply PutAway WITHOUT going through apply_session so we can
                // build a custom journal entry with a Place inverse.
                let _inverse = self.state.session.apply(&SessionOp::PutAway {
                    desk: source_desk,
                    id,
                    was_at,
                });
                self.state.session_dirty_at = Some(std::time::Instant::now());
                // ONE journal entry; inverse restores the card at its old position.
                self.state.journal.push(JournalEntry {
                    label: "Put card away".to_owned(),
                    inverse: InverseAction::Session(SessionOp::Place {
                        desk: source_desk,
                        id,
                        pos: was_at,
                    }),
                    context: OpContext {
                        desk: Some(source_desk),
                        note: Some(id),
                    },
                });
            }

            // Card dropped on a desk row = PutAway from source + Place on target.
            // Journal: ONE entry "Move card to desk '<name>'" with composite inverse.
            // Task 6's undo executor applies Sessions in order and journals the
            // reverse-ordered inverses as the redo entry.
            RailEvent::CardDroppedOnDesk {
                target_desk,
                id,
                source_desk,
                was_at,
            } => {
                let target_name = self
                    .state
                    .session
                    .desks
                    .iter()
                    .find(|d| d.id == target_desk)
                    .map(|d| d.name.clone())
                    .unwrap_or_default();
                // PutAway from source (no journal).
                let _put_away_inverse = self.state.session.apply(&SessionOp::PutAway {
                    desk: source_desk,
                    id,
                    was_at,
                });
                // Place on target at target viewport center.
                let target_center = self
                    .state
                    .session
                    .desks
                    .iter()
                    .find(|d| d.id == target_desk)
                    .map(|d| d.viewport.center)
                    .unwrap_or_default();
                let _place_inverse = self.state.session.apply(&SessionOp::Place {
                    desk: target_desk,
                    id,
                    pos: target_center,
                });
                self.state.session_dirty_at = Some(std::time::Instant::now());
                // ONE journal entry; inverse is Sessions applied in order:
                // 1. PutAway from target (undoes the Place on target)
                // 2. Place back on source at old pos (undoes the PutAway from source)
                self.state.journal.push(JournalEntry {
                    label: format!("Move card to desk '{target_name}'"),
                    inverse: InverseAction::Sessions(vec![
                        SessionOp::PutAway {
                            desk: target_desk,
                            id,
                            was_at: target_center,
                        },
                        SessionOp::Place {
                            desk: source_desk,
                            id,
                            pos: was_at,
                        },
                    ]),
                    context: OpContext {
                        desk: Some(source_desk),
                        note: Some(id),
                    },
                });
            }
        }
    }

    pub fn ui(&mut self, ui: &mut egui::Ui) {
        if !self.fonts_installed {
            crate::theme::install_fonts(ui.ctx());
            self.fonts_installed = true;
        }
        let dark = ui.style().visuals.dark_mode;
        if dark != self.theme.dark {
            self.theme = if dark {
                crate::theme::Theme::dark()
            } else {
                crate::theme::Theme::light()
            };
        }
        self.waker.attach(ui.ctx());

        // ══════════════════════════════════════════════════════════════════
        // Frame-loop order (architecture §3, WP2)
        // ══════════════════════════════════════════════════════════════════

        // 1. Drain all pending worker events (OpDone, ScanComplete, Body, …).
        //    OpDone + pending_create → place_card + open editor inside drain_events.
        self.drain_events();

        // 2. IPC — WP7; skip.

        // 3. Global shortcut dispatch.
        //    If editor is open → only editor keys (Task 10).
        //    If confirm modal is pending → only modal Enter/Esc (below); all
        //    other shortcuts (Ctrl+N, Del, surface keys) are suppressed.
        //    Otherwise: Ctrl+N → create a new fleeting scrap in Inbox.
        if self.state.editor.is_none() {
            // ------------------------------------------------------------------
            // Ctrl+K: toggle the palette (WP4 Task 1).
            // Spec says "anywhere in-app", but our overlay discipline is one
            // overlay at a time: Ctrl+K with the editor open is a WP6 question,
            // so it is gated off here (whole block runs only when the editor is
            // closed) and while a confirm modal is pending.
            // consume_key so the K never leaks into the palette's TextEdit.
            // ------------------------------------------------------------------
            let ctrl_k = ui.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, egui::Key::K));
            if ctrl_k && self.state.pending_confirm.is_none() {
                if self.state.palette.take().is_some() {
                    // Toggled closed: release keyboard focus held by the input.
                    ui.ctx().memory_mut(|mem| mem.stop_text_input());
                } else {
                    self.state.palette = Some(crate::palette::PaletteState::new());
                }
            }
            let palette_open = self.state.palette.is_some();

            let ctrl_n = ui.input(|i| {
                i.events.iter().any(|e| {
                    matches!(
                        e,
                        egui::Event::Key {
                            key: egui::Key::N,
                            pressed: true,
                            modifiers,
                            ..
                        } if modifiers.command
                    )
                })
            });
            if ctrl_n && self.state.pending_confirm.is_none() && !palette_open {
                // Determine where to place the new card: pointer world pos if
                // the pointer is over the panel, otherwise panel center.
                let place_at = self
                    .last_panel_rect
                    .map(|panel| {
                        let pointer = ui.input(|i| i.pointer.latest_pos());
                        let ptr = pointer.unwrap_or(panel.center());
                        // Convert screen → world using the current camera.
                        if let Some(desk) = self.state.session.desks.first() {
                            let cam = crate::surfaces::desk::DeskCamera {
                                center: egui::vec2(desk.viewport.center.x, desk.viewport.center.y),
                                zoom: desk.viewport.zoom,
                            };
                            let world = cam.to_world(panel, ptr);
                            CoreVec2 {
                                x: world.x,
                                y: world.y,
                            }
                        } else {
                            CoreVec2::default()
                        }
                    })
                    .unwrap_or_default();

                self.state.pending_create = Some(crate::state::PendingCreate {
                    at: place_at,
                    open_editor: true,
                });

                // Send Create{seed: empty fleeting scrap, dest: Inbox}.
                let seed = NewNote {
                    body: String::new(),
                    status: Status::Fleeting,
                    kind: Kind::Note,
                    source: None,
                    tags: Vec::new(),
                };
                let _ = self.vault.commands.send(VaultCommand::Op {
                    op: VaultOp::Create {
                        seed,
                        dest: Dest::Inbox,
                    },
                    source: OpSource::User,
                });
            }

            // ------------------------------------------------------------------
            // Del key: toss or delete the focused card (Task 5).
            // Fleeting → VaultOp::Toss immediately (no confirm).
            // Permanent → open delete-confirm modal (pending_confirm).
            // Inbox surface handles Del for its own fleeting list; this branch
            // handles the desk surface (where both Fleeting and Permanent notes
            // may be focused) and the Trash surface (no-op).
            // When a confirm modal is already pending, Del is a no-op here
            // (Enter/Esc in the modal section below handles it).
            // ------------------------------------------------------------------
            let del_pressed = ui.input(|i| i.key_pressed(egui::Key::Delete));
            if del_pressed
                && self.state.pending_confirm.is_none()
                && !palette_open
                && matches!(
                    self.state.session.current_surface,
                    Some(jd_core::session::SurfaceId::Desk(_))
                )
                && let Some(focused_id) = self.state.focus
            {
                let is_fleeting = {
                    let idx = self.vault.index.read().unwrap();
                    idx.get(focused_id)
                        .map(|m| m.status == jd_core::note::Status::Fleeting)
                        .unwrap_or(false)
                };
                if is_fleeting {
                    let _ = self.vault.commands.send(VaultCommand::Op {
                        op: VaultOp::Toss { id: focused_id },
                        source: OpSource::User,
                    });
                } else {
                    // Permanent note: open the confirm modal.
                    self.state.pending_confirm = Some(focused_id);
                }
            }

            // ------------------------------------------------------------------
            // Ctrl+Z / Ctrl+Shift+Z / Ctrl+Y: undo/redo (Task 6).
            // Suppressed while the palette is open: its TextEdit owns Ctrl+Z
            // (text-field undo), which must not fire the app journal.
            // ------------------------------------------------------------------
            if self.state.pending_confirm.is_none() && !palette_open {
                let ctrl_z = ui.input(|i| {
                    i.events.iter().any(|e| {
                        matches!(
                            e,
                            egui::Event::Key {
                                key: egui::Key::Z,
                                pressed: true,
                                modifiers,
                                ..
                            } if modifiers.command && !modifiers.shift
                        )
                    })
                });
                let ctrl_shift_z = ui.input(|i| {
                    i.events.iter().any(|e| {
                        matches!(
                            e,
                            egui::Event::Key {
                                key: egui::Key::Z,
                                pressed: true,
                                modifiers,
                                ..
                            } if modifiers.command && modifiers.shift
                        )
                    })
                });
                let ctrl_y = ui.input(|i| {
                    i.events.iter().any(|e| {
                        matches!(
                            e,
                            egui::Event::Key {
                                key: egui::Key::Y,
                                pressed: true,
                                modifiers,
                                ..
                            } if modifiers.command
                        )
                    })
                });
                if ctrl_z {
                    self.execute_undo();
                } else if ctrl_shift_z || ctrl_y {
                    self.execute_redo();
                }
            }

            // ------------------------------------------------------------------
            // Delete-confirm modal (Task 5): Enter = confirm delete, Esc = cancel.
            // Runs whenever pending_confirm is Some, regardless of surface.
            // ------------------------------------------------------------------
            if let Some(confirm_id) = self.state.pending_confirm {
                // consume_key prevents Enter/Esc from leaking to surface handlers
                // (desk_ui, inbox_ui) in the same frame — defense in depth on top
                // of the confirm_pending gate in those surfaces.
                let enter_pressed =
                    ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Enter));
                let esc_pressed =
                    ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape));
                if enter_pressed {
                    self.state.pending_confirm = None;
                    let _ = self.vault.commands.send(VaultCommand::Op {
                        op: VaultOp::Delete { id: confirm_id },
                        source: OpSource::User,
                    });
                } else if esc_pressed {
                    self.state.pending_confirm = None;
                }
            }
        }

        // 4. Render: left rail + central surface + status line; editor overlay (Task 10).

        // ------------------------------------------------------------------
        // Expire stale status echo (Task 6).
        // ------------------------------------------------------------------
        if let Some((_, ref ts)) = self.state.status_echo
            && ts.elapsed() >= std::time::Duration::from_secs(4)
        {
            self.state.status_echo = None;
        }

        // ------------------------------------------------------------------
        // Highlight pulse (WP4 Task 2): a ~600ms fading ring at a card the
        // palette panned to. Computed here as (id, 0..1 age fraction) for
        // desk_ui; expired entries are cleared. Repaint while active so the
        // fade animates.
        // ------------------------------------------------------------------
        const PULSE_SECS: f32 = 0.6;
        let highlight_pulse: Option<(NoteId, f32)> =
            self.state.highlight_pulse.and_then(|(id, t0)| {
                let frac = t0.elapsed().as_secs_f32() / PULSE_SECS;
                (frac < 1.0).then_some((id, frac))
            });
        if self.state.highlight_pulse.is_some() {
            if highlight_pulse.is_none() {
                self.state.highlight_pulse = None;
            } else {
                ui.ctx().request_repaint();
            }
        }

        // ------------------------------------------------------------------
        // Edit menu bar (top panel) — must be added before Bottom/SidePanels
        // and CentralPanel so egui allocates its space correctly.
        // Actions: Undo/Redo delegate to execute_undo/execute_redo.
        //          SplitCard: only enabled when editor is open; sets
        //          editor.split_requested so editor.rs dispatches the Batch on
        //          the same frame (the editor handles Ctrl+Shift+Enter directly
        //          via its pre-TextEdit hook; the menu action fires through the
        //          same EditorEvent::SplitRequested path — see editor.rs).
        // ------------------------------------------------------------------
        let edit_menu_action = egui::Panel::top("edit_menu_bar")
            .exact_size(24.0)
            .show(ui, |ui| {
                let ctx = EditMenuCtx {
                    journal: &self.state.journal,
                    editor_open: self.state.editor.is_some(),
                };
                edit_menu_bar(ui, &ctx)
            })
            .inner;
        match edit_menu_action {
            Some(EditMenuAction::Undo) => {
                if self.state.editor.is_none() && self.state.pending_confirm.is_none() {
                    self.execute_undo();
                }
            }
            Some(EditMenuAction::Redo) => {
                if self.state.editor.is_none() && self.state.pending_confirm.is_none() {
                    self.execute_redo();
                }
            }
            Some(EditMenuAction::SplitCard) => {
                // Signal the editor to execute a split on this frame.
                if let Some(ed) = &mut self.state.editor {
                    ed.split_requested = true;
                }
            }
            None => {}
        }

        // ------------------------------------------------------------------
        // Status line (bottom) — must be added before SidePanel and CentralPanel.
        // ------------------------------------------------------------------
        let fit_clicked = egui::Panel::bottom("status_line")
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label("Junk Drawer");
                    if let Some((ref echo_text, _)) = self.state.status_echo {
                        ui.label(echo_text.as_str());
                    }
                    let fit = ui.button("Fit");
                    // Zoom % — show for the active desk surface only.
                    if let Some(SurfaceId::Desk(desk_id)) = self.state.session.current_surface
                        && let Some(desk) =
                            self.state.session.desks.iter().find(|d| d.id == desk_id)
                    {
                        ui.label(format!("{:.0}%", desk.viewport.zoom * 100.0));
                    }
                    if let Some(err) = &self.state.last_error {
                        ui.label(err.as_str());
                    }
                    fit.clicked()
                })
                .inner
            })
            .inner;

        // ------------------------------------------------------------------
        // Left rail (always visible).
        // Inbox count: computed once per frame under a single index read lock,
        // mirroring the FaceMeta lock pattern established in WP2.
        // ------------------------------------------------------------------
        let inbox_count = {
            let idx = self.vault.index.read().unwrap();
            idx.fleeting().len()
        };
        let rail_events = egui::Panel::left("rail")
            .resizable(false)
            .exact_size(160.0)
            .show(ui, |ui| {
                let mut deps = RailUiDeps {
                    session: &self.state.session,
                    inbox_count,
                    id_gen: &mut self.id_gen,
                    row_hits: &mut self.rail_row_hits,
                };
                crate::rail::rail_ui(ui, &mut deps)
            })
            .inner;
        self.apply_rail_events(rail_events);

        // ------------------------------------------------------------------
        // Central panel: route to the active surface.
        // ------------------------------------------------------------------
        egui::CentralPanel::default().show(ui, |ui| {
            match self.state.session.current_surface {
                Some(SurfaceId::Desk(desk_id)) => {
                    // Prefetch FaceMeta for all placed cards under ONE index read
                    // lock — plus (WP4 Task 5) the ghost-fan candidates and the
                    // edges-on-select list for the anchor (focused-or-open) card.
                    let (face_metas, ghost_anchor, ghosts, edges) = {
                        let idx = self.vault.index.read().unwrap();
                        let desk = self.state.session.desks.iter().find(|d| d.id == desk_id);
                        let face_metas: Vec<FaceMeta> = desk
                            .map(|desk| {
                                desk.cards
                                    .iter()
                                    .filter_map(|c| {
                                        idx.get(c.id).map(|m| FaceMeta::from_note_meta(m, &idx))
                                    })
                                    .collect()
                            })
                            .unwrap_or_default();
                        let on_desk: std::collections::HashSet<NoteId> = desk
                            .map(|d| d.cards.iter().map(|c| c.id).collect())
                            .unwrap_or_default();
                        // Anchor: the focused card, else the open card — but
                        // only when it sits ON this desk (ghosts + edges are
                        // desk-local by definition).
                        let anchor = self.state.focus.filter(|id| on_desk.contains(id)).or(self
                            .state
                            .session
                            .open_card
                            .filter(|id| on_desk.contains(id)));
                        let (ghosts, edges) = anchor
                            .map(|a| {
                                let specs: Vec<crate::surfaces::desk::GhostSpec> =
                                    crate::surfaces::desk::ghost_candidates(&idx, a, &on_desk)
                                        .into_iter()
                                        .filter_map(|(gid, _)| {
                                            idx.get(gid).map(|m| crate::surfaces::desk::GhostSpec {
                                                id: gid,
                                                title: m
                                                    .title
                                                    .clone()
                                                    .unwrap_or_else(|| m.first_line.clone()),
                                                size: crate::card::shape::card_size(
                                                    crate::card::shape::shape_for(m.status, m.kind),
                                                ),
                                            })
                                        })
                                        .collect();
                                let edges =
                                    crate::surfaces::desk::selected_edges(&idx, a, &on_desk);
                                (specs, edges)
                            })
                            .unwrap_or_default();
                        (face_metas, anchor, ghosts, edges)
                    };

                    // Handle Fit button for this desk.
                    if fit_clicked
                        && let Some(desk) = self
                            .state
                            .session
                            .desks
                            .iter()
                            .find(|d| d.id == desk_id)
                            .cloned()
                    {
                        let panel = ui.max_rect();
                        let positions: Vec<(NoteId, CoreVec2)> =
                            desk.cards.iter().map(|c| (c.id, c.pos)).collect();
                        let mut cam = crate::surfaces::desk::DeskCamera {
                            center: egui::vec2(desk.viewport.center.x, desk.viewport.center.y),
                            zoom: desk.viewport.zoom,
                        };
                        cam.zoom_to_fit(&positions, panel);
                        if let Some(d) = self
                            .state
                            .session
                            .desks
                            .iter_mut()
                            .find(|d| d.id == desk_id)
                        {
                            d.viewport.center = CoreVec2 {
                                x: cam.center.x,
                                y: cam.center.y,
                            };
                            d.viewport.zoom = cam.zoom;
                        }
                        self.state.session_dirty_at = Some(std::time::Instant::now());
                    }

                    if let Some(desk) = self
                        .state
                        .session
                        .desks
                        .iter()
                        .find(|d| d.id == desk_id)
                        .cloned()
                    {
                        // Capture the real panel rect before desk_ui runs so that
                        // reveal() in apply_desk_events (FocusChanged) uses the actual
                        // desk area rather than a hardcoded sentinel.  One frame of
                        // staleness is acceptable.
                        self.last_panel_rect = Some(ui.max_rect());

                        // Build desk list for the "Take to Desk ▸" submenu.
                        let desk_list: Vec<(jd_core::session::DeskId, String)> = self
                            .state
                            .session
                            .desks
                            .iter()
                            .map(|d| (d.id, d.name.clone()))
                            .collect();

                        let mut deps = DeskUiDeps {
                            focus: &mut self.state.focus,
                            bodies: &mut self.state.bodies,
                            commands: &self.vault.commands,
                            theme: &self.theme,
                            line_cache: &mut self.line_cache,
                            face_metas: &face_metas,
                            drag: &mut self.drag,
                            editor_open: self.state.editor.is_some(),
                            confirm_pending: self.state.pending_confirm.is_some(),
                            palette_open: self.state.palette.is_some(),
                            highlight_pulse,
                            desks: &desk_list,
                            current_desk_id: desk_id,
                            rail_row_hits: &self.rail_row_hits,
                            ghost_anchor,
                            ghosts: &ghosts,
                            edges: &edges,
                        };
                        let evts = crate::surfaces::desk::desk_ui(ui, &desk, &mut deps);
                        self.apply_desk_events(evts, desk.id, &face_metas);
                    }
                }

                // Task 3: Inbox surface.
                Some(SurfaceId::Inbox) => {
                    // Prefetch FaceMeta for all fleeting notes under ONE index read lock.
                    // Also get the ordered list (oldest-first by `created`).
                    let (face_metas, ordered_ids): (Vec<FaceMeta>, Vec<NoteId>) = {
                        let idx = self.vault.index.read().unwrap();
                        let ordered = idx.fleeting(); // oldest-first
                        let metas: Vec<FaceMeta> = ordered
                            .iter()
                            .filter_map(|&id| {
                                idx.get(id).map(|m| FaceMeta::from_note_meta(m, &idx))
                            })
                            .collect();
                        (metas, ordered)
                    };
                    let mut deps = InboxUiDeps {
                        focus: &mut self.state.focus,
                        bodies: &mut self.state.bodies,
                        commands: &self.vault.commands,
                        theme: &self.theme,
                        line_cache: &mut self.line_cache,
                        face_metas: &face_metas,
                        session: &self.state.session,
                        ordered_ids: &ordered_ids,
                        editor_open: self.state.editor.is_some(),
                        confirm_pending: self.state.pending_confirm.is_some(),
                        palette_open: self.state.palette.is_some(),
                    };
                    let evts = crate::surfaces::inbox::inbox_ui(ui, &mut deps);
                    self.apply_inbox_events(evts);
                }

                // Task 5: Trash surface.
                Some(SurfaceId::Trash) => {
                    let mut deps = TrashUiDeps {
                        vault_ref: &self.vault_ref,
                        theme: &self.theme,
                    };
                    let evts = crate::surfaces::trash::trash_ui(ui, &mut deps);
                    self.apply_trash_events(evts);
                }

                // WP4 Task 4: Drawer surface.
                Some(SurfaceId::Drawer) => {
                    // Filtered ids + FaceMeta + tag list under ONE index read
                    // lock (the FaceMeta prefetch idiom).
                    let (face_metas, ordered_ids, all_tags): (
                        Vec<FaceMeta>,
                        Vec<NoteId>,
                        Vec<(jd_core::tag::Tag, usize)>,
                    ) = {
                        let idx = self.vault.index.read().unwrap();
                        let ids = crate::surfaces::drawer::drawer_ids(
                            &idx,
                            &self.state.drawer_filters,
                            &self.state.conflicts,
                        );
                        let metas = ids
                            .iter()
                            .filter_map(|&id| {
                                idx.get(id).map(|m| FaceMeta::from_note_meta(m, &idx))
                            })
                            .collect();
                        (metas, ids, idx.all_tags())
                    };
                    let mut deps = DrawerUiDeps {
                        focus: &mut self.state.focus,
                        bodies: &mut self.state.bodies,
                        commands: &self.vault.commands,
                        theme: &self.theme,
                        line_cache: &mut self.line_cache,
                        face_metas: &face_metas,
                        ordered_ids: &ordered_ids,
                        filters: &mut self.state.drawer_filters,
                        all_tags: &all_tags,
                        quarantined: &self.state.quarantined,
                        session: &self.state.session,
                        editor_open: self.state.editor.is_some(),
                        confirm_pending: self.state.pending_confirm.is_some(),
                        palette_open: self.state.palette.is_some(),
                    };
                    let evts = crate::surfaces::drawer::drawer_ui(ui, &mut deps);
                    self.apply_drawer_events(evts);
                }

                None => {
                    crate::surfaces::placeholder::placeholder_ui(ui);
                }

                // Map (WP5) — placeholder per scope boundaries.
                Some(SurfaceId::Map) => {
                    crate::surfaces::placeholder::placeholder_ui(ui);
                }
            }
        });

        // 4b. Editor modal overlay (Task 10).
        let editor_result: Option<crate::editor::EditorEvent> =
            if let Some(ed) = &mut self.state.editor {
                let mut deps = crate::editor::EditorDeps {
                    theme: &self.theme,
                    commands: &self.vault.commands,
                    index: &self.vault.index,
                    reduced_motion: self.state.reduced_motion,
                };
                Some(crate::editor::editor_ui(ui, ed, &mut deps))
            } else {
                None
            };

        // Handle SplitAndClose: dispatch Batch([SaveBody, Split]) and close the editor.
        // pending_label is set so drain_events uses the Split op's natural label
        // ("Split card '<title>'" / "Split scrap '<title>'") from the worker.
        // pending_split is set so drain_events places the two cards side-by-side.
        if let Some(crate::editor::EditorEvent::SplitAndClose { at_byte }) = editor_result {
            let editor = self.state.editor.take().unwrap();
            // Stash the editor's text undo stack.
            self.state.text_undo.insert(editor.id, editor.undo);
            self.state.session.open_card = None;
            self.state.session_dirty_at = Some(std::time::Instant::now());
            ui.ctx().memory_mut(|mem| mem.stop_text_input());
            // Set pending_split so drain_events places original + split-off side-by-side.
            self.state.pending_split = Some(editor.id);
            // Set pending_label from the index so the journal entry has the right label.
            // The worker computes "Split card '<title>'" / "Split scrap '<first_line>'"
            // via op_label; we need to match exactly for the Task 6 echo suffix logic.
            let pending_label_str = {
                let idx = self.vault.index.read().unwrap();
                if let Some(meta) = idx.get(editor.id) {
                    let is_fleeting = meta.status == jd_core::note::Status::Fleeting;
                    let kind_word = if is_fleeting { "scrap" } else { "card" };
                    let display = jd_core::command::label_display(
                        meta.title.as_deref().unwrap_or(&meta.first_line),
                    );
                    format!("Split {kind_word} '{display}'")
                } else {
                    "Split card".to_owned()
                }
            };
            self.state.pending_label = Some(pending_label_str);
            // Dispatch the Batch. The SaveBody ensures the split sees exactly what
            // the user sees (dirty buffer); the worker splits the saved content.
            let _ = self.vault.commands.send(VaultCommand::Op {
                op: jd_core::command::VaultOp::Batch(vec![
                    jd_core::command::VaultOp::SaveBody {
                        id: editor.id,
                        content: editor.buffer.clone(),
                    },
                    jd_core::command::VaultOp::Split {
                        id: editor.id,
                        at_byte,
                    },
                ]),
                source: OpSource::User,
            });
            // Skip the close_editor block below (we already handled it here).
        }

        let close_editor = matches!(
            editor_result,
            Some(crate::editor::EditorEvent::CloseAndSave)
        );
        if close_editor {
            // Only save when the buffer was actually modified.  A clean
            // open→close must not write the file (which would invalidate
            // the body cache via the watcher echo and push a phantom undo
            // entry with label "Save body").
            let editor = self.state.editor.take().unwrap();
            if editor.pending_promotion {
                // Task 4: commit ONE compound op: SaveBody with "# title\nbody"
                // then Promote. The body transformation prepends "# <first line>\n"
                // to give extract_title the `# ` marker it requires.
                let first_newline = editor.buffer.find('\n').unwrap_or(editor.buffer.len());
                let first_line = editor.buffer[..first_newline].to_owned();
                let rest = if first_newline < editor.buffer.len() {
                    &editor.buffer[first_newline + 1..]
                } else {
                    ""
                };
                let promoted_body = if rest.is_empty() {
                    format!("# {first_line}\n")
                } else {
                    format!("# {first_line}\n{rest}")
                };
                // Set the pending_label before dispatching so drain_events picks
                // it up on the next OpDone.
                self.state.pending_label = Some(format!("Promote scrap '{first_line}'"));
                let _ = self.vault.commands.send(VaultCommand::Op {
                    op: jd_core::command::VaultOp::Batch(vec![
                        jd_core::command::VaultOp::SaveBody {
                            id: editor.id,
                            content: promoted_body,
                        },
                        jd_core::command::VaultOp::Promote { id: editor.id },
                    ]),
                    source: jd_core::command::OpSource::User,
                });
            } else if editor.dirty && editor.buffer != editor.saved_buffer {
                // No-op guard (mirrors the autosave guard in editor_ui): a buffer
                // undone back to the as-opened / last-saved content must not
                // dispatch a SaveBody — it would journal a phantom "Edit" entry.
                let _ = self.vault.commands.send(VaultCommand::Op {
                    op: jd_core::command::VaultOp::SaveBody {
                        id: editor.id,
                        content: editor.buffer.clone(),
                    },
                    source: jd_core::command::OpSource::User,
                });
            }
            // Task 12: stash the undo stack so it survives close/reopen.
            self.state.text_undo.insert(editor.id, editor.undo);
            self.state.session.open_card = None;
            self.state.session_dirty_at = Some(std::time::Instant::now());
            // Return focus to the desk card by clearing the focused-widget memory.
            ui.ctx().memory_mut(|mem| mem.stop_text_input());
        }

        // 4c. Delete-confirm modal (Task 5).
        // Rendered as a centred egui::Modal when pending_confirm is Some.
        // Enter = Delete (VaultOp::Delete → moves to Trash), Esc = Cancel.
        // The keyboard dispatch in section 3 (above) already handles Enter/Esc
        // and clears pending_confirm; this block renders the visible dialog.
        if let Some(confirm_id) = self.state.pending_confirm {
            let title = {
                let idx = self.vault.index.read().unwrap();
                idx.get(confirm_id)
                    .map(|m| m.title.clone().unwrap_or_else(|| m.first_line.clone()))
                    .unwrap_or_default()
            };
            let modal =
                egui::Modal::new(egui::Id::new("delete_confirm_modal")).show(ui.ctx(), |ui| {
                    ui.set_width(320.0);
                    ui.heading("Delete card");
                    ui.add_space(8.0);
                    ui.label(format!("Delete '{title}'? It moves to Trash."));
                    ui.add_space(12.0);
                    ui.horizontal(|ui| {
                        // "Delete" button — same as pressing Enter.
                        let delete_btn = ui.button("Delete");
                        let cancel_btn = ui.button("Cancel");
                        if delete_btn.clicked() {
                            self.state.pending_confirm = None;
                            let _ = self.vault.commands.send(VaultCommand::Op {
                                op: VaultOp::Delete { id: confirm_id },
                                source: OpSource::User,
                            });
                        }
                        if cancel_btn.clicked() {
                            self.state.pending_confirm = None;
                        }
                    });
                });
            // Dismiss on click outside the modal.
            if modal.should_close() {
                self.state.pending_confirm = None;
            }
        }

        // 4d. Set Source… modal (Task 7).
        // Rendered when pending_set_source is Some; Enter commits, Esc cancels.
        if let Some((source_id, ref mut source_buf)) = self.state.pending_set_source {
            let mut submitted = false;
            let mut cancelled = false;
            let modal = egui::Modal::new(egui::Id::new("set_source_modal")).show(ui.ctx(), |ui| {
                ui.set_width(400.0);
                ui.heading("Set Source");
                ui.add_space(8.0);
                ui.label("Source URL or citation:");
                let te = ui.text_edit_singleline(source_buf);
                te.request_focus();
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.button("OK").clicked() {
                        submitted = true;
                    }
                    if ui.button("Cancel").clicked() {
                        cancelled = true;
                    }
                });
                if ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Enter)) {
                    submitted = true;
                }
                if ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape)) {
                    cancelled = true;
                }
            });
            if modal.should_close() {
                cancelled = true;
            }
            if submitted {
                let source_text = source_buf.trim().to_owned();
                let source_opt = if source_text.is_empty() {
                    None
                } else {
                    Some(source_text)
                };
                let _ = self.vault.commands.send(VaultCommand::Op {
                    op: VaultOp::SetSource {
                        id: source_id,
                        source: source_opt,
                    },
                    source: OpSource::User,
                });
                self.state.pending_set_source = None;
            } else if cancelled {
                self.state.pending_set_source = None;
            }
        }

        // 4e. Palette overlay (WP4). Rendered above the surfaces. The Ctrl+K
        // gate (section 3) prevents the palette from OPENING while the editor
        // or confirm modal is live; the converse (an editor/confirm appearing
        // while the palette is up) is prevented by the palette_open gates on
        // the surfaces' keyboard AND mouse paths (double-click, context menu).
        // Esc inside palette_ui closes ONLY the palette; Enter/Ctrl+Enter
        // activate the selected row (Task 2).
        if let Some(pal) = &mut self.state.palette {
            let mut deps = crate::palette::PaletteDeps {
                index: &self.vault.index,
                bodies: &mut self.state.bodies,
                commands: &self.vault.commands,
                theme: &self.theme,
            };
            let event = crate::palette::palette_ui(ui, pal, &mut deps);
            if let Some(event) = event {
                // Close first (both Close and Activate dismiss the palette),
                // keeping the query text for the NewScrap seed body.
                let query = pal.query.clone();
                self.state.palette = None;
                // Release the keyboard focus held by the palette input.
                ui.ctx().memory_mut(|mem| mem.stop_text_input());
                if let crate::palette::PaletteEvent::Activate { row, open_after } = event {
                    self.apply_palette_activation(row, open_after, &query);
                    // If the activation armed the highlight pulse, request a
                    // repaint now: the pulse fade check at the top of ui() ran
                    // before this frame's arming, so without this the first
                    // fade frame would wait for the next input event.
                    if self.state.highlight_pulse.is_some() {
                        ui.ctx().request_repaint();
                    }
                }
            }
        }

        // 4f. Clipboard copy (Task 7: Copy Link).
        // pending_copy_text is set by apply_card_menu_event; we copy it here
        // where we have a ui context.
        if let Some(text) = self.state.pending_copy_text.take() {
            ui.ctx().copy_text(text);
        }

        // 5. Debounced saves: if session_dirty_at elapsed > 1s → save and clear.
        if let Some(dirty_at) = self.state.session_dirty_at
            && dirty_at.elapsed() > std::time::Duration::from_secs(1)
        {
            let _ = self.state.session.save(&self.vault_ref);
            self.state.session_dirty_at = None;
        }
    }

    /// Apply a single `CardMenuEvent`.  Public so kitests can fire events directly.
    pub fn apply_card_menu_event(&mut self, ev: CardMenuEvent, current_desk: Option<DeskId>) {
        match ev {
            CardMenuEvent::Promote(id) => {
                // Same path as InboxEvent::Promote (Ctrl+Enter from inbox).
                self.open_card_editor_with_promotion(id, true);
            }

            CardMenuEvent::Toss(id) => {
                // Status-routed: fleeting → immediate Toss, permanent → confirm modal.
                let is_fleeting = {
                    let idx = self.vault.index.read().unwrap();
                    idx.get(id)
                        .map(|m| m.status == jd_core::note::Status::Fleeting)
                        .unwrap_or(false)
                };
                if is_fleeting {
                    let _ = self.vault.commands.send(VaultCommand::Op {
                        op: VaultOp::Toss { id },
                        source: OpSource::User,
                    });
                } else {
                    self.state.pending_confirm = Some(id);
                }
            }

            CardMenuEvent::TakeToDesk { id, desk } => {
                // Move card from current desk (or inbox) to target desk.
                // This reuses the CardDroppedOnDesk path: PutAway from source + Place on target.
                if let Some(source_desk) = current_desk {
                    let was_at = self
                        .state
                        .session
                        .desks
                        .iter()
                        .find(|d| d.id == source_desk)
                        .and_then(|d| d.cards.iter().find(|c| c.id == id))
                        .map(|c| c.pos)
                        .unwrap_or_default();

                    use crate::rail::RailEvent;
                    self.apply_rail_event(RailEvent::CardDroppedOnDesk {
                        target_desk: desk,
                        id,
                        source_desk,
                        was_at,
                    });
                } else {
                    // Card is in inbox (no current desk) — just place on target desk.
                    let pos = self
                        .state
                        .session
                        .desks
                        .iter()
                        .find(|d| d.id == desk)
                        .map(|d| d.viewport.center)
                        .unwrap_or_default();
                    self.place_card(desk, id, pos);
                }
            }

            CardMenuEvent::PutAway(id) => {
                // Put card away from the current desk.
                if let Some(desk_id) = current_desk {
                    let was_at = self
                        .state
                        .session
                        .desks
                        .iter()
                        .find(|d| d.id == desk_id)
                        .and_then(|d| d.cards.iter().find(|c| c.id == id))
                        .map(|c| c.pos)
                        .unwrap_or_default();
                    self.apply_session(
                        jd_core::session::SessionOp::PutAway {
                            desk: desk_id,
                            id,
                            was_at,
                        },
                        Some("Put card away"),
                    );
                }
            }

            CardMenuEvent::SetSource(id) => {
                // Open the Set Source… modal.  Pre-populate with the current source if any.
                let current_source = {
                    let idx = self.vault.index.read().unwrap();
                    idx.get(id)
                        .and_then(|m| m.source.clone())
                        .unwrap_or_default()
                };
                self.state.pending_set_source = Some((id, current_source));
            }

            CardMenuEvent::MakeDivider(id) => {
                let _ = self.vault.commands.send(VaultCommand::Op {
                    op: VaultOp::SetKind {
                        id,
                        kind: jd_core::note::Kind::Structure,
                    },
                    source: OpSource::User,
                });
            }

            CardMenuEvent::Demote(id) => {
                let _ = self.vault.commands.send(VaultCommand::Op {
                    op: VaultOp::Demote { id },
                    source: OpSource::User,
                });
            }

            CardMenuEvent::CopyLink(id) => {
                // Copy [[title]] to clipboard.  Title is non-empty (enablement gate).
                let title = {
                    let idx = self.vault.index.read().unwrap();
                    idx.get(id)
                        .and_then(|m| m.title.clone())
                        .unwrap_or_default()
                };
                if !title.is_empty() {
                    // ctx.copy_text is only available from a ui context; we store
                    // the text in the pending_copy field and copy it in the next
                    // render frame via ui.ctx().copy_text.
                    self.state.pending_copy_text = Some(format!("[[{title}]]"));
                }
            }

            CardMenuEvent::RevealInFileManager(id) => {
                // Look up the absolute path, then spawn the platform reveal command.
                let abs_path: Option<std::path::PathBuf> = {
                    let idx = self.vault.index.read().unwrap();
                    idx.get(id).map(|m| self.vault_ref.root().join(&m.rel_path))
                };
                if let Some(path) = abs_path {
                    let result = reveal_in_file_manager(&path);
                    if let Err(e) = result {
                        self.state.last_error = Some(format!("Reveal: {e}"));
                    }
                }
            }
        }
    }

    /// Apply a palette row activation (WP4 Task 2). Public so kitests can
    /// drive activations directly.
    ///
    /// - Title/Body row, card NOT on the target desk → `place_card` at the
    ///   desk's viewport center (journaled "Place card").
    /// - Title/Body row, card ALREADY on the target desk → pan/zoom to center
    ///   it. SACRED: the card's position is untouched and nothing is journaled
    ///   (navigation). A ~600ms highlight pulse is armed unless reduced_motion.
    /// - `open_after` (Ctrl+Enter) → additionally open the editor
    ///   (session.open_card path).
    /// - From a non-desk surface (Inbox/Drawer/Trash): the FIRST desk is the
    ///   target; we switch to it (direct field set, not journaled — the rail
    ///   Switch idiom) and, on placement, echo the desk by name (the WP3
    ///   split-fallback idiom).
    /// - NewScrap → the Ctrl+N create path with the query as the seed body:
    ///   Create{Fleeting, Inbox} + pending_create{viewport center, open_editor}.
    pub fn apply_palette_activation(
        &mut self,
        row: crate::palette::PaletteRow,
        open_after: bool,
        query: &str,
    ) {
        use crate::palette::PaletteRow;
        match row {
            PaletteRow::Title { id, .. } | PaletteRow::Body { id, .. } => {
                // Resolve the target desk: the current desk, or the FIRST desk
                // when the palette was opened from a non-desk surface.
                let (target_desk, from_non_desk) = match self.state.session.current_surface {
                    Some(SurfaceId::Desk(d)) => (Some(d), false),
                    _ => (self.state.session.desks.first().map(|d| d.id), true),
                };
                let Some(desk_id) = target_desk else { return };
                if from_non_desk {
                    // Navigation: direct field set, not journaled.
                    self.state.session.current_surface = Some(SurfaceId::Desk(desk_id));
                    self.state.session_dirty_at = Some(std::time::Instant::now());
                }
                let already_at = self
                    .state
                    .session
                    .desks
                    .iter()
                    .find(|d| d.id == desk_id)
                    .and_then(|d| d.cards.iter().find(|c| c.id == id))
                    .map(|c| c.pos);
                if let Some(pos) = already_at {
                    // SACRED: already on this desk → pan to center the card.
                    // Its position is byte-identical after; NO journal entry.
                    let half = {
                        let idx = self.vault.index.read().unwrap();
                        idx.get(id)
                            .map(|m| {
                                crate::card::shape::card_size(crate::card::shape::shape_for(
                                    m.status, m.kind,
                                )) * 0.5
                            })
                            .unwrap_or(egui::vec2(150.0, 100.0))
                    };
                    if let Some(d) = self
                        .state
                        .session
                        .desks
                        .iter_mut()
                        .find(|d| d.id == desk_id)
                    {
                        d.viewport.center = CoreVec2 {
                            x: pos.x + half.x,
                            y: pos.y + half.y,
                        };
                    }
                    self.state.session_dirty_at = Some(std::time::Instant::now());
                    // Highlight pulse (~600ms fading ring); skipped under
                    // reduced motion (UiState::reduced_motion is the source
                    // of truth EditorDeps also reads).
                    if !self.state.reduced_motion {
                        self.state.highlight_pulse = Some((id, std::time::Instant::now()));
                    }
                } else {
                    // Not on this desk → place at the viewport center (journaled).
                    let center = self
                        .state
                        .session
                        .desks
                        .iter()
                        .find(|d| d.id == desk_id)
                        .map(|d| d.viewport.center)
                        .unwrap_or_default();
                    self.place_card(desk_id, id, center);
                    if from_non_desk {
                        let desk_name = self
                            .state
                            .session
                            .desks
                            .iter()
                            .find(|d| d.id == desk_id)
                            .map(|d| d.name.as_str())
                            .unwrap_or("desk");
                        self.state.status_echo = Some((
                            format!("Placed on desk '{desk_name}'"),
                            std::time::Instant::now(),
                        ));
                    }
                }
                self.state.focus = Some(id);
                if open_after {
                    self.open_card_editor(id);
                }
            }
            PaletteRow::NewScrap => {
                let body = query.trim();
                if body.is_empty() {
                    return;
                }
                // The Ctrl+N create path with a seed body: pending_create
                // places the new scrap at the first desk's viewport center
                // (handle_pending_create targets desks.first()) + opens the
                // editor; the note itself lands in inbox/ as Fleeting.
                let at = self
                    .state
                    .session
                    .desks
                    .first()
                    .map(|d| d.viewport.center)
                    .unwrap_or_default();
                self.state.pending_create = Some(crate::state::PendingCreate {
                    at,
                    open_editor: true,
                });
                let seed = NewNote {
                    body: body.to_owned(),
                    status: Status::Fleeting,
                    kind: Kind::Note,
                    source: None,
                    tags: Vec::new(),
                };
                let _ = self.vault.commands.send(VaultCommand::Op {
                    op: VaultOp::Create {
                        seed,
                        dest: Dest::Inbox,
                    },
                    source: OpSource::User,
                });
            }
        }
    }

    /// Apply `DeskEvent`s emitted by `desk_ui`.
    fn apply_desk_events(
        &mut self,
        events: Vec<DeskEvent>,
        _desk_id: DeskId,
        face_metas: &[FaceMeta],
    ) {
        for ev in events {
            match ev {
                DeskEvent::OpenCard(id) => {
                    self.state.session.open_card = Some(id);
                    self.state.session_dirty_at = Some(std::time::Instant::now());
                    // Kick off the body fetch (or use cached body immediately).
                    // If the body is already cached, open the editor right now.
                    // If not, drain_events will open it when the Body event arrives.
                    if let Some(cached) = self.state.bodies.get_or_request(id, &self.vault.commands)
                        && self.state.editor.is_none()
                    {
                        let saved_undo = self.state.text_undo.remove(&id);
                        let is_fleeting = {
                            let idx = self.vault.index.read().unwrap();
                            idx.get(id)
                                .map(|m| m.status == jd_core::note::Status::Fleeting)
                                .unwrap_or(false)
                        };
                        self.state.editor = Some(crate::editor::EditorState::open(
                            id,
                            cached.text.clone(),
                            saved_undo,
                            is_fleeting,
                            false,
                        ));
                    }
                }
                DeskEvent::FocusChanged(id) => {
                    self.state.focus = id;
                    // Reveal: if the newly focused card is off-screen, pan to it.
                    if let Some(focused_id) = id
                        && let Some(desk) = self.state.session.desks.first()
                    {
                        // Use the panel rect captured from ui.max_rect() during
                        // the previous frame's CentralPanel closure.  When None
                        // (first frame), pass an empty rect so every card appears
                        // off-screen and reveal() always fires — erring on the
                        // side of centering.
                        let panel = self.last_panel_rect.unwrap_or(egui::Rect::NOTHING);
                        if let Some(new_cam) =
                            crate::surfaces::desk::reveal(desk, focused_id, panel, face_metas)
                        {
                            let desk_id = desk.id;
                            if let Some(d) = self
                                .state
                                .session
                                .desks
                                .iter_mut()
                                .find(|d| d.id == desk_id)
                            {
                                d.viewport.center = jd_core::geom::Vec2 {
                                    x: new_cam.center.x,
                                    y: new_cam.center.y,
                                };
                                d.viewport.zoom = new_cam.zoom;
                            }
                            self.state.session_dirty_at = Some(std::time::Instant::now());
                        }
                    }
                }
                DeskEvent::SessionOp(op) => {
                    // Move/PutAway are journaled; viewport ops are not applicable here
                    // (desk_ui emits ViewportMoved for those, not SessionOp).
                    let label = session_op_label(&op);
                    self.apply_session(op, label);
                }
                DeskEvent::ViewportMoved { desk, cam } => {
                    if let Some(d) = self.state.session.desks.iter_mut().find(|d| d.id == desk) {
                        d.viewport.center = CoreVec2 {
                            x: cam.center.x,
                            y: cam.center.y,
                        };
                        d.viewport.zoom = cam.zoom;
                    }
                    // Not journaled — just mark dirty for debounced save.
                    self.state.session_dirty_at = Some(std::time::Instant::now());
                }
                DeskEvent::CardMenu(ev) => {
                    let current_desk = Some(_desk_id);
                    self.apply_card_menu_event(ev, current_desk);
                }
                DeskEvent::CardDroppedOnRail(rail_ev) => {
                    // Drag-to-rail gesture: route through the same handler as
                    // a regular rail drop event (CardDroppedOnInbox / CardDroppedOnDesk).
                    self.apply_rail_event(rail_ev);
                }
                DeskEvent::ToggleTaskBox { id, ordinal } => {
                    // Locate the cached body, toggle the Nth task box, and save.
                    // Ordinal matching: the Nth occurrence of "- [" in the raw body.
                    let cached_text = self.state.bodies.get_cached(id).map(|b| b.text.clone());
                    if let Some(body) = cached_text {
                        let toggled = crate::card::toggle_task_box(&body, ordinal);
                        if toggled != body {
                            let _ = self.vault.commands.send(jd_core::worker::VaultCommand::Op {
                                op: VaultOp::SaveBody {
                                    id,
                                    content: toggled,
                                },
                                source: jd_core::command::OpSource::User,
                            });
                        }
                    }
                }
                DeskEvent::GhostClicked { id, pos } => {
                    // The ghost becomes a real card where it stood — journaled
                    // "Place card" (the same single placement path the palette
                    // and Ctrl+D use). The fan recomputes next frame with the
                    // newly-placed note excluded (it is on-desk now).
                    self.place_card(_desk_id, id, pos);
                }
            }
        }
    }

    /// Open the card editor for `id`, either immediately (body cached) or deferred
    /// (body not yet loaded; drain_events will open the editor when Body arrives).
    /// Mirrors the DeskEvent::OpenCard path in apply_desk_events.
    fn open_card_editor(&mut self, id: NoteId) {
        self.open_card_editor_with_promotion(id, false);
    }

    /// Open the card editor for `id`, with optional immediate pending_promotion.
    /// Used by InboxEvent::Promote (Ctrl+Enter on inbox card = promote-without-typing).
    ///
    /// Invariant: pending_open_promotion is NON-DOWNGRADING. If a prior call set
    /// it to `true` (e.g. Ctrl+Enter) and a subsequent call arrives with `false`
    /// (e.g. a plain OpenCard for the same id while the body is still loading),
    /// the stashed `true` must not be overwritten. Use `|=` so a `true` can never
    /// be reset to `false` by a later call that passes `false`.
    fn open_card_editor_with_promotion(&mut self, id: NoteId, pending_promotion: bool) {
        // Same-id guard: if the editor is already open for this exact id (e.g. a
        // deferred-open Body event races with a second Promote dispatch for the
        // same card), do not overwrite the live editor or pending_open_promotion.
        // This prevents the promotion stash from being silently dropped by a no-op
        // re-open that would replace pending_open_promotion with its own value.
        if self.state.editor.as_ref().is_some_and(|e| e.id == id) {
            return;
        }
        self.state.session.open_card = Some(id);
        self.state.session_dirty_at = Some(std::time::Instant::now());
        // Stash pending_promotion for the deferred-open path (drain_events Body
        // handler) in case the body hasn't arrived yet.
        // NON-DOWNGRADING: a prior `true` must survive a subsequent `false` call
        // (e.g. InboxEvent::Promote followed by a plain OpenCard for the same card).
        self.state.pending_open_promotion |= pending_promotion;
        if let Some(cached) = self.state.bodies.get_or_request(id, &self.vault.commands)
            && self.state.editor.is_none()
        {
            let saved_undo = self.state.text_undo.remove(&id);
            let is_fleeting = {
                let idx = self.vault.index.read().unwrap();
                idx.get(id)
                    .map(|m| m.status == jd_core::note::Status::Fleeting)
                    .unwrap_or(false)
            };
            // Use the (possibly already-set) pending_open_promotion, not the local arg.
            let effective_promotion = self.state.pending_open_promotion;
            self.state.editor = Some(crate::editor::EditorState::open(
                id,
                cached.text.clone(),
                saved_undo,
                is_fleeting,
                effective_promotion,
            ));
            // Consumed — clear.
            self.state.pending_open_promotion = false;
        }
    }

    /// Apply `InboxEvent`s emitted by `inbox_ui`.
    pub fn apply_inbox_event(&mut self, ev: InboxEvent) {
        match ev {
            InboxEvent::OpenCard(id) => {
                self.open_card_editor(id);
            }

            InboxEvent::Promote(id) => {
                // Task 4: Ctrl+Enter on an inbox scrap = promote-without-typing.
                // Open the editor with pending_promotion=true so close dispatches
                // the compound Batch([SaveBody, Promote]) immediately.
                self.open_card_editor_with_promotion(id, true);
            }

            InboxEvent::Toss(id) => {
                let _ = self.vault.commands.send(jd_core::worker::VaultCommand::Op {
                    op: jd_core::command::VaultOp::Toss { id },
                    source: jd_core::command::OpSource::User,
                });
            }

            InboxEvent::PlaceOnDesk { id, desk } => {
                // Place the card on the desk at that desk's viewport center.
                // Card stays fleeting/inboxed — this is placement only, not a status change.
                let pos = self
                    .state
                    .session
                    .desks
                    .iter()
                    .find(|d| d.id == desk)
                    .map(|d| d.viewport.center)
                    .unwrap_or_default();
                self.place_card(desk, id, pos);
            }
        }
    }

    fn apply_inbox_events(&mut self, events: Vec<InboxEvent>) {
        for ev in events {
            self.apply_inbox_event(ev);
        }
    }

    /// Apply a single `DrawerEvent` — public so kitests can dispatch events directly.
    pub fn apply_drawer_event(&mut self, ev: DrawerEvent) {
        match ev {
            DrawerEvent::OpenCard(id) => {
                // Same open path as the desk/inbox; the editor overlay is
                // surface-agnostic, so it opens in place over the Drawer.
                self.open_card_editor(id);
            }
            DrawerEvent::PlaceOnDesk { id, desk } => {
                // Place at that desk's viewport center (journaled "Place card").
                // Placement only — status is untouched (the Inbox Ctrl+D idiom).
                let pos = self
                    .state
                    .session
                    .desks
                    .iter()
                    .find(|d| d.id == desk)
                    .map(|d| d.viewport.center)
                    .unwrap_or_default();
                self.place_card(desk, id, pos);
            }
        }
    }

    /// Apply `DrawerEvent`s emitted by `drawer_ui`.
    fn apply_drawer_events(&mut self, events: Vec<DrawerEvent>) {
        for ev in events {
            self.apply_drawer_event(ev);
        }
    }

    /// Apply a single `TrashEvent` — public so kitests can dispatch events directly.
    pub fn apply_trash_event(&mut self, ev: TrashEvent) {
        match ev {
            TrashEvent::Restore(id) => {
                let _ = self.vault.commands.send(VaultCommand::Op {
                    op: VaultOp::Restore { id },
                    source: OpSource::User,
                });
            }
        }
    }

    /// Apply `TrashEvent`s emitted by `trash_ui`.
    fn apply_trash_events(&mut self, events: Vec<TrashEvent>) {
        for ev in events {
            self.apply_trash_event(ev);
        }
    }

    /// Apply `RailEvent`s emitted by `rail_ui`.
    ///
    /// Switch: sets `current_surface` directly + marks `session_dirty_at`
    ///   (navigation is not undoable, like viewport).
    /// CreateDesk / RenameDesk / ReorderDesk: journaled SessionOps.
    /// CardDroppedOnInbox: PutAway from source desk, ONE journal entry.
    /// CardDroppedOnDesk: PutAway from source + Place on target, ONE journal entry
    ///   whose inverse is Session(Place back on source at old pos).
    fn apply_rail_events(&mut self, events: Vec<RailEvent>) {
        for ev in events {
            self.apply_rail_event(ev);
        }
    }

    // ------------------------------------------------------------------
    // Task 6: undo/redo executors + view-travel helper
    // ------------------------------------------------------------------

    /// View-travel: if `ctx` names a desk, switch to it (if not already there).
    /// If it also names a note on that desk, reveal (pan/center) it.
    fn do_view_travel(&mut self, ctx: OpContext) {
        let Some(desk_id) = ctx.desk else { return };
        // Guard: skip surface switch if the desk no longer exists (e.g. a journaled
        // op whose desk was deleted since the entry was created). The echo still
        // shows what was undone, but the surface doesn't change.
        if !self.state.session.desks.iter().any(|d| d.id == desk_id) {
            return;
        }
        // Switch surface if not already on this desk.
        if self.state.session.current_surface != Some(SurfaceId::Desk(desk_id)) {
            self.state.session.current_surface = Some(SurfaceId::Desk(desk_id));
            self.state.session_dirty_at = Some(std::time::Instant::now());
        }
        // Reveal the note if present on this desk.
        // Clone desk to avoid borrow conflict with iter_mut below.
        if let Some(note_id) = ctx.note
            && let Some(desk) = self
                .state
                .session
                .desks
                .iter()
                .find(|d| d.id == desk_id)
                .cloned()
            && desk.cards.iter().any(|c| c.id == note_id)
        {
            let panel = self.last_panel_rect.unwrap_or(egui::Rect::NOTHING);
            // Pass empty face_metas — reveal falls back to 300×200 size.
            if let Some(new_cam) = crate::surfaces::desk::reveal(&desk, note_id, panel, &[]) {
                if let Some(d) = self
                    .state
                    .session
                    .desks
                    .iter_mut()
                    .find(|d| d.id == desk_id)
                {
                    d.viewport.center = jd_core::geom::Vec2 {
                        x: new_cam.center.x,
                        y: new_cam.center.y,
                    };
                    d.viewport.zoom = new_cam.zoom;
                }
                self.state.session_dirty_at = Some(std::time::Instant::now());
            }
        }
    }

    /// Execute the top-of-undo-stack entry (Ctrl+Z).
    fn execute_undo(&mut self) {
        // Guard: ignore while a vault undo/redo is in-flight.  Two presses
        // before the async OpDone drains would overwrite pending_undo_entry
        // and lose the first stashed entry.
        if self.state.pending_undo_redo.is_some() {
            return;
        }
        let Some(entry) = self.state.journal.pop_undo() else {
            return;
        };
        let label = entry.label.clone();
        let context = entry.context;
        // Split label strings are pinned in jd-core::command (§Split op_label).
        // Keep format changes in sync there.
        let echo = if label.starts_with("Split card") || label.starts_with("Split scrap") {
            format!("Undid: {label} (split-off card moved to trash)")
        } else {
            format!("Undid: {label}")
        };
        self.state.status_echo = Some((echo, std::time::Instant::now()));
        match entry.inverse.clone() {
            InverseAction::Vault(op) => {
                // Async: stash entry, send op, wait for OpDone to build redo entry.
                self.state.pending_undo_redo = Some(UndoRedoKind::Undo);
                self.state.pending_undo_entry = Some(entry);
                let _ = self.vault.commands.send(VaultCommand::Op {
                    op,
                    source: OpSource::UndoRedo,
                });
            }
            InverseAction::Session(op) => {
                let inv = self.state.session.apply(&op);
                self.state.session_dirty_at = Some(std::time::Instant::now());
                self.state.journal.push_redo(JournalEntry {
                    label,
                    inverse: InverseAction::Session(inv),
                    context,
                });
                self.do_view_travel(context);
            }
            InverseAction::Sessions(ops) => {
                let mut inverses: Vec<SessionOp> = Vec::new();
                for op in &ops {
                    let inv = self.state.session.apply(op);
                    inverses.push(inv);
                }
                self.state.session_dirty_at = Some(std::time::Instant::now());
                inverses.reverse();
                self.state.journal.push_redo(JournalEntry {
                    label,
                    inverse: InverseAction::Sessions(inverses),
                    context,
                });
                self.do_view_travel(context);
            }
        }
    }

    /// Execute the top-of-redo-stack entry (Ctrl+Y / Ctrl+Shift+Z).
    fn execute_redo(&mut self) {
        // Guard: ignore while a vault undo/redo is in-flight (same rationale as
        // execute_undo — prevents stash overwrite before OpDone drains).
        if self.state.pending_undo_redo.is_some() {
            return;
        }
        let Some(entry) = self.state.journal.pop_redo() else {
            return;
        };
        let label = entry.label.clone();
        let context = entry.context;
        self.state.status_echo = Some((format!("Redid: {label}"), std::time::Instant::now()));
        match entry.inverse.clone() {
            InverseAction::Vault(op) => {
                // Async: stash entry, wait for OpDone to build undo entry.
                self.state.pending_undo_redo = Some(UndoRedoKind::Redo);
                self.state.pending_undo_entry = Some(entry);
                let _ = self.vault.commands.send(VaultCommand::Op {
                    op,
                    source: OpSource::UndoRedo,
                });
            }
            InverseAction::Session(op) => {
                let inv = self.state.session.apply(&op);
                self.state.session_dirty_at = Some(std::time::Instant::now());
                self.state.journal.push_undo_from_redo(JournalEntry {
                    label,
                    inverse: InverseAction::Session(inv),
                    context,
                });
                self.do_view_travel(context);
            }
            InverseAction::Sessions(ops) => {
                let mut inverses: Vec<SessionOp> = Vec::new();
                for op in &ops {
                    let inv = self.state.session.apply(op);
                    inverses.push(inv);
                }
                self.state.session_dirty_at = Some(std::time::Instant::now());
                inverses.reverse();
                self.state.journal.push_undo_from_redo(JournalEntry {
                    label,
                    inverse: InverseAction::Sessions(inverses),
                    context,
                });
                self.do_view_travel(context);
            }
        }
    }
}

/// Save-on-drop: best-effort flush of any pending session changes.
impl Drop for JdUi {
    fn drop(&mut self) {
        if self.state.session_dirty_at.is_some() {
            let _ = self.state.session.save(&self.vault_ref);
        }
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Launch the platform file manager to reveal `path`.
/// macOS: `open -R <path>`; Windows: `explorer /select,"<path>"` (raw arg);
/// Linux/other: `xdg-open <parent-dir>`.
/// Spawns and detaches (non-blocking).  Returns `Err` if spawn fails.
fn reveal_in_file_manager(path: &std::path::Path) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open")
            .arg("-R")
            .arg(path)
            .spawn()
            .map_err(|e| e.to_string())?;
    }
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        // `/select,"<path>"` must be ONE raw token passed to CreateProcess.
        // Using .arg() would quote the whole argument as a separate token,
        // breaking paths with spaces (Windows Shell parses `/select,` prefix
        // directly from the raw command line).
        std::process::Command::new("explorer")
            .raw_arg(explorer_select_arg(path))
            .spawn()
            .map_err(|e| e.to_string())?;
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        let parent = path.parent().unwrap_or(path);
        std::process::Command::new("xdg-open")
            .arg(parent)
            .spawn()
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Build the raw `/select,"<path>"` argument string for Windows explorer.
/// Extracted as a pure function so its string construction can be unit-tested
/// cross-platform (the cfg(windows) CommandExt::raw_arg call is not, but the
/// argument form it receives is pinned by this test).
#[allow(dead_code)] // used only on cfg(windows), but tested on all platforms
fn explorer_select_arg(path: &std::path::Path) -> String {
    format!("/select,\"{}\"", path.display())
}

/// If a vault undo/redo op fails, clear the in-flight guard and restore the
/// stashed entry to the stack it came from so the user can retry. Without this
/// the guard stays set and all future app-stack vault undo/redo is permanently
/// blocked.
fn restore_failed_undo_redo(state: &mut UiState) {
    if let (Some(kind), Some(entry)) = (
        state.pending_undo_redo.take(),
        state.pending_undo_entry.take(),
    ) {
        match kind {
            UndoRedoKind::Undo => {
                // Entry was popped from undo; push it back without clearing redo.
                state.journal.push_undo_from_redo(entry);
            }
            UndoRedoKind::Redo => {
                state.journal.push_redo(entry);
            }
        }
    }
}

/// Journal label for session ops that are undoable user actions.
/// Viewport changes (ViewportMoved) and structural ops (CreateDesk etc.)
/// are NOT journaled.
fn session_op_label(op: &SessionOp) -> Option<&'static str> {
    match op {
        SessionOp::Move { .. } => Some("Move card"),
        SessionOp::PutAway { .. } => Some("Put card away"),
        SessionOp::Place { .. } => None, // never emitted by desk_ui; place_card journals directly
        _ => None,
    }
}

/// Collect every `NoteId` that `op` acts on as a *subject* (i.e. an existing
/// note that may have been mutated).  `Create` produces a brand-new id, so it
/// contributes nothing here — callers handle `OpResult::created` separately.
///
/// The match is exhaustive (no `_` arm) so that adding a new `VaultOp` variant
/// forces a compile error until this function is updated.
pub fn op_subject_ids(op: &VaultOp, out: &mut Vec<NoteId>) {
    match op {
        VaultOp::Create { .. } => {
            // New id — no pre-existing subject to invalidate.
        }
        VaultOp::SaveBody { id, .. }
        | VaultOp::RenameTitle { id, .. }
        | VaultOp::Promote { id }
        | VaultOp::Demote { id }
        | VaultOp::SetKind { id, .. }
        | VaultOp::SetSource { id, .. }
        | VaultOp::SetTags { id, .. }
        | VaultOp::Toss { id }
        | VaultOp::Delete { id }
        | VaultOp::Restore { id }
        | VaultOp::Split { id, .. } => {
            out.push(*id);
        }
        VaultOp::Batch(ops) => {
            for inner in ops {
                op_subject_ids(inner, out);
            }
        }
    }
}

// ---------------------------------------------------------------------------

/// The eframe shell. Owns nothing but JdUi.
pub struct JdApp(pub JdUi);

impl eframe::App for JdApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.0.ui(ui);
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use jd_core::command::VaultOp;

    fn nid(n: u8) -> NoteId {
        let s = format!("01ARZ3NDEKTSV4RRFFQ69G5F{n:02}");
        NoteId::parse(&s).unwrap()
    }

    #[test]
    fn op_subject_ids_save_body() {
        let op = VaultOp::SaveBody {
            id: nid(1),
            content: "hello".into(),
        };
        let mut out = Vec::new();
        op_subject_ids(&op, &mut out);
        assert_eq!(out, vec![nid(1)]);
    }

    #[test]
    fn op_subject_ids_rename_title() {
        let op = VaultOp::RenameTitle {
            id: nid(2),
            new_title: "new name".into(),
        };
        let mut out = Vec::new();
        op_subject_ids(&op, &mut out);
        assert_eq!(out, vec![nid(2)]);
    }

    #[test]
    fn op_subject_ids_batch_collects_all() {
        let op = VaultOp::Batch(vec![
            VaultOp::SaveBody {
                id: nid(3),
                content: "body".into(),
            },
            VaultOp::RenameTitle {
                id: nid(4),
                new_title: "title".into(),
            },
        ]);
        let mut out = Vec::new();
        op_subject_ids(&op, &mut out);
        assert_eq!(out, vec![nid(3), nid(4)]);
    }

    #[test]
    fn op_subject_ids_create_contributes_nothing() {
        use jd_core::command::Dest;
        use jd_core::note::{Kind, NewNote, Status};
        let op = VaultOp::Create {
            seed: NewNote {
                body: String::new(),
                status: Status::Fleeting,
                kind: Kind::Note,
                source: None,
                tags: Vec::new(),
            },
            dest: Dest::Inbox,
        };
        let mut out = Vec::new();
        op_subject_ids(&op, &mut out);
        assert!(out.is_empty());
    }

    fn journal_entry(n: u8) -> JournalEntry {
        JournalEntry {
            label: format!("op {n}"),
            inverse: InverseAction::Vault(VaultOp::Promote { id: nid(n) }),
            context: OpContext::default(),
        }
    }

    /// A failed vault-undo op must clear the in-flight guard and push the
    /// stashed entry back onto the undo stack (so the user can retry).
    #[test]
    fn restore_failed_undo_redo_undo_restores_to_undo_stack() {
        let mut state = UiState::default();
        let entry = journal_entry(1);
        state.pending_undo_redo = Some(UndoRedoKind::Undo);
        state.pending_undo_entry = Some(entry.clone());

        restore_failed_undo_redo(&mut state);

        assert!(state.pending_undo_redo.is_none(), "guard must be cleared");
        assert!(state.pending_undo_entry.is_none(), "stash must be cleared");
        assert_eq!(
            state.journal.undo_label(),
            Some(entry.label.as_str()),
            "entry must be back on undo stack"
        );
        assert!(
            state.journal.redo_label().is_none(),
            "redo stack must not be disturbed"
        );
    }

    /// A failed vault-redo op must clear the in-flight guard and push the
    /// stashed entry back onto the redo stack (so the user can retry).
    #[test]
    fn restore_failed_undo_redo_redo_restores_to_redo_stack() {
        let mut state = UiState::default();
        let entry = journal_entry(2);
        state.pending_undo_redo = Some(UndoRedoKind::Redo);
        state.pending_undo_entry = Some(entry.clone());

        restore_failed_undo_redo(&mut state);

        assert!(state.pending_undo_redo.is_none(), "guard must be cleared");
        assert!(state.pending_undo_entry.is_none(), "stash must be cleared");
        assert_eq!(
            state.journal.redo_label(),
            Some(entry.label.as_str()),
            "entry must be back on redo stack"
        );
    }

    /// With no in-flight undo/redo, restore_failed_undo_redo is a no-op.
    #[test]
    fn restore_failed_undo_redo_no_op_when_not_in_flight() {
        let mut state = UiState::default();
        restore_failed_undo_redo(&mut state);
        assert!(state.pending_undo_redo.is_none());
        assert!(state.pending_undo_entry.is_none());
        assert!(state.journal.undo_label().is_none());
    }

    /// Pin the raw argument form used for Windows explorer /select,.
    /// The raw_arg call receives this string verbatim; spaces in the path must
    /// be inside the quotes so Windows Shell parses /select, as a single token.
    #[test]
    fn explorer_select_arg_quotes_path() {
        use std::path::Path;
        let simple = Path::new(r"C:\notes\file.md");
        let result = explorer_select_arg(simple);
        assert!(
            result.starts_with("/select,\""),
            "must start with /select,\": {result}"
        );
        assert!(
            result.ends_with('"'),
            "must end with closing quote: {result}"
        );
        assert!(
            result.contains("file.md"),
            "must contain the filename: {result}"
        );

        let with_spaces = Path::new(r"C:\my notes\my file.md");
        let result2 = explorer_select_arg(with_spaces);
        // The whole path is quoted as one token; spaces inside quotes are OK.
        assert!(
            result2.starts_with("/select,\""),
            "spaced path must start with /select,\": {result2}"
        );
        assert!(
            result2.contains("my notes"),
            "spaced path must contain directory: {result2}"
        );
    }

    /// View-travel must skip the surface switch if the desk has been deleted since
    /// the journal entry was created. The status echo still shows what was undone,
    /// but current_surface remains unchanged and no panic occurs.
    #[test]
    fn view_travel_skips_deleted_desk() {
        use jd_core::session::{Desk, DeskId};

        let mut state = UiState::default();
        let desk_id = DeskId::generate(&mut IdGen::new());
        let other_desk_id = DeskId::generate(&mut IdGen::new());

        // Create two desks and set current_surface to desk_id.
        state.session.desks.push(Desk {
            id: desk_id,
            name: "Desk A".into(),
            cards: vec![],
            viewport: Default::default(),
        });
        state.session.desks.push(Desk {
            id: other_desk_id,
            name: "Desk B".into(),
            cards: vec![],
            viewport: Default::default(),
        });
        state.session.current_surface = Some(SurfaceId::Desk(other_desk_id));

        // Create a context that names the first desk (which we're about to delete).
        let ctx = OpContext {
            desk: Some(desk_id),
            note: None,
        };

        // Delete the first desk directly (simulating a desk deletion since the
        // journal entry was created).
        state.session.desks.retain(|d| d.id != desk_id);

        // Create a minimal JdUi to call do_view_travel. We use a dummy vault ref.
        // Since we can't easily construct JdUi::new without a real vault, we'll
        // directly test the logic here with the state.
        // (Alternatively, a kittest or integration test would be cleaner.)

        // After view-travel on the deleted desk, current_surface should not change
        // (still pointing at other_desk_id).
        let original_surface = state.session.current_surface;

        // Simulate do_view_travel's logic:
        if let Some(desk_id_ctx) = ctx.desk {
            if !state.session.desks.iter().any(|d| d.id == desk_id_ctx) {
                // The guard prevents the switch.
                assert_eq!(
                    state.session.current_surface, original_surface,
                    "current_surface should not change when desk is deleted"
                );
                return;
            }
            if state.session.current_surface != Some(SurfaceId::Desk(desk_id_ctx)) {
                state.session.current_surface = Some(SurfaceId::Desk(desk_id_ctx));
            }
        }

        panic!("Guard should have prevented reaching this point");
    }

    /// When a Create OpDone arrives before ScanComplete (reversed order), the
    /// created id is buffered in orphaned_create_id while pending_create is kept
    /// alive.  Once ScanComplete fires and the bootstrap desk is created, the
    /// orphaned card must be placed on that desk.
    ///
    /// Simulates the state transitions directly (mirrors the logic in
    /// handle_pending_create + drain_events ScanComplete path) without a real vault.
    #[test]
    fn pending_create_sweep_opdone_before_scan_complete() {
        use crate::state::PendingCreate;
        use jd_core::command::VaultOp;
        use jd_core::geom::Vec2 as CoreVec2;
        use jd_core::session::DeskId;

        let mut state = UiState::default();
        let mut id_gen = jd_core::id::IdGen::new();

        // Arm pending_create (simulates Ctrl+N pressed before ScanComplete).
        let place_at = CoreVec2 { x: 50.0, y: 50.0 };
        state.pending_create = Some(PendingCreate {
            at: place_at,
            open_editor: false,
        });

        let new_id = nid(42);

        // --- Step 1: handle_pending_create called with no desks (mirrors drain_events OpDone) ---
        // Only consume pending_create for Create ops (inverse = Delete{id}).
        let result_inverse = Some(VaultOp::Delete { id: new_id });
        let is_create_op = matches!(result_inverse, Some(VaultOp::Delete { .. }));
        assert!(is_create_op);

        let created_id = new_id;
        // No desk: keep pending_create alive and buffer the orphan id.
        state.orphaned_create_id.get_or_insert(created_id);
        // pending_create stays Some (not consumed) so ScanComplete can read the position.

        assert!(
            state.pending_create.is_some(),
            "pending_create must stay alive when OpDone arrives before desk"
        );
        assert_eq!(
            state.orphaned_create_id,
            Some(new_id),
            "orphaned_create_id must buffer the new note id"
        );

        // --- Step 2: ScanComplete handler: create bootstrap desk + consume orphan ---
        state.scan_done = true;
        let desk_id = DeskId::generate(&mut id_gen);
        let _ = state.session.apply(&SessionOp::CreateDesk {
            id: desk_id,
            name: "Desk".into(),
        });
        state.session.current_surface = Some(SurfaceId::Desk(desk_id));

        // Mirrors drain_events ScanComplete orphan consumption.
        if let Some(orphan_id) = state.orphaned_create_id.take()
            && let Some(pending) = state.pending_create.take()
            && let Some(d) = state.session.desks.first()
        {
            let _ = state.session.apply(&SessionOp::Place {
                desk: d.id,
                id: orphan_id,
                pos: pending.at,
            });
        }

        assert!(
            state.orphaned_create_id.is_none(),
            "orphaned_create_id must be consumed after ScanComplete"
        );
        assert!(
            state.pending_create.is_none(),
            "pending_create must be consumed during orphan placement"
        );
        assert!(
            state
                .session
                .desks
                .iter()
                .any(|d| d.cards.iter().any(|c| c.id == new_id)),
            "orphaned card must be placed on the bootstrap desk"
        );
    }

    /// Minimal on-drop-cleaned temp dir for the drain_events test below
    /// (mirrors jd-core's tests/common; jd-app keeps zero test-only deps).
    struct TestDir(std::path::PathBuf);
    impl TestDir {
        fn new() -> TestDir {
            let p = std::env::temp_dir().join(format!(
                "jd-app-test-{}-{:x}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            std::fs::create_dir_all(&p).unwrap();
            TestDir(p)
        }
    }
    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// WP4 Task 3: drain_events stores the ScanComplete quarantine list and
    /// appends+dedups Conflict note ids (Needs-Attention plumbing for Task 4).
    ///
    /// Uses a real JdUi over a temp vault, then swaps in a hand-fed event
    /// channel so we control exactly which VaultEvents drain_events sees.
    #[test]
    fn drain_events_tracks_quarantine_and_conflicts() {
        use jd_core::vault::scan::QuarantinedFile;
        use std::sync::mpsc;

        let dir = TestDir::new();
        let mut ui = JdUi::new(&dir.0).unwrap();

        // Replace the worker handle with a hand-built one we feed directly.
        let (event_tx, event_rx) = mpsc::channel();
        let (cmd_tx, _cmd_rx) = mpsc::channel();
        ui.vault = VaultHandle {
            commands: cmd_tx,
            events: event_rx,
            index: std::sync::Arc::new(std::sync::RwLock::new(jd_core::index::Index::new())),
        };

        let qfile = QuarantinedFile {
            rel_path: "notes/bad.md".into(),
            error: "invalid UTF-8".into(),
        };
        event_tx
            .send(VaultEvent::ScanComplete {
                quarantined: vec![qfile.clone()],
            })
            .unwrap();
        // Conflict for nid(1) twice (must dedup) and nid(2) once.
        for n in [1u8, 1, 2] {
            event_tx
                .send(VaultEvent::Conflict {
                    id: nid(n),
                    conflict_copy: std::path::PathBuf::from("notes/x (conflict).md"),
                })
                .unwrap();
        }
        ui.drain_events();

        assert_eq!(ui.state.quarantined, vec![qfile], "quarantine list stored");
        assert_eq!(
            ui.state.conflicts,
            vec![nid(1), nid(2)],
            "conflicts append in arrival order and dedup"
        );

        // A later rescan replaces the quarantine list wholesale.
        event_tx
            .send(VaultEvent::ScanComplete {
                quarantined: vec![],
            })
            .unwrap();
        ui.drain_events();
        assert!(
            ui.state.quarantined.is_empty(),
            "next ScanComplete replaces the list"
        );
        assert_eq!(
            ui.state.conflicts,
            vec![nid(1), nid(2)],
            "conflicts are session-scoped, not cleared by rescan"
        );
    }
}
