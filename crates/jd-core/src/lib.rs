//! Junk Drawer core: vault I/O, parsers, index, search, undo journal.
//! No egui dependency — fully testable headless.

pub mod id;
pub mod note;
pub mod rng;
pub mod tag;
pub mod time;

#[cfg(test)]
mod tests {
    #[test]
    fn workspace_wiring() {
        assert_eq!(2 + 2, 4);
    }
}
