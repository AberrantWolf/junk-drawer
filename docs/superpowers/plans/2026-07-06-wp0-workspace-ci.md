# WP0 — Workspace Skeleton & CI Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Convert the bare `cargo new` repo into the two-crate workspace (`jd-core`, `jd-app`) with green fmt/clippy/test CI on Linux, macOS, and Windows.

**Architecture:** Cargo workspace at the root; `crates/jd-core` (library, zero deps for now) and `crates/jd-app` (eframe binary that opens an empty window). See `docs/superpowers/plans/2026-07-06-technical-architecture.md` §1.

**Tech Stack:** Rust stable (pinned via `rust-toolchain.toml`), `eframe` (glow backend, latest via `cargo add`), GitHub Actions.

## Global Constraints

- **Crate names are exactly `jd-core` and `jd-app`. Never name any crate `junk-*`.** The existing root package `junk-drawer` must not survive this WP.
- No dependencies beyond `eframe` for `jd-app`; `jd-core` gets **zero** dependencies in this WP.
- Every commit leaves `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`, and `cargo test --workspace` green.
- Commit `Cargo.lock` (this workspace ships a binary).

---

### Task 1: Workspace skeleton

**Files:**
- Delete: `src/main.rs` (and the `src/` dir — it's the throwaway `cargo new` output, untracked)
- Rewrite: `Cargo.toml` (root → workspace manifest)
- Create: `rust-toolchain.toml`, `crates/jd-core/Cargo.toml`, `crates/jd-core/src/lib.rs`, `crates/jd-app/Cargo.toml`, `crates/jd-app/src/main.rs`
- Modify: `.gitignore`

**Interfaces:**
- Consumes: nothing (first task in the project).
- Produces: the workspace every later WP builds in; `jd-app` depends on `jd-core` by path.

- [ ] **Step 1: Remove the cargo-new scaffolding and lay out the workspace**

```bash
rm -rf src
mkdir -p crates/jd-core/src crates/jd-app/src
```

Root `Cargo.toml` (full replacement):

```toml
[workspace]
resolver = "3"
members = ["crates/jd-core", "crates/jd-app"]

[workspace.package]
version = "0.1.0"
edition = "2024"
```

`rust-toolchain.toml`:

```toml
[toolchain]
channel = "stable"
components = ["rustfmt", "clippy"]
```

`crates/jd-core/Cargo.toml`:

```toml
[package]
name = "jd-core"
version.workspace = true
edition.workspace = true

[dependencies]
```

`crates/jd-core/src/lib.rs`:

```rust
//! Junk Drawer core: vault I/O, parsers, index, search, undo journal.
//! No egui dependency — fully testable headless.

#[cfg(test)]
mod tests {
    #[test]
    fn workspace_wiring() {
        assert_eq!(2 + 2, 4);
    }
}
```

`crates/jd-app/Cargo.toml` (eframe version filled by Step 2):

```toml
[package]
name = "jd-app"
version.workspace = true
edition.workspace = true

[dependencies]
jd-core = { path = "../jd-core" }
```

`.gitignore` stays `/target` (Cargo.lock is committed).

- [ ] **Step 2: Add eframe at the latest version**

```bash
cargo add eframe -p jd-app
```

Expected: eframe appears in `crates/jd-app/Cargo.toml` with a concrete version; `Cargo.lock` generated at the root.

- [ ] **Step 3: Write the minimal app**

`crates/jd-app/src/main.rs`:

```rust
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use eframe::egui;

fn main() -> eframe::Result {
    let options = eframe::NativeOptions::default();
    eframe::run_native(
        "Junk Drawer",
        options,
        Box::new(|_cc| Ok(Box::new(JdApp))),
    )
}

struct JdApp;

impl eframe::App for JdApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.label("Junk Drawer");
        });
    }
}
```

Note: if the resolved eframe's `run_native` creator signature differs (older versions take `Box::new(|_cc| Box::new(...))` without the `Ok`), match what the compiler asks for — the pinned intent is "empty window titled Junk Drawer".

- [ ] **Step 4: Verify the gate commands**

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo build -p jd-app
```

Expected: all green; `jd-core` runs 1 test. (`cargo run -p jd-app` opens a window titled "Junk Drawer" — smoke-check only if a display is available.)

- [ ] **Step 5: Commit**

```bash
git add -A
git commit -m "feat: workspace skeleton with jd-core and jd-app crates"
```

---

### Task 2: CI workflow

**Files:**
- Create: `.github/workflows/ci.yml`

**Interfaces:**
- Consumes: Task 1's workspace.
- Produces: the merge gate every later WP relies on.

- [ ] **Step 1: Write the workflow**

`.github/workflows/ci.yml`:

```yaml
name: CI
on:
  push:
    branches: [main]
  pull_request:

jobs:
  check:
    strategy:
      fail-fast: false
      matrix:
        os: [ubuntu-latest, macos-latest, windows-latest]
    runs-on: ${{ matrix.os }}
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
        with:
          components: rustfmt, clippy
      - name: Install Linux GUI build deps
        if: runner.os == 'Linux'
        run: |
          sudo apt-get update
          sudo apt-get install -y libgtk-3-dev libxkbcommon-dev libwayland-dev \
            libxcb-render0-dev libxcb-shape0-dev libxcb-xfixes0-dev
      - uses: Swatinem/rust-cache@v2
      - run: cargo fmt --all --check
      - run: cargo clippy --workspace --all-targets -- -D warnings
      - run: cargo test --workspace
```

- [ ] **Step 2: Validate the YAML locally**

```bash
python3 -c "import yaml,sys; yaml.safe_load(open('.github/workflows/ci.yml'))" && echo OK
```

Expected: `OK`. (Full validation happens on the first push; if the repo has no remote yet, that's fine — the workflow is inert until one exists.)

- [ ] **Step 3: Commit**

```bash
git add .github/workflows/ci.yml
git commit -m "ci: fmt, clippy, and test matrix on linux/macos/windows"
```
