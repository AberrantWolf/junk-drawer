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

use crate::state::UiState;
use crate::surfaces::desk::{DeskEvent, DeskUiDeps, DragState, FaceMeta};

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

        // 4. Render: central desk + status line; editor overlay when open (Task 10).

        // ------------------------------------------------------------------
        // Status line (bottom)
        // ------------------------------------------------------------------
        let fit_clicked = egui::Panel::bottom("status_line")
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label("Junk Drawer");
                    let fit = ui.button("Fit");
                    // Zoom %
                    if let Some(desk) = self.state.session.desks.first() {
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
        // Central panel: current desk
        // ------------------------------------------------------------------
        egui::CentralPanel::default().show(ui, |ui| {
            // Prefetch FaceMeta for all placed cards under ONE index read lock.
            let face_metas: Vec<FaceMeta> = {
                if let Some(desk) = self.state.session.desks.first() {
                    let idx = self.vault.index.read().unwrap();
                    desk.cards
                        .iter()
                        .filter_map(|c| idx.get(c.id).map(|m| FaceMeta::from_note_meta(m, &idx)))
                        .collect()
                } else {
                    Vec::new()
                }
            };

            // Handle Fit button
            if fit_clicked && let Some(desk) = self.state.session.desks.first() {
                let panel = ui.max_rect();
                let positions: Vec<(NoteId, CoreVec2)> =
                    desk.cards.iter().map(|c| (c.id, c.pos)).collect();
                let mut cam = crate::surfaces::desk::DeskCamera {
                    center: egui::vec2(desk.viewport.center.x, desk.viewport.center.y),
                    zoom: desk.viewport.zoom,
                };
                cam.zoom_to_fit(&positions, panel);
                let desk_id = desk.id;
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

            if let Some(desk) = self.state.session.desks.first().cloned() {
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
