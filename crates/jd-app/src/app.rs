//! JdUi: the whole application as an egui-only struct (kittest-testable).
//! JdApp: the thin eframe shell around it.

use std::path::Path;
use std::sync::{Arc, Mutex};

use eframe::egui;
use jd_core::command::{OpSource, VaultOp};
use jd_core::error::CoreError;
use jd_core::geom::Vec2 as CoreVec2;
use jd_core::id::{IdGen, NoteId};
use jd_core::journal::{InverseAction, JournalEntry, OpContext};
use jd_core::session::{DeskId, SessionOp, SessionState, SurfaceId};
use jd_core::vault::Vault;
use jd_core::worker::{self, VaultEvent, VaultHandle};

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
    waker: Waker,
    pub state: UiState,
    pub theme: crate::theme::Theme,
    pub fonts_installed: bool,
    id_gen: IdGen,
    /// Persists across frames for card body layout caching.
    line_cache: crate::editor::LineCache,
    /// Current card-drag state (None = no drag in progress).
    drag: Option<DragState>,
}

impl JdUi {
    pub fn new(vault_root: &Path) -> Result<JdUi, CoreError> {
        let vault = Vault::open(vault_root)?;
        // Load session BEFORE starting the worker (worker::start consumes vault).
        let session = SessionState::load(&vault);
        let waker = Waker::default();
        let w = waker.clone();
        let handle = worker::start(vault, Box::new(move || w.wake()))?;
        let state = UiState {
            session,
            ..Default::default()
        };
        Ok(JdUi {
            vault: handle,
            waker,
            state,
            theme: crate::theme::Theme::light(),
            fonts_installed: false,
            id_gen: IdGen::new(),
            line_cache: crate::editor::LineCache::default(),
            drag: None,
        })
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
                        // Bootstrap desk is a system act, not undoable — do not journal.
                        let _inv = self.state.session.apply(&SessionOp::CreateDesk {
                            id: desk_id,
                            name: "Desk".into(),
                        });
                        self.state.session.current_surface = Some(SurfaceId::Desk(desk_id));
                    }
                }
                VaultEvent::Body { id, content } => {
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
        self.drain_events();

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
                self.apply_desk_events(evts, desk.id);
            }
        });
    }

    /// Apply `DeskEvent`s emitted by `desk_ui`.
    fn apply_desk_events(&mut self, events: Vec<DeskEvent>, _desk_id: DeskId) {
        for ev in events {
            match ev {
                DeskEvent::OpenCard(id) => {
                    self.state.session.open_card = Some(id);
                    self.state.session_dirty_at = Some(std::time::Instant::now());
                }
                DeskEvent::FocusChanged(id) => {
                    self.state.focus = id;
                    // Reveal: if the newly focused card is off-screen, pan to it.
                    if let Some(focused_id) = id
                        && let Some(desk) = self.state.session.desks.first()
                    {
                        // Approximate the panel rect (app window minus status bar).
                        // kittest uses 1200×800 minus ~24px; in production this is
                        // a best-effort approximation (close enough for reveal).
                        // We cannot get the real panel rect here (outside the ui fn),
                        // so we use a sentinel that errs on the side of revealing.
                        // A zero-rect makes every card "off-screen", centering on it.
                        let panel =
                            egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1200.0, 776.0));
                        if let Some(new_cam) =
                            crate::surfaces::desk::reveal(desk, focused_id, panel)
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
                    let label = session_op_label(&op);
                    let inverse = self.state.session.apply(&op);
                    self.state.session_dirty_at = Some(std::time::Instant::now());
                    // Journal only user-facing session ops (Move, PutAway).
                    if let Some(lbl) = label {
                        self.state.journal.push(JournalEntry {
                            label: lbl.to_owned(),
                            inverse: InverseAction::Session(inverse),
                            context: OpContext::default(),
                        });
                    }
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
        SessionOp::Place { .. } => None, // system placement not journaled here
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
