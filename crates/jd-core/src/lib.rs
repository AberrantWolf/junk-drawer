//! Junk Drawer core: vault I/O, parsers, index, search, undo journal.
//! No egui dependency — fully testable headless.

pub mod command;
pub mod doc;
pub mod error;
pub mod frontmatter;
pub mod geom;
pub mod id;
pub mod index;
pub mod lexer;
pub mod note;
pub mod rng;
pub mod session;
pub mod tag;
pub mod time;
pub mod vault;
pub mod worker;

#[cfg(test)]
mod tests {
    #[test]
    fn workspace_wiring() {
        assert_eq!(2 + 2, 4);
    }
}
