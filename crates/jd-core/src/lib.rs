//! Junk Drawer core: vault I/O, parsers, index, search, undo journal.
//! No egui dependency — fully testable headless.

#[cfg(test)]
mod tests {
    #[test]
    fn workspace_wiring() {
        assert_eq!(2 + 2, 4);
    }
}
