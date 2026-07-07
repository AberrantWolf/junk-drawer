//! JdUi: the whole application as an egui-only struct (kittest-testable).
//! JdApp: the thin eframe shell around it.

use std::path::Path;
use std::sync::{Arc, Mutex};

use eframe::egui;
use jd_core::error::CoreError;
use jd_core::vault::Vault;
use jd_core::worker::{self, VaultEvent, VaultHandle};

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
    pub scan_done: bool,
    pub last_error: Option<String>,
    pub theme: crate::theme::Theme,
    pub fonts_installed: bool,
}

impl JdUi {
    pub fn new(vault_root: &Path) -> Result<JdUi, CoreError> {
        let vault = Vault::open(vault_root)?;
        let waker = Waker::default();
        let w = waker.clone();
        let handle = worker::start(vault, Box::new(move || w.wake()))?;
        Ok(JdUi {
            vault: handle,
            waker,
            scan_done: false,
            last_error: None,
            theme: crate::theme::Theme::light(),
            fonts_installed: false,
        })
    }

    /// Frame-loop step 1 (architecture §3): drain ALL pending worker events.
    pub fn drain_events(&mut self) {
        while let Ok(ev) = self.vault.events.try_recv() {
            match ev {
                VaultEvent::ScanComplete { .. } => self.scan_done = true,
                VaultEvent::Error { context, message } => {
                    self.last_error = Some(format!("{context}: {message}"));
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
        // Status line (bottom). Real surfaces land in Tasks 8-9.
        egui::Panel::bottom("status_line").show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label("Junk Drawer");
                if let Some(err) = &self.last_error {
                    ui.label(err.as_str());
                }
            });
        });
    }
}

/// The eframe shell. Owns nothing but JdUi.
pub struct JdApp(pub JdUi);

impl eframe::App for JdApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.0.ui(ui);
    }
}
