#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use jd_app::app::{JdApp, JdUi};

fn main() -> eframe::Result {
    // WP2: vault path = first CLI arg, else ~/JunkDrawer (created on demand).
    // Proper arg parsing / vault picker arrives with later WPs.
    let root = std::env::args()
        .nth(1)
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| {
            // std::env::home_dir is deprecated; read $HOME / %USERPROFILE% directly.
            let home = std::env::var("HOME")
                .or_else(|_| std::env::var("USERPROFILE"))
                .unwrap_or_else(|_| ".".to_string());
            std::path::PathBuf::from(home).join("JunkDrawer")
        });
    let ui = JdUi::new(&root).expect("failed to open vault");
    let options = eframe::NativeOptions::default();
    eframe::run_native(
        "Junk Drawer",
        options,
        Box::new(|_cc| Ok(Box::new(JdApp(ui)))),
    )
}
