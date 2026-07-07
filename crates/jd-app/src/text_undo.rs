//! Text undo stack — placeholder for Task 12.
//! The real word-granularity undo implementation arrives in WP2 Task 12.

/// Per-card text undo stack. Task 12 will replace this with a real
/// word-granularity undo implementation.
#[derive(Default)]
pub struct TextUndo;

impl TextUndo {
    pub fn new(_initial: &str) -> TextUndo {
        TextUndo
    }
}
