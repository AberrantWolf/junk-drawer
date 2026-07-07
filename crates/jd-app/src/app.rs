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

use crate::rail::{RailEvent, RailUiDeps};
use crate::state::UiState;
use crate::surfaces::desk::{DeskEvent, DeskUiDeps, DragState, FaceMeta};
use crate::surfaces::inbox::{InboxEvent, InboxUiDeps};

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
        })
    }

    /// Apply a session op, always mark dirty, and push a journal entry when
    /// `journal` is `Some(label)`.  This is the single authoritative path for
    /// all session mutations.
    fn apply_session(&mut self, op: SessionOp, journal: Option<&'static str>) {
        let inverse = self.state.session.apply(&op);
        self.state.session_dirty_at = Some(std::time::Instant::now());
        if let Some(label) = journal {
            self.state.journal.push(JournalEntry {
                label: label.to_owned(),
                inverse: InverseAction::Session(inverse),
                context: OpContext::default(),
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
            // No desk yet — restore pending_create (unlikely race; best-effort).
            self.state.pending_create = Some(pending);
        }
    }

    /// Frame-loop step 1 (architecture §3): drain ALL pending worker events.
    pub fn drain_events(&mut self) {
        while let Ok(ev) = self.vault.events.try_recv() {
            match ev {
                VaultEvent::ScanComplete { .. } => {
                    self.state.scan_done = true;
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
                }
                VaultEvent::Body { id, content } => {
                    // If this body belongs to the open_card and no editor is live yet,
                    // open the editor now.  This is the "body arrived" trigger described
                    // in the architecture: OpenCard fires get_or_request; when the body
                    // lands here the editor is created.
                    if self.state.session.open_card == Some(id) && self.state.editor.is_none() {
                        let saved_undo = self.state.text_undo.remove(&id);
                        self.state.editor = Some(crate::editor::EditorState::open(
                            id,
                            content.clone(),
                            saved_undo,
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

                    // Ctrl+N pending_create: if a Create just finished while
                    // pending_create is set, place the new card and open editor.
                    self.handle_pending_create(&result);

                    // Push to journal only for user-originated ops.
                    if source == OpSource::User
                        && let Some(inv_op) = result.inverse
                    {
                        self.state.journal.push(JournalEntry {
                            label: result.label,
                            inverse: InverseAction::Vault(inv_op),
                            context: OpContext::default(),
                        });
                    }
                }
                VaultEvent::OpFailed { label, message } => {
                    self.state.last_error = Some(format!("{label}: {message}"));
                }
                VaultEvent::Error { context, message } => {
                    self.state.last_error = Some(format!("{context}: {message}"));
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
                    context: OpContext::default(),
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
                    context: OpContext::default(),
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
        //    Otherwise: Ctrl+N → create a new fleeting scrap in Inbox.
        if self.state.editor.is_none() {
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
            if ctrl_n {
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
        }

        // 4. Render: left rail + central surface + status line; editor overlay (Task 10).

        // ------------------------------------------------------------------
        // Status line (bottom) — must be added before SidePanel and CentralPanel.
        // ------------------------------------------------------------------
        let fit_clicked = egui::Panel::bottom("status_line")
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label("Junk Drawer");
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
                    // Prefetch FaceMeta for all placed cards under ONE index read lock.
                    let face_metas: Vec<FaceMeta> = {
                        let idx = self.vault.index.read().unwrap();
                        self.state
                            .session
                            .desks
                            .iter()
                            .find(|d| d.id == desk_id)
                            .map(|desk| {
                                desk.cards
                                    .iter()
                                    .filter_map(|c| {
                                        idx.get(c.id).map(|m| FaceMeta::from_note_meta(m, &idx))
                                    })
                                    .collect()
                            })
                            .unwrap_or_default()
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

                        let mut deps = DeskUiDeps {
                            focus: &mut self.state.focus,
                            bodies: &mut self.state.bodies,
                            commands: &self.vault.commands,
                            theme: &self.theme,
                            line_cache: &mut self.line_cache,
                            face_metas: &face_metas,
                            drag: &mut self.drag,
                            editor_open: self.state.editor.is_some(),
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
                    };
                    let evts = crate::surfaces::inbox::inbox_ui(ui, &mut deps);
                    self.apply_inbox_events(evts);
                }

                // Task 5: Trash surface — placeholder until it lands.
                Some(SurfaceId::Trash) | None => {
                    crate::surfaces::placeholder::placeholder_ui(ui);
                }

                // Drawer (WP4) and Map (WP5) — placeholder per scope boundaries.
                Some(SurfaceId::Drawer) | Some(SurfaceId::Map) => {
                    crate::surfaces::placeholder::placeholder_ui(ui);
                }
            }
        });

        // 4b. Editor modal overlay (Task 10).
        let close_editor = if let Some(ed) = &mut self.state.editor {
            let mut deps = crate::editor::EditorDeps {
                theme: &self.theme,
                commands: &self.vault.commands,
                index: &self.vault.index,
                reduced_motion: false,
            };
            let ev = crate::editor::editor_ui(ui, ed, &mut deps);
            matches!(ev, crate::editor::EditorEvent::CloseAndSave)
        } else {
            false
        };
        if close_editor {
            // Only save when the buffer was actually modified.  A clean
            // open→close must not write the file (which would invalidate
            // the body cache via the watcher echo and push a phantom undo
            // entry with label "Save body").
            let editor = self.state.editor.take().unwrap();
            if editor.dirty {
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

        // 5. Debounced saves: if session_dirty_at elapsed > 1s → save and clear.
        if let Some(dirty_at) = self.state.session_dirty_at
            && dirty_at.elapsed() > std::time::Duration::from_secs(1)
        {
            let _ = self.state.session.save(&self.vault_ref);
            self.state.session_dirty_at = None;
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
                        self.state.editor = Some(crate::editor::EditorState::open(
                            id,
                            cached.text.clone(),
                            saved_undo,
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
            }
        }
    }

    /// Open the card editor for `id`, either immediately (body cached) or deferred
    /// (body not yet loaded; drain_events will open the editor when Body arrives).
    /// Mirrors the DeskEvent::OpenCard path in apply_desk_events.
    fn open_card_editor(&mut self, id: NoteId) {
        self.state.session.open_card = Some(id);
        self.state.session_dirty_at = Some(std::time::Instant::now());
        if let Some(cached) = self.state.bodies.get_or_request(id, &self.vault.commands)
            && self.state.editor.is_none()
        {
            let saved_undo = self.state.text_undo.remove(&id);
            self.state.editor = Some(crate::editor::EditorState::open(
                id,
                cached.text.clone(),
                saved_undo,
            ));
        }
    }

    /// Apply `InboxEvent`s emitted by `inbox_ui`.
    pub fn apply_inbox_event(&mut self, ev: InboxEvent) {
        match ev {
            InboxEvent::OpenCard(id) => {
                self.open_card_editor(id);
            }

            InboxEvent::Promote(_id) => {
                // wired in promotion task
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
}
