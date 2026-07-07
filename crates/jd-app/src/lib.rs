//! jd-app library target: everything except the eframe shell, so
//! integration tests (egui_kittest) can drive the real UI headless.
pub mod app;
pub mod card;
pub mod editor;
pub mod state;
pub mod surfaces;
pub mod text_undo;
pub mod theme;
