# WP2 — Desk, Cards, Editor Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** The first pixels — one desk with real draggable/focusable cards, the card visual language snapshot-tested, and the styled-source editor in its floating window — with the two riskiest UI bets (mixed-size layouter, AccessKit spatial focus) spiked in week one.

**Architecture:** `jd-app` splits into an eframe shell (`JdApp`) and an egui-only core (`JdUi`) so every behavior is drivable headless through `egui_kittest` (which drives the AccessKit tree — every UI test doubles as an accessibility check). All vault I/O goes through the WP1 vault worker (`VaultHandle`); the UI thread never touches the filesystem except session/settings debounced saves via `jd-core` APIs. Card faces and the editor both style text through `jd_core::lexer::lex_line` mapped to egui `TextFormat`s in `theme.rs`.

**Tech Stack:** Rust stable, eframe/egui 0.35 (glow for the shipped app), `egui_kittest` 0.35 (dev-dependency, features `snapshot` + `wgpu` — wgpu prefers software rasterizers in tests since egui #5506), bundled Inter + JetBrains Mono fonts, `jd-core` (WP1).

## Global Constraints

- **Dependency policy (spec Appendix B):** `jd-core` deps stay exactly `notify`. `jd-app` runtime deps stay `eframe`, `jd-core` (`rfd` comes later). **Dev-dependencies** of `jd-app` may add `egui_kittest = { version = "0.35", features = ["snapshot", "wgpu"] }` only — this is Scott-approved (2026-07-07) as a test-only exception; nothing from its tree may leak into `[dependencies]`.
- **Never name anything `junk-*`** — crates are `jd-core` / `jd-app`.
- **Round-trip law:** the editor buffers raw source and saves it verbatim via `VaultOp::SaveBody`. No smart quotes, no rewriting of anything the user typed. The lexer styles; it never mutates.
- **No FS on the UI thread** for note content: body reads via `VaultCommand::ReadBody`, writes via `VaultCommand::Op`. Session state save (`SessionState::save`) and recovery journaling go through their `jd-core` APIs (session save is small + debounced + atomic; this is the one sanctioned UI-thread write per architecture decision §6.5's session-op design).
- **Single writer:** only the vault worker mutates note files. The UI sends commands and drains events once per frame.
- **CI green on ubuntu/macos/windows:** `cargo fmt --check`, `clippy -D warnings`, `cargo test` all pass. Run `cargo fmt` **last, before every commit**.
- **Shape is semantic and universal; texture is Paper-only** (spec §4.5) — Plain keeps scrap proportions, divider tab, citation footer.
- **WCAG AA:** every (fg, bg) pair actually used ≥ 4.5:1 for text, ≥ 3:1 for UI affordances — enforced by `#[test]` in `theme.rs`.
- Commit trailer: `Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>`.
- **BRANCH GUARD:** verify `git status -sb` shows the expected feature branch before starting AND before each commit. Never `checkout`/`switch`. If the branch is wrong, STOP and report.

## Scope boundaries (WP2 does NOT include)

Deferred to WP3+ (do not implement, do not stub beyond what a task says):
- Promotion (Enter-promotes, `Ctrl+Enter`-promotes, PendingPromotion) — WP3. In WP2 `Ctrl+Enter` simply closes the editor (same as Esc).
- Inbox/Trash surfaces, the left rail, card context menu, Toss/Delete keys — WP3.
- App-stack undo/redo routing and named Edit-menu entries — WP3 (WP2 journals SessionOps but wires no undo keys except the editor's text undo).
- Palette, Drawer, ghost fan, edges-on-select — WP4. Map — WP5. Guidance banner, settings UI, menus, shortcut overlay — WP6.
- `Split` UI (`Ctrl+Shift+Enter`) — WP3 (the op exists in core; the editor hook lands with the promotion work).

## Interfaces consumed from jd-core (verified against source, 2026-07-07)

```rust
// worker.rs
pub enum VaultCommand { Op { op: VaultOp, source: OpSource }, ReadBody { id: NoteId },
    JournalBuffer { id: NoteId, content: String }, PurgeTrash { older_than_days: Option<u32> },
    RescanAll, Shutdown }
pub enum VaultEvent { OpDone { result: OpResult, source: OpSource }, OpFailed { label: String, message: String },
    Body { id: NoteId, content: String }, External { changed: Vec<NoteId>, removed: Vec<NoteId> },
    Conflict { id: NoteId, conflict_copy: PathBuf }, ScanProgress { done: usize, total: usize },
    ScanComplete { quarantined_count: usize }, Error { context: String, message: String } }
pub struct VaultHandle { pub commands: Sender<VaultCommand>, pub events: Receiver<VaultEvent>, pub index: SharedIndex }
pub fn start(vault: Vault, wake: Box<dyn Fn() + Send + Sync>) -> Result<VaultHandle, CoreError>;

// lexer.rs
pub fn lex_line(line: &str, entry: LineState, resolve: &dyn Fn(&str) -> bool) -> (Vec<StyledSpan>, LineState);
pub enum SpanStyle { Text, Heading(u8), HeadingMarker, Bold, Italic, BoldItalic, Strike, InlineCode,
    CodeFenceMarker, CodeBlock, ListMarker, TaskBoxUnchecked, TaskBoxChecked, QuoteMarker, Quote,
    WikiLink { resolved: bool }, Tag, Url, MdLinkText, MdLinkUrl }
pub struct StyledSpan { pub range: Range<usize>, pub style: SpanStyle }
pub enum LineState { Normal, InCodeFence }   // Default = Normal

// session.rs
pub struct DeskId(pub NoteId);  impl DeskId { pub fn generate(gen: &mut IdGen) -> DeskId }
pub enum SurfaceId { Desk(DeskId), Inbox, Drawer, Map, Trash }
pub struct Viewport { pub center: Vec2, pub zoom: f32 }
pub struct PlacedCard { pub id: NoteId, pub pos: Vec2 }
pub struct Desk { pub id: DeskId, pub name: String, pub viewport: Viewport, pub cards: Vec<PlacedCard> }
pub struct SessionState { pub desks: Vec<Desk>, pub current_surface: SurfaceId, pub open_card: Option<NoteId> }
impl SessionState { pub fn apply(&mut self, op: &SessionOp) -> SessionOp;
    pub fn load(vault: &Vault) -> SessionState; pub fn save(&self, vault: &Vault) -> Result<(), IoError> }
pub enum SessionOp { Place {..}, Move {..}, PutAway {..}, CreateDesk {..}, RenameDesk {..}, ReorderDesk {..}, DeleteDesk {..} }

// geom.rs — jd_core::geom::Vec2 { x: f32, y: f32 } (no egui dep; we add conversions)
// command.rs — VaultOp::{Create{seed,dest}, SaveBody{id,content}, ...}, OpSource::{User,UndoRedo}, Dest::{Inbox,Notes}
// journal.rs — Journal::{new, push, undo_label, pop_undo, ...}; JournalEntry { label, inverse, context }
// index — SharedIndex = Arc<RwLock<Index>>; Index::{resolve, fuzzy, meta(id), all_tags, ...}
```

`eframe 0.35` App trait (verified): `fn logic(&mut self, ctx, frame)` (no painting) + `fn ui(&mut self, ui: &mut egui::Ui, frame: &mut Frame)`. The egui 0.35 layouter signature (verified): `TextEdit::layouter(&'t mut dyn FnMut(&Ui, &dyn TextBuffer, f32) -> Arc<Galley>)`.

## File Structure

```
crates/jd-app/
├── Cargo.toml            # + [dev-dependencies] egui_kittest; + assets include
├── assets/fonts/         # Inter-{Regular,Bold,Italic,BoldItalic}.ttf, JetBrainsMono-Regular.ttf, OFL licenses
├── kittest.toml          # per-platform snapshot thresholds
├── src/
│   ├── main.rs           # eframe shell only: options, JdApp::new(vault_root), run_native
│   ├── app.rs            # JdApp (eframe::App) wrapping JdUi; JdUi = ALL logic, egui-only, kittest-testable
│   ├── state.rs          # UiState, BodyCache, geom conversions, DebouncedFlag
│   ├── theme.rs          # fonts, palettes (light/dark), WCAG test, SpanStyle→TextFormat, ruled-line + card metrics
│   ├── card/
│   │   ├── mod.rs        # CardWidget: face rendering, AccessKit label, focus, drag
│   │   └── shape.rs      # CardShape from (Status,Kind); geometry, torn edge, tab, footer, rules
│   ├── surfaces/
│   │   ├── mod.rs        # pub mod desk;
│   │   └── desk.rs       # pannable canvas, spatial focus, culling, drag→SessionOp, place_card
│   ├── editor.rs         # floating editor: TextEdit+layouter, line cache, behaviors, autocomplete, autosave
│   └── text_undo.rs      # per-card text undo stacks, word-granularity grouping
└── tests/
    ├── harness_smoke.rs  # Task 1: pipeline proof (query + one snapshot)
    ├── spike_layouter.rs # Task 2 (Spike A)
    ├── spike_focus.rs    # Task 3 (Spike B) — grows into desk focus tests
    ├── card_faces.rs     # Task 7: snapshot matrix
    ├── desk_kittest.rs   # Tasks 8–9: pan/zoom/drag/cull/session/Ctrl+N
    └── editor_kittest.rs # Tasks 10–12: editor behaviors + text undo
    └── snapshots/        # committed .png goldens (diff/new excluded by .gitignore)
```

**Testing pattern used throughout** (established in Task 1): tests build a real temp vault (reuse the `TempDir` pattern from `jd-core/tests/common/mod.rs` — copy it into a `jd-app/tests/common/mod.rs`), construct `JdUi::new(vault_root)` which starts the real worker, then drive it with `egui_kittest::Harness`. The harness closure calls `ui.add(&mut jd_ui)`-style entry `jd_ui.ui(ui)`. Worker events arrive asynchronously → tests call `harness.run()` (which steps until no repaint is requested) or loop `step()` with a bounded retry while draining. A helper `pump(harness, ui, pred, max_frames)` polls until a predicate holds — **never sleep**.

---

### Task 1: Test harness, app split, snapshot pipeline on CI

The point of this task: prove the whole `egui_kittest` pipeline (queries AND snapshots) on all three CI platforms **before** any real UI exists, so later tasks never debug infrastructure and feature at once.

**Files:**
- Modify: `crates/jd-app/Cargo.toml`
- Create: `crates/jd-app/src/app.rs`
- Modify: `crates/jd-app/src/main.rs` (becomes a thin shell)
- Create: `crates/jd-app/tests/common/mod.rs`, `crates/jd-app/tests/harness_smoke.rs`
- Create: `crates/jd-app/kittest.toml`
- Modify: `.gitignore`, `.github/workflows/ci.yml`

**Interfaces:**
- Consumes: `jd_core::vault::Vault::open`, `jd_core::worker::{start, VaultHandle, VaultCommand, VaultEvent}`.
- Produces: `JdUi` (all later tasks add fields/methods to it), `JdUi::new(vault_root: &Path) -> Result<JdUi, CoreError>`, `JdUi::ui(&mut self, ui: &mut egui::Ui)`, `JdUi::drain_events(&mut self)`; test helpers `common::temp_vault()` and `common::pump`.

- [ ] **Step 1: Add dev-dependency and config files**

`crates/jd-app/Cargo.toml` — append:

```toml
[dev-dependencies]
egui_kittest = { version = "0.35.0", features = ["snapshot", "wgpu"] }
```

Create `crates/jd-app/kittest.toml` (per-platform snapshot diff thresholds; start strict, Task 7 tunes if CI shows rasterizer deltas):

```toml
[snapshots]
threshold = 0.6

[snapshots.macos]
threshold = 1.2

[snapshots.windows]
threshold = 1.2
```

Append to `.gitignore`:

```
**/tests/snapshots/**/*.diff.png
**/tests/snapshots/**/*.new.png
**/tests/snapshots/**/*.old.png
```

- [ ] **Step 2: Write the failing smoke test**

`crates/jd-app/tests/common/mod.rs` — copy the `TempDir` helper from `crates/jd-core/tests/common/mod.rs` verbatim (same self-cleaning temp-dir pattern, under `std::env::temp_dir()`), then add:

```rust
use std::path::Path;

/// Step the harness until `pred` returns true or `max_frames` elapse.
/// Panics with `what` on exhaustion. Use this instead of sleeping: worker
/// events arrive between frames, and `wake` requests repaints.
pub fn pump<S>(
    harness: &mut egui_kittest::Harness<'_, S>,
    pred: &mut dyn FnMut(&S) -> bool,
    max_frames: usize,
    what: &str,
) {
    for _ in 0..max_frames {
        if pred(harness.state()) {
            return;
        }
        harness.step();
        std::thread::sleep(std::time::Duration::from_millis(5)); // yield to worker thread only
    }
    panic!("pump: gave up waiting for {what}");
}

/// A minimal vault on disk: inbox/, notes/ (Vault::open creates structure).
pub fn temp_vault() -> TempDir {
    TempDir::new("jd-app-test")
}
```

(Adjust `TempDir::new`'s signature to whatever the jd-core helper actually exposes — copy, don't reinvent.)

`crates/jd-app/tests/harness_smoke.rs`:

```rust
mod common;

use egui_kittest::Harness;
use jd_app::app::JdUi;

#[test]
fn harness_boots_app_and_finds_status_label() {
    let vault = common::temp_vault();
    let app = JdUi::new(vault.path()).expect("JdUi::new");
    let mut harness = Harness::builder().build_ui_state(
        |ui, app: &mut JdUi| app.ui(ui),
        app,
    );
    // The status line always shows the app name in WP2 Task 1.
    common::pump(&mut harness, &mut |_| false, 3, "warmup"); // let the initial scan land
    harness.run_ok();
    harness.get_by_label_contains("Junk Drawer");
}

#[test]
fn snapshot_pipeline_works() {
    // Deliberately trivial: proves wgpu software rendering + dify diffing on CI.
    let mut harness = Harness::new_ui(|ui| {
        ui.label("snapshot pipeline probe");
    });
    harness.run_ok();
    harness.snapshot("pipeline_probe");
}
```

Note: `JdUi` must be exported — add `pub mod app;` via a `src/lib.rs` (see Step 4). Integration tests can only see a lib target, so `jd-app` gains a `lib.rs`; `main.rs` uses the lib.

- [ ] **Step 3: Run to verify failure**

Run: `cargo test -p jd-app --test harness_smoke`
Expected: FAIL — `jd_app` has no lib / `JdUi` unresolved.

- [ ] **Step 4: Implement the split**

`crates/jd-app/src/lib.rs`:

```rust
//! jd-app library target: everything except the eframe shell, so
//! integration tests (egui_kittest) can drive the real UI headless.
pub mod app;
```

`crates/jd-app/src/app.rs`:

```rust
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
}

impl JdUi {
    pub fn new(vault_root: &Path) -> Result<JdUi, CoreError> {
        let vault = Vault::open(vault_root)?;
        let waker = Waker::default();
        let w = waker.clone();
        let handle = worker::start(vault, Box::new(move || w.wake()))?;
        Ok(JdUi { vault: handle, waker, scan_done: false, last_error: None })
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
        self.waker.attach(ui.ctx());
        self.drain_events();
        // Status line (bottom). Real surfaces land in Tasks 8-9.
        egui::TopBottomPanel::bottom("status_line").show_inside(ui, |ui| {
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
```

Note on `Vault::open`: check its actual signature in `crates/jd-core/src/vault/mod.rs` and adapt (it may want `PathBuf` or return `(Vault, ...)`). Do not change jd-core.

`crates/jd-app/src/main.rs` becomes:

```rust
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use jd_app::app::{JdApp, JdUi};

fn main() -> eframe::Result {
    // WP2: vault path = first CLI arg, else ~/JunkDrawer (created on demand).
    // Proper arg parsing / vault picker arrives with later WPs.
    let root = std::env::args().nth(1).map(std::path::PathBuf::from).unwrap_or_else(|| {
        std::env::home_dir().unwrap_or_else(|| std::path::PathBuf::from(".")).join("JunkDrawer")
    });
    let ui = JdUi::new(&root).expect("failed to open vault");
    let options = eframe::NativeOptions::default();
    eframe::run_native("Junk Drawer", options, Box::new(|_cc| Ok(Box::new(JdApp(ui)))))
}
```

(If `std::env::home_dir` is deprecated-and-warning on this toolchain, read `$HOME`/`%USERPROFILE%` manually — clippy must stay clean; no new deps.)

- [ ] **Step 5: Generate the golden snapshot + run tests**

Run: `UPDATE_SNAPSHOTS=force cargo test -p jd-app --test harness_smoke`
Then: `cargo test -p jd-app --test harness_smoke`
Expected: PASS both times; `crates/jd-app/tests/snapshots/pipeline_probe.png` exists → `git add` it.

- [ ] **Step 6: CI plumbing for headless wgpu**

In `.github/workflows/ci.yml`, add to the test job, **before** the test step, linux-only:

```yaml
      - name: Install Mesa (software Vulkan for egui_kittest snapshots)
        if: runner.os == 'Linux'
        run: |
          sudo apt-get update
          sudo apt-get install -y libvulkan1 mesa-vulkan-drivers
```

macOS runners have Metal; Windows falls back to DX12 WARP — no steps needed there.

- [ ] **Step 7: Full check + commit**

Run: `cargo test -p jd-app && cargo clippy --workspace --all-targets -- -D warnings && cargo fmt --check`
Expected: all green (run `cargo fmt` first if needed).

```bash
git add -A
git commit -m "test(app): egui_kittest harness, JdUi/JdApp split, snapshot pipeline"
```

---

### Task 2: Spike A — mixed-size text in the TextEdit layouter

**This is a spike (spec §14 risk 1).** Deliverable = working proof OR a recorded fallback decision, plus the exit-criteria tests. Timebox: if after honest effort cursor/selection can NOT be made correct across a size boundary, STOP, implement the fallback (uniform size; headings styled via color/underline + bold family only), and record the outcome. Either way the architecture doc gains **decision §6.13** documenting what was proven and chosen (the controller writes it from your report).

**Files:**
- Create: `crates/jd-app/src/editor.rs` (only the layouter core this task; the editor window is Task 10)
- Modify: `crates/jd-app/src/lib.rs` (`pub mod editor;`)
- Create: `crates/jd-app/tests/spike_layouter.rs`

**Interfaces:**
- Consumes: `jd_core::lexer::{lex_line, LineState, SpanStyle, StyledSpan}`.
- Produces: `editor::LineCache` (default-constructible), `editor::layout_body(ui: &egui::Ui, text: &str, wrap_width: f32, cache: &mut LineCache, resolve: &dyn Fn(&str) -> bool) -> Arc<egui::Galley>` — Task 7 (card faces) and Task 10 (editor window) both call this.
- Heading sizes pinned here, consumed by theme Task 4: body 15.0 pt; `Heading(1)` 24.0, `(2)` 20.0, `(3)` 17.0. Mono size 14.0.

- [ ] **Step 1: Write the failing exit-criteria tests**

`crates/jd-app/tests/spike_layouter.rs`:

```rust
mod common;

use eframe::egui;
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;

const SAMPLE: &str = "# The heading line\nbody text under it\nmore body";

fn edit_harness(initial: &str) -> Harness<'static, String> {
    let mut cache = jd_app::editor::LineCache::default();
    Harness::builder().build_ui_state(
        move |ui, text: &mut String| {
            let mut layouter = |ui: &egui::Ui, buf: &dyn egui::TextBuffer, wrap: f32| {
                jd_app::editor::layout_body(ui, buf.as_str(), wrap, &mut cache, &|_| false)
            };
            ui.add(
                egui::TextEdit::multiline(text)
                    .desired_width(400.0)
                    .layouter(&mut layouter),
            );
        },
        initial.to_owned(),
    )
}

/// Exit criterion 1: the galley really is mixed-size (heading row taller than body row).
/// No harness needed — inspect the layouter output directly.
#[test]
fn heading_row_is_taller_than_body_row() {
    let ctx = egui::Context::default();
    let _ = ctx.run(Default::default(), |ctx| {
        egui::CentralPanel::default().show(ctx, |ui| {
            let mut cache = jd_app::editor::LineCache::default();
            let galley = jd_app::editor::layout_body(ui, SAMPLE, 400.0, &mut cache, &|_| false);
            assert!(galley.rows.len() >= 3, "expected one row per line");
            let h0 = galley.rows[0].rect().height();
            let h1 = galley.rows[1].rect().height();
            assert!(h0 > h1 * 1.3, "heading row ({h0}) must be visibly taller than body ({h1})");
        });
    });
}

/// Exit criterion 2: typing at the end of the heading line inserts THERE, not
/// at a drifted position (cursor mapping across the size boundary is sound).
#[test]
fn typing_across_size_boundary_lands_where_the_cursor_is() {
    let mut h = edit_harness(SAMPLE);
    h.run_ok();
    let edit = h.get_by_role(egui_kittest::kittest::Role::MultilineTextInput);
    edit.click();
    h.run_ok();
    // Deterministic cursor placement: top of buffer, then end of line 1.
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::Home); // Ctrl+Home on win/linux — kittest maps COMMAND per-platform; if not, use two presses of ArrowUp
    h.run_ok();
    h.key_press(egui::Key::End);
    h.run_ok();
    h.get_by_role(egui_kittest::kittest::Role::MultilineTextInput).type_text("!");
    h.run_ok();
    assert!(
        h.state().starts_with("# The heading line!\nbody text under it"),
        "insert landed at end of the heading line, got: {}",
        h.state()
    );
    // And across the boundary: ArrowDown+End then type — lands at end of line 2.
    h.key_press(egui::Key::ArrowDown);
    h.key_press(egui::Key::End);
    h.run_ok();
    h.get_by_role(egui_kittest::kittest::Role::MultilineTextInput).type_text("?");
    h.run_ok();
    assert!(h.state().contains("body text under it?"), "got: {}", h.state());
}

/// Exit criterion 3: select-all covers the full raw source (selection geometry
/// spans size boundaries without dropping lines).
#[test]
fn select_all_spans_the_boundary() {
    let mut h = edit_harness(SAMPLE);
    h.run_ok();
    let edit = h.get_by_role(egui_kittest::kittest::Role::MultilineTextInput);
    edit.click();
    h.run_ok();
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
    h.run_ok();
    // After select-all, typing replaces everything: state proves selection covered all.
    h.get_by_role(egui_kittest::kittest::Role::MultilineTextInput).type_text("x");
    h.run_ok();
    assert_eq!(h.state(), "x");
}
```

**Spike latitude:** the exact kittest API surface (`key_press_modifiers`, `type_text`, `Role::MultilineTextInput`, whether `Modifiers::COMMAND` maps to Ctrl on win/linux) must be verified against `egui_kittest` 0.35 docs and adapted — the *criteria* (taller row, no cursor drift, boundary-spanning selection) are fixed and may not be weakened.

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p jd-app --test spike_layouter`
Expected: FAIL — `jd_app::editor` does not exist.

- [ ] **Step 3: Implement the layouter core**

`crates/jd-app/src/editor.rs`:

```rust
//! Editor internals. This task: the mixed-size layouter (Spike A).
//! The floating editor window, behaviors, autocomplete arrive in Tasks 10-12.

use std::collections::HashMap;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::sync::Arc;

use eframe::egui::{self, FontFamily, FontId, TextFormat, text::LayoutJob};
use jd_core::lexer::{LineState, SpanStyle, StyledSpan, lex_line};

/// Per-line lexing is O(n^2) on pathological unclosed-delimiter lines
/// (WP1b review). Lines are lexed only up to this many bytes; the rest is
/// styled as plain text. Invisible for human-authored notes.
pub const MAX_LEXED_LINE_BYTES: usize = 8 * 1024;

pub const BODY_SIZE: f32 = 15.0;
pub const MONO_SIZE: f32 = 14.0;
pub fn heading_size(level: u8) -> f32 {
    match level {
        1 => 24.0,
        2 => 20.0,
        _ => 17.0,
    }
}

#[derive(Clone, PartialEq, Eq, Hash)]
struct LineKey {
    hash: u64,
    entry: LineState,
}

/// Cache: (line content hash, entry fence state) → (spans, exit state).
/// Only edited lines re-lex; a fence toggle upstream changes entry states
/// downstream, which changes keys, which re-lexes exactly the affected lines.
#[derive(Default)]
pub struct LineCache {
    map: HashMap<LineKey, (Vec<StyledSpan>, LineState)>,
}

fn line_key(line: &str, entry: LineState) -> LineKey {
    let mut h = DefaultHasher::new();
    line.hash(&mut h);
    LineKey { hash: h.finish(), entry }
}

fn lex_capped(
    line: &str,
    entry: LineState,
    resolve: &dyn Fn(&str) -> bool,
) -> (Vec<StyledSpan>, LineState) {
    if line.len() <= MAX_LEXED_LINE_BYTES {
        return lex_line(line, entry, resolve);
    }
    // Cap: lex the head (back off to a char boundary), tail is plain Text.
    let mut cut = MAX_LEXED_LINE_BYTES;
    while !line.is_char_boundary(cut) {
        cut -= 1;
    }
    let (mut spans, exit) = lex_line(&line[..cut], entry, resolve);
    spans.push(StyledSpan { range: cut..line.len(), style: SpanStyle::Text });
    (spans, exit)
}

/// Map a lexer span to an egui TextFormat. Colors come from theme.rs from
/// Task 4 on; the spike uses egui's current visuals so it stands alone.
fn format_for(style: SpanStyle, visuals: &egui::Visuals) -> TextFormat {
    let body = FontId::new(BODY_SIZE, FontFamily::Proportional);
    let mono = FontId::new(MONO_SIZE, FontFamily::Monospace);
    let text = visuals.text_color();
    let weak = visuals.weak_text_color();
    let accent = visuals.hyperlink_color;
    let mut f = TextFormat::simple(body.clone(), text);
    match style {
        SpanStyle::Text | SpanStyle::ListMarker => {}
        SpanStyle::Heading(n) => {
            f.font_id = FontId::new(heading_size(n), FontFamily::Proportional);
            // Bold family arrives with theme.rs (Task 4); size alone carries the spike.
        }
        SpanStyle::HeadingMarker => {
            f.font_id = FontId::new(heading_size(1), FontFamily::Proportional);
            f.color = weak;
        }
        SpanStyle::Bold | SpanStyle::BoldItalic => { /* bold family in Task 4 */ }
        SpanStyle::Italic => f.italics = true,
        SpanStyle::Strike => f.strikethrough = egui::Stroke::new(1.0, text),
        SpanStyle::InlineCode | SpanStyle::CodeBlock | SpanStyle::CodeFenceMarker => {
            f.font_id = mono;
            f.background = visuals.extreme_bg_color;
        }
        SpanStyle::TaskBoxUnchecked | SpanStyle::TaskBoxChecked => f.color = weak,
        SpanStyle::QuoteMarker => f.color = weak,
        SpanStyle::Quote => f.italics = true,
        SpanStyle::WikiLink { resolved } => {
            f.color = accent;
            if !resolved {
                f.underline = egui::Stroke::new(1.0, accent); // dashed styling refined in Task 4
            }
        }
        SpanStyle::Tag => f.color = accent,
        SpanStyle::Url | SpanStyle::MdLinkUrl => {
            f.color = accent;
            f.underline = egui::Stroke::new(1.0, accent);
        }
        SpanStyle::MdLinkText => f.color = accent,
    }
    f
}

/// Build the mixed-size galley for `text`. HEADING SIZES ARE REAL SIZES —
/// this is the spike's whole bet. One LayoutJob for the entire buffer;
/// egui's TextEdit maps cursor hits through the galley, so as long as the
/// job's byte ranges exactly tile the buffer, cursor/selection/IME inherit
/// correctness from TextEdit itself.
pub fn layout_body(
    ui: &egui::Ui,
    text: &str,
    wrap_width: f32,
    cache: &mut LineCache,
    resolve: &dyn Fn(&str) -> bool,
) -> Arc<egui::Galley> {
    let mut job = LayoutJob::default();
    job.wrap.max_width = wrap_width;
    let visuals = ui.visuals();
    let mut state = LineState::Normal;
    let mut offset = 0usize;
    for (i, line) in text.split('\n').enumerate() {
        if i > 0 {
            // The '\n' itself: append with the BODY format so every byte of
            // the buffer is present in the job (cursor mapping requirement).
            job.append("\n", 0.0, format_for(SpanStyle::Text, visuals));
            offset += 1;
        }
        let key = line_key(line, state);
        let (spans, exit) = cache
            .map
            .entry(key)
            .or_insert_with(|| lex_capped(line, state, resolve))
            .clone();
        if spans.is_empty() {
            // empty line: nothing to append beyond the newline handled above
        }
        for s in &spans {
            job.append(&line[s.range.clone()], 0.0, format_for(s.style, visuals));
        }
        state = exit;
        offset += line.len();
    }
    let _ = offset;
    ui.fonts(|f| f.layout_job(job))
}
```

API-adaptation latitude: `ui.fonts(|f| f.layout_job(job))` and `TextFormat` field names must match egui 0.35 exactly — fix compile errors against the real API without changing the *architecture* (one LayoutJob tiling the whole buffer; per-line cache; capped lexing).

- [ ] **Step 4: Make the exit-criteria tests real and run them**

Finish `typing_across_size_boundary_lands_where_clicked` per the Step 1 notes. Run: `cargo test -p jd-app --test spike_layouter`
Expected: PASS all three. **If cursor drift is irreparable** (egui maps clicks through galley rows, so failure would mean egui itself can't handle mixed row heights): implement the fallback (all sizes = BODY_SIZE; headings get accent color + bold once Task 4 lands), adjust criterion 1 to assert equal row heights, and report the fallback outcome as the spike result.

- [ ] **Step 5: Manual IME note**

Automated IME testing isn't feasible in kittest. Report in your completion summary: the architecture keeps TextEdit's own IME machinery (we only supply the galley), so IME risk reduces to galley-row geometry — covered by criteria 1–2. Flag "manual IME pass on macOS (Japanese input)" for the WP8 release checklist.

- [ ] **Step 6: Full check + commit**

Run: `cargo test -p jd-app && cargo clippy --workspace --all-targets -- -D warnings && cargo fmt --check`

```bash
git add -A
git commit -m "feat(app): spike A - mixed-size markdown layouter with line cache"
```

---

### Task 3: Spike B — AccessKit spatial focus on a free-form canvas

**This is a spike (spec §14 risk 3), built alongside the real desk, not as a throwaway.** The spatial-order function and the focus mechanics written here ARE the shipping code (they land in `surfaces/desk.rs`); only the rendering around them is minimal. Exit criteria: kittest (which sees exactly what a screen reader sees) can find every card by its announcement label, and arrow keys traverse in spatial reading order.

**Files:**
- Create: `crates/jd-app/src/surfaces/mod.rs`, `crates/jd-app/src/surfaces/desk.rs` (focus machinery only; pan/zoom/drag arrive in Task 8)
- Modify: `crates/jd-app/src/lib.rs` (`pub mod surfaces;`)
- Create: `crates/jd-app/tests/spike_focus.rs`

**Interfaces:**
- Consumes: `jd_core::geom::Vec2`, `jd_core::id::NoteId`.
- Produces (consumed by Tasks 7–9):
  - `desk::BAND_HEIGHT: f32 = 120.0` (0.6 × index-card height 200.0)
  - `desk::reading_order(cards: &[(NoteId, jd_core::geom::Vec2)]) -> Vec<NoteId>` — pure; sort key `((pos.y / BAND_HEIGHT).round() as i64, pos.x)`; ties broken by NoteId for determinism.
  - `desk::next_focus(cards: &[(NoteId, Vec2)], current: Option<NoteId>, dir: FocusDir) -> Option<NoteId>` with `pub enum FocusDir { Left, Right, Up, Down }` — Left/Right walk the reading order; Up/Down move to the nearest card by |Δx| in the adjacent band (searching outward band by band); no wrap-around (spatial honesty: at the edge, focus stays).
  - `desk::card_a11y_label(title: &str, first_line: &str, is_scrap: bool, links: usize, tags: usize) -> String` — permanent: `Card: '<title>', N links, M tags` (singular/plural correct: `1 link`, `2 links`); scrap: `Scrap: '<first line>'`.

- [ ] **Step 1: Write failing unit tests for the pure functions**

At the bottom of the new `crates/jd-app/src/surfaces/desk.rs` (write the `#[cfg(test)]` module FIRST, with stub-free intent — the impl follows in Step 3):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use jd_core::geom::Vec2;
    use jd_core::id::NoteId;

    fn id(n: u8) -> NoteId {
        // NoteId parses from a 26-char ULID string; build distinct ids cheaply.
        let s = format!("01ARZ3NDEKTSV4RRFFQ69G5F{:02}", n);
        s.parse().unwrap_or_else(|_| panic!("bad test ulid {s}"))
    }

    #[test]
    fn reading_order_is_bands_then_x() {
        // Band height 120: y=10 and y=50 share band 0; y=200 is band 2.
        let cards = vec![
            (id(1), Vec2 { x: 300.0, y: 10.0 }),
            (id(2), Vec2 { x: 5.0, y: 50.0 }),
            (id(3), Vec2 { x: 0.0, y: 200.0 }),
        ];
        assert_eq!(reading_order(&cards), vec![id(2), id(1), id(3)]);
    }

    #[test]
    fn reading_order_stable_under_small_drags() {
        let before = vec![(id(1), Vec2 { x: 0.0, y: 100.0 }), (id(2), Vec2 { x: 200.0, y: 130.0 })];
        // 20px vertical wiggle does not change bands (100→0.83 rounds 1; 120→1).
        let after = vec![(id(1), Vec2 { x: 0.0, y: 120.0 }), (id(2), Vec2 { x: 200.0, y: 110.0 })];
        assert_eq!(reading_order(&before), reading_order(&after));
    }

    #[test]
    fn arrows_traverse_and_do_not_wrap() {
        let cards = vec![
            (id(1), Vec2 { x: 0.0, y: 0.0 }),
            (id(2), Vec2 { x: 400.0, y: 0.0 }),
            (id(3), Vec2 { x: 100.0, y: 300.0 }),
        ];
        assert_eq!(next_focus(&cards, Some(id(1)), FocusDir::Right), Some(id(2)));
        assert_eq!(next_focus(&cards, Some(id(2)), FocusDir::Right), None); // no wrap
        assert_eq!(next_focus(&cards, Some(id(1)), FocusDir::Down), Some(id(3)));
        assert_eq!(next_focus(&cards, Some(id(3)), FocusDir::Up), Some(id(1))); // nearest |Δx|
        assert_eq!(next_focus(&cards, None, FocusDir::Right), Some(id(1))); // no focus → first
    }

    #[test]
    fn a11y_labels_match_spec() {
        assert_eq!(
            card_a11y_label("Immediate mode trades layout power for state simplicity", "", false, 3, 2),
            "Card: 'Immediate mode trades layout power for state simplicity', 3 links, 2 tags"
        );
        assert_eq!(card_a11y_label("T", "", false, 1, 0), "Card: 'T', 1 link, 0 tags");
        assert_eq!(card_a11y_label("", "buy milk", true, 0, 0), "Scrap: 'buy milk'");
    }
}
```

- [ ] **Step 2: Run to verify failure** — `cargo test -p jd-app --lib` → FAIL (module missing).

- [ ] **Step 3: Implement the pure focus machinery**

`crates/jd-app/src/surfaces/mod.rs`: `pub mod desk;`

`crates/jd-app/src/surfaces/desk.rs` (top half — the canvas render fn joins it in Task 8):

```rust
//! The desk surface. This file owns spatial focus order (Spike B) and,
//! from Task 8, the pannable canvas itself.

use jd_core::geom::Vec2;
use jd_core::id::NoteId;

/// 0.6 × index-card height (200.0). Rounded y-bands make reading order
/// stable under small drags (architecture §3, spec §12).
pub const BAND_HEIGHT: f32 = 120.0;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FocusDir { Left, Right, Up, Down }

fn band(y: f32) -> i64 { (y / BAND_HEIGHT).round() as i64 }

pub fn reading_order(cards: &[(NoteId, Vec2)]) -> Vec<NoteId> {
    let mut v: Vec<&(NoteId, Vec2)> = cards.iter().collect();
    v.sort_by(|a, b| {
        band(a.1.y).cmp(&band(b.1.y))
            .then(a.1.x.total_cmp(&b.1.x))
            .then(a.0.cmp(&b.0))
    });
    v.into_iter().map(|(id, _)| *id).collect()
}

pub fn next_focus(cards: &[(NoteId, Vec2)], current: Option<NoteId>, dir: FocusDir) -> Option<NoteId> {
    if cards.is_empty() { return None; }
    let order = reading_order(cards);
    let Some(cur) = current else { return order.first().copied() };
    let Some(idx) = order.iter().position(|id| *id == cur) else { return order.first().copied() };
    match dir {
        FocusDir::Left => idx.checked_sub(1).map(|i| order[i]),
        FocusDir::Right => order.get(idx + 1).copied(),
        FocusDir::Up | FocusDir::Down => {
            let pos = cards.iter().find(|(id, _)| *id == cur)?.1;
            let cur_band = band(pos.y);
            let step: i64 = if dir == FocusDir::Down { 1 } else { -1 };
            // Search outward band by band for the nearest card by |Δx|.
            let bands: std::collections::BTreeSet<i64> =
                cards.iter().map(|(_, p)| band(p.y)).collect();
            let mut target = cur_band + step;
            let (min_b, max_b) = (*bands.iter().next()?, *bands.iter().last()?);
            while target >= min_b && target <= max_b {
                let mut best: Option<(f32, NoteId)> = None;
                for (id, p) in cards {
                    if band(p.y) == target {
                        let dx = (p.x - pos.x).abs();
                        if best.map_or(true, |(bd, bid)| dx < bd || (dx == bd && *id < bid)) {
                            best = Some((dx, *id));
                        }
                    }
                }
                if let Some((_, id)) = best { return Some(id); }
                target += step;
            }
            None
        }
    }
}

pub fn card_a11y_label(title: &str, first_line: &str, is_scrap: bool, links: usize, tags: usize) -> String {
    if is_scrap {
        return format!("Scrap: '{first_line}'");
    }
    let l = if links == 1 { "link" } else { "links" };
    let t = if tags == 1 { "tag" } else { "tags" };
    format!("Card: '{title}', {links} {l}, {tags} {t}")
}
```

Check `NoteId`'s actual constructors (`FromStr`? `NoteId::parse`?) in `jd-core/src/id.rs` and adapt the test helper; `NoteId` is `Ord` (verified: derives PartialOrd/Ord).

- [ ] **Step 4: Run unit tests** — `cargo test -p jd-app --lib` → PASS.

- [ ] **Step 5: Write the failing kittest canvas test**

`crates/jd-app/tests/spike_focus.rs`:

```rust
mod common;

use eframe::egui;
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use jd_core::geom::Vec2;

/// Minimal spike canvas: real focus machinery, dummy rendering.
/// Proves: cards exist as labeled AccessKit nodes; arrows move focus spatially.
struct SpikeDesk {
    cards: Vec<(jd_core::id::NoteId, Vec2, String)>, // id, pos, title
    focus: Option<jd_core::id::NoteId>,
}

impl SpikeDesk {
    fn ui(&mut self, ui: &mut egui::Ui) {
        use jd_app::surfaces::desk::{FocusDir, card_a11y_label, next_focus};
        let positions: Vec<_> = self.cards.iter().map(|(id, p, _)| (*id, *p)).collect();
        // Arrow handling BEFORE widgets, mirroring the real desk's dispatch.
        for (key, dir) in [
            (egui::Key::ArrowLeft, FocusDir::Left),
            (egui::Key::ArrowRight, FocusDir::Right),
            (egui::Key::ArrowUp, FocusDir::Up),
            (egui::Key::ArrowDown, FocusDir::Down),
        ] {
            if ui.input(|i| i.key_pressed(key)) {
                if let Some(next) = next_focus(&positions, self.focus, dir) {
                    self.focus = Some(next);
                }
            }
        }
        for (id, pos, title) in &self.cards {
            let rect = egui::Rect::from_min_size(
                egui::pos2(pos.x, pos.y) + ui.min_rect().min.to_vec2(),
                egui::vec2(150.0, 90.0),
            );
            let label = card_a11y_label(title, "", false, 0, 0);
            let resp = ui
                .allocate_rect(rect, egui::Sense::click())
                .on_hover_text(title);
            // The spike's core claim: a free-form-positioned widget can carry
            // proper AccessKit semantics.
            resp.widget_info(|| {
                egui::WidgetInfo::labeled(egui::WidgetType::Button, true, label.clone())
            });
            if self.focus == Some(*id) {
                resp.request_focus();
                ui.painter().rect_stroke(
                    rect, 4.0,
                    egui::Stroke::new(2.0, ui.visuals().selection.stroke.color),
                    egui::StrokeKind::Outside,
                );
            } else {
                ui.painter().rect_stroke(
                    rect, 4.0,
                    egui::Stroke::new(1.0, ui.visuals().weak_text_color()),
                    egui::StrokeKind::Outside,
                );
            }
        }
    }
}

fn make_desk() -> SpikeDesk {
    let ids: Vec<_> = (1..=4u8)
        .map(|n| format!("01ARZ3NDEKTSV4RRFFQ69G5F{n:02}").parse().unwrap())
        .collect();
    SpikeDesk {
        cards: vec![
            (ids[0], Vec2 { x: 20.0, y: 20.0 }, "Alpha".into()),
            (ids[1], Vec2 { x: 300.0, y: 30.0 }, "Beta".into()),
            (ids[2], Vec2 { x: 40.0, y: 250.0 }, "Gamma".into()),
            (ids[3], Vec2 { x: 320.0, y: 260.0 }, "Delta".into()),
        ],
        focus: None,
    }
}

#[test]
fn every_card_is_a_labeled_accesskit_node() {
    let mut h = Harness::builder().build_ui_state(|ui, d: &mut SpikeDesk| d.ui(ui), make_desk());
    h.run_ok();
    for name in ["Alpha", "Beta", "Gamma", "Delta"] {
        h.get_by_label_contains(&format!("Card: '{name}'"));
    }
}

#[test]
fn arrows_walk_reading_order_across_the_canvas() {
    let mut h = Harness::builder().build_ui_state(|ui, d: &mut SpikeDesk| d.ui(ui), make_desk());
    h.run_ok();
    let ids = make_desk().cards.iter().map(|(id, _, _)| *id).collect::<Vec<_>>();
    h.key_press(egui::Key::ArrowRight); h.run_ok();
    assert_eq!(h.state().focus, Some(ids[0]), "first Right lands on Alpha");
    h.key_press(egui::Key::ArrowRight); h.run_ok();
    assert_eq!(h.state().focus, Some(ids[1]), "Beta is next in band 0");
    h.key_press(egui::Key::ArrowDown); h.run_ok();
    assert_eq!(h.state().focus, Some(ids[3]), "Down from Beta → Delta (nearest |dx|)");
    h.key_press(egui::Key::ArrowLeft); h.run_ok();
    assert_eq!(h.state().focus, Some(ids[2]), "Left in band 2 → Gamma");
    h.key_press(egui::Key::ArrowLeft); h.run_ok();
    // Reading order: Left from Gamma goes back up to Beta (previous in order).
    assert_eq!(h.state().focus, Some(ids[1]));
}
```

API-adaptation latitude (spike): exact kittest query/key methods, `widget_info` availability on `Response`, `StrokeKind` — verify against egui/egui_kittest 0.35 and adapt mechanics, not criteria. If `allocate_rect + widget_info` does not produce a queryable AccessKit node, the known-good alternative is `ui.put(rect, egui::Button::new(...))` or a child `Ui` with `ui.label`/`Sense::click` — find the pattern that works and REPORT which one; Task 7's CardWidget builds on it.

- [ ] **Step 6: Run, iterate to green** — `cargo test -p jd-app --test spike_focus` → PASS. This is the spike: if the node pattern fails, iterate here, in the small.

- [ ] **Step 7: Full check + commit**

Run: `cargo test -p jd-app && cargo clippy --workspace --all-targets -- -D warnings && cargo fmt --check`

```bash
git add -A
git commit -m "feat(app): spike B - AccessKit spatial focus machinery for free-form canvas"
```

---

### Task 4: theme.rs — bundled fonts, palettes, WCAG test, span formats

**Files:**
- Create: `crates/jd-app/assets/fonts/` (5 font files + 2 license files)
- Create: `crates/jd-app/src/theme.rs`
- Modify: `crates/jd-app/src/lib.rs` (`pub mod theme;`), `crates/jd-app/src/editor.rs` (swap `format_for` to theme-driven), `crates/jd-app/src/app.rs` (install fonts once)

**Interfaces:**
- Consumes: `SpanStyle` (lexer), Spike A's pinned sizes (`BODY_SIZE` 15.0, `MONO_SIZE` 14.0, `heading_size`).
- Produces:
  - Font family names (egui `FontFamily::Name`): `"inter"` (also set as Proportional default), `"inter-bold"`, `"inter-italic"`, `"inter-bold-italic"`, `"jbmono"` (also Monospace default).
  - `theme::install_fonts(ctx: &egui::Context)` — idempotent.
  - `pub struct Theme { pub dark: bool, /* named Color32 constants as fields */ }` with `Theme::light()`, `Theme::dark()`; fields (all `egui::Color32`): `desk_bg`, `card_paper_cream`, `card_plain_bg`, `card_border`, `card_shadow`, `text`, `text_weak`, `accent` (wikilinks/tags/urls), `tag_pill_bg`, `code_bg`, `rule_red`, `rule_blue`, `rule_ink`, `focus_ring`, `error_text`, `divider_tab_bg`, `footer_bg`.
  - `theme::text_format(style: SpanStyle, theme: &Theme) -> egui::TextFormat` — the single mapping used by editor AND card faces (moves/absorbs Spike A's `format_for`; unresolved wikilinks get `underline` (egui has no dashed underline; the "dashed" affordance is: underline + `text_weak` color instead of accent — record as part of decision §6.13 notes)).
  - `theme::RULE_SPACING: f32 = 22.0`, `theme::RULE_TOP_OFFSET: f32 = 34.0` (below the header rule), used by card rules drawing.
- Card-face metric constants live in `card/shape.rs` (Task 6), not here.

- [ ] **Step 1: Vendor the fonts**

Download (curl; if the sandbox blocks network, ask the controller to fetch):
- Inter v4.1 from `https://github.com/rsms/inter/releases/download/v4.1/Inter-4.1.zip` → extract `extras/ttf/` (or `ttf/`) `Inter-Regular.ttf`, `Inter-Bold.ttf`, `Inter-Italic.ttf`, `Inter-BoldItalic.ttf`
- JetBrains Mono v2.304 from `https://github.com/JetBrains/JetBrainsMono/releases/download/v2.304/JetBrainsMono-2.304.zip` → `fonts/ttf/JetBrainsMono-Regular.ttf`

Place in `crates/jd-app/assets/fonts/` together with each family's `OFL.txt` (rename: `LICENSE-Inter-OFL.txt`, `LICENSE-JetBrainsMono-OFL.txt`). Both are SIL OFL 1.1 — bundling is fine with license text included. Static instances, NOT the variable font (egui doesn't drive variation axes). Verify: 5 `.ttf` + 2 licenses, each ttf < 900 KB.

- [ ] **Step 2: Write the failing tests**

In `crates/jd-app/src/theme.rs`'s `#[cfg(test)]` module (written first):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use eframe::egui::Color32;

    /// WCAG relative luminance (sRGB).
    fn lum(c: Color32) -> f64 {
        fn chan(u: u8) -> f64 {
            let s = u as f64 / 255.0;
            if s <= 0.04045 { s / 12.92 } else { ((s + 0.055) / 1.055).powf(2.4) }
        }
        0.2126 * chan(c.r()) + 0.7152 * chan(c.g()) + 0.0722 * chan(c.b())
    }
    fn contrast(a: Color32, b: Color32) -> f64 {
        let (l1, l2) = (lum(a).max(lum(b)), lum(a).min(lum(b)));
        (l1 + 0.05) / (l2 + 0.05)
    }

    #[test]
    fn wcag_aa_for_every_used_pair() {
        for theme in [Theme::light(), Theme::dark()] {
            // TEXT pairs (≥ 4.5) — every (fg, bg) actually drawn:
            let text_pairs: &[(&str, Color32, Color32)] = &[
                ("body on paper", theme.text, theme.card_paper_cream),
                ("body on plain", theme.text, theme.card_plain_bg),
                ("weak on paper", theme.text_weak, theme.card_paper_cream),
                ("weak on plain", theme.text_weak, theme.card_plain_bg),
                ("accent on paper", theme.accent, theme.card_paper_cream),
                ("accent on plain", theme.accent, theme.card_plain_bg),
                ("accent on tag pill", theme.accent, theme.tag_pill_bg),
                ("code on code bg", theme.text, theme.code_bg),
                ("text on desk (status)", theme.text, theme.desk_bg),
                ("error on desk", theme.error_text, theme.desk_bg),
                ("title on divider tab", theme.text, theme.divider_tab_bg),
                ("source on footer", theme.text_weak, theme.footer_bg),
            ];
            for (what, fg, bg) in text_pairs {
                assert!(contrast(*fg, *bg) >= 4.5,
                    "{} ({:?}): {:.2} < 4.5 [dark={}]", what, (fg, bg), contrast(*fg, *bg), theme.dark);
            }
            // AFFORDANCE pairs (≥ 3.0):
            let ui_pairs: &[(&str, Color32, Color32)] = &[
                ("card border on desk", theme.card_border, theme.desk_bg),
                ("focus ring on desk", theme.focus_ring, theme.desk_bg),
                ("focus ring on paper", theme.focus_ring, theme.card_paper_cream),
            ];
            for (what, fg, bg) in ui_pairs {
                assert!(contrast(*fg, *bg) >= 3.0,
                    "{} : {:.2} < 3.0 [dark={}]", what, contrast(*fg, *bg), theme.dark);
            }
            // Ruled lines are decoration (spec: "text never snaps to them") — exempt,
            // but they must not accidentally out-contrast text: keep them subtle.
            assert!(contrast(theme.rule_blue, theme.card_paper_cream) < 4.5, "rules stay quiet");
        }
    }

    #[test]
    fn fonts_are_bundled_and_parse() {
        // include_bytes! proves the assets exist at compile time; FontDefinitions
        // construction proves egui accepts them.
        let defs = font_definitions();
        for fam in ["inter", "inter-bold", "inter-italic", "inter-bold-italic", "jbmono"] {
            assert!(defs.families.contains_key(&eframe::egui::FontFamily::Name(fam.into())),
                "missing family {fam}");
        }
    }
}
```

- [ ] **Step 3: Run to verify failure** — `cargo test -p jd-app --lib theme` → FAIL (module missing).

- [ ] **Step 4: Implement theme.rs**

```rust
//! All colors, fonts, and text-format mapping. Every color used anywhere in
//! jd-app is a named constant here, WCAG-checked by test.

use eframe::egui::{self, Color32, FontData, FontDefinitions, FontFamily, FontId, TextFormat};
use jd_core::lexer::SpanStyle;

use crate::editor::{BODY_SIZE, MONO_SIZE, heading_size};

pub const RULE_SPACING: f32 = 22.0;
pub const RULE_TOP_OFFSET: f32 = 34.0;

pub fn font_definitions() -> FontDefinitions {
    let mut d = FontDefinitions::default();
    let fonts: &[(&str, &[u8])] = &[
        ("inter", include_bytes!("../assets/fonts/Inter-Regular.ttf")),
        ("inter-bold", include_bytes!("../assets/fonts/Inter-Bold.ttf")),
        ("inter-italic", include_bytes!("../assets/fonts/Inter-Italic.ttf")),
        ("inter-bold-italic", include_bytes!("../assets/fonts/Inter-BoldItalic.ttf")),
        ("jbmono", include_bytes!("../assets/fonts/JetBrainsMono-Regular.ttf")),
    ];
    for (name, bytes) in fonts {
        d.font_data.insert((*name).into(), FontData::from_static(bytes).into());
        d.families.insert(FontFamily::Name((*name).into()), vec![(*name).into()]);
    }
    // Bundled faces first; egui's built-ins stay as fallback for uncovered
    // scripts (system-font fallback proper is a WP7 concern).
    d.families.get_mut(&FontFamily::Proportional).unwrap().insert(0, "inter".into());
    d.families.get_mut(&FontFamily::Monospace).unwrap().insert(0, "jbmono".into());
    d
}

pub fn install_fonts(ctx: &egui::Context) {
    ctx.set_fonts(font_definitions());
}

pub struct Theme {
    pub dark: bool,
    pub desk_bg: Color32,
    pub card_paper_cream: Color32,
    pub card_plain_bg: Color32,
    pub card_border: Color32,
    pub card_shadow: Color32,
    pub text: Color32,
    pub text_weak: Color32,
    pub accent: Color32,
    pub tag_pill_bg: Color32,
    pub code_bg: Color32,
    pub rule_red: Color32,
    pub rule_blue: Color32,
    pub rule_ink: Color32,
    pub focus_ring: Color32,
    pub error_text: Color32,
    pub divider_tab_bg: Color32,
    pub footer_bg: Color32,
}

impl Theme {
    pub fn light() -> Theme {
        Theme {
            dark: false,
            desk_bg: Color32::from_rgb(0xE8, 0xE4, 0xDC),          // warm gray desk
            card_paper_cream: Color32::from_rgb(0xFB, 0xF7, 0xEB), // cream stock
            card_plain_bg: Color32::from_rgb(0xFF, 0xFF, 0xFF),
            card_border: Color32::from_rgb(0x8A, 0x84, 0x78),
            card_shadow: Color32::from_black_alpha(40),
            text: Color32::from_rgb(0x26, 0x24, 0x20),
            text_weak: Color32::from_rgb(0x6B, 0x66, 0x5C),
            accent: Color32::from_rgb(0x1A, 0x56, 0xA0),
            tag_pill_bg: Color32::from_rgb(0xE4, 0xEC, 0xF6),
            code_bg: Color32::from_rgb(0xEF, 0xEA, 0xDD),
            rule_red: Color32::from_rgb(0xD9, 0x8A, 0x8A),
            rule_blue: Color32::from_rgb(0xB9, 0xC8, 0xDD),
            rule_ink: Color32::from_rgb(0x4A, 0x52, 0x60),         // used on dark only
            focus_ring: Color32::from_rgb(0x1A, 0x56, 0xA0),
            error_text: Color32::from_rgb(0x9E, 0x2A, 0x2A),
            divider_tab_bg: Color32::from_rgb(0xEA, 0xDF, 0xC8),
            footer_bg: Color32::from_rgb(0xF1, 0xEA, 0xD8),
        }
    }
    pub fn dark() -> Theme {
        Theme {
            dark: true,
            desk_bg: Color32::from_rgb(0x1E, 0x1F, 0x22),
            card_paper_cream: Color32::from_rgb(0x2A, 0x2C, 0x31), // "dark card" stock
            card_plain_bg: Color32::from_rgb(0x26, 0x28, 0x2C),
            card_border: Color32::from_rgb(0x8E, 0x93, 0x9E),
            card_shadow: Color32::from_black_alpha(90),
            text: Color32::from_rgb(0xE8, 0xE6, 0xE1),
            text_weak: Color32::from_rgb(0xAG, 0xA4, 0x9C),        // FIX: invalid hex — pick ~#A6A49C and verify contrast
            accent: Color32::from_rgb(0x7F, 0xB3, 0xF0),
            tag_pill_bg: Color32::from_rgb(0x22, 0x33, 0x48),
            code_bg: Color32::from_rgb(0x1A, 0x1B, 0x1E),
            rule_red: Color32::from_rgb(0x6E, 0x4A, 0x4A),
            rule_blue: Color32::from_rgb(0x3E, 0x4A, 0x5C),
            rule_ink: Color32::from_rgb(0x55, 0x5E, 0x6E),         // faint luminous rules
            focus_ring: Color32::from_rgb(0x7F, 0xB3, 0xF0),
            error_text: Color32::from_rgb(0xF0, 0x9A, 0x9A),
            divider_tab_bg: Color32::from_rgb(0x37, 0x33, 0x28),
            footer_bg: Color32::from_rgb(0x30, 0x2E, 0x28),
        }
    }
}

/// THE span→format mapping (editor + card faces both use this).
pub fn text_format(style: SpanStyle, th: &Theme) -> TextFormat {
    let prop = |size: f32| FontId::new(size, FontFamily::Name("inter".into()));
    let named = |fam: &str, size: f32| FontId::new(size, FontFamily::Name(fam.into()));
    let mono = FontId::new(MONO_SIZE, FontFamily::Name("jbmono".into()));
    let mut f = TextFormat::simple(prop(BODY_SIZE), th.text);
    match style {
        SpanStyle::Text => {}
        SpanStyle::Heading(n) => f.font_id = named("inter-bold", heading_size(n)),
        SpanStyle::HeadingMarker => {
            f.font_id = named("inter-bold", heading_size(1));
            f.color = th.text_weak;
        }
        SpanStyle::Bold => f.font_id = named("inter-bold", BODY_SIZE),
        SpanStyle::Italic => f.font_id = named("inter-italic", BODY_SIZE),
        SpanStyle::BoldItalic => f.font_id = named("inter-bold-italic", BODY_SIZE),
        SpanStyle::Strike => f.strikethrough = egui::Stroke::new(1.0, th.text),
        SpanStyle::InlineCode | SpanStyle::CodeBlock => { f.font_id = mono; f.background = th.code_bg; }
        SpanStyle::CodeFenceMarker => { f.font_id = mono; f.color = th.text_weak; }
        SpanStyle::ListMarker => f.color = th.text_weak,
        SpanStyle::TaskBoxUnchecked | SpanStyle::TaskBoxChecked => f.color = th.text_weak,
        SpanStyle::QuoteMarker => f.color = th.text_weak,
        SpanStyle::Quote => f.font_id = named("inter-italic", BODY_SIZE),
        SpanStyle::WikiLink { resolved: true } => {
            f.color = th.accent;
            f.underline = egui::Stroke::new(1.0, th.accent);
        }
        SpanStyle::WikiLink { resolved: false } => {
            // egui has no dashed underline: unresolved = weak color + underline.
            f.color = th.text_weak;
            f.underline = egui::Stroke::new(1.0, th.text_weak);
        }
        SpanStyle::Tag => { f.color = th.accent; f.background = th.tag_pill_bg; }
        SpanStyle::Url | SpanStyle::MdLinkUrl => {
            f.color = th.accent;
            f.underline = egui::Stroke::new(1.0, th.accent);
        }
        SpanStyle::MdLinkText => f.color = th.accent,
    }
    f
}
```

**The `0xAG` above is a deliberate marker** — tune every dark-theme value until the WCAG test passes; the light values were eyeballed and the test is the arbiter. Iterate: run test → read failing pair + ratio → adjust. Then wire it in:
- `editor.rs`: `layout_body` gains a `theme: &Theme` parameter; `format_for` is deleted in favor of `theme::text_format` (update the spike test call sites — they construct `Theme::light()`).
- `app.rs`: `JdUi` gains `pub theme: Theme` (init: `Theme::light()`; OS detection in Step 6) and `JdApp` installs fonts on first frame (`install_fonts` guarded by a `fonts_installed: bool`). In tests, `JdUi::ui` installs them the same lazy way — kittest exercises the real path.

- [ ] **Step 5: Run tests until green** — `cargo test -p jd-app` (WCAG + fonts + spike tests still pass).

- [ ] **Step 6: OS theme detection**

In `JdUi::ui`: `let dark = ui.ctx().style().visuals.dark_mode;` — recompute `self.theme` when it changes (eframe follows the OS by default). One-liner + manual sanity; kittest default is light.

- [ ] **Step 7: Full check + commit**

Run: `cargo test -p jd-app && cargo clippy --workspace --all-targets -- -D warnings && cargo fmt --check`

```bash
git add -A
git commit -m "feat(app): theme - bundled Inter/JetBrains Mono, WCAG-checked palettes, span formats"
```

---

### Task 5: state.rs — UiState, BodyCache, geom conversions

**Files:**
- Create: `crates/jd-app/src/state.rs`
- Modify: `crates/jd-app/src/lib.rs` (`pub mod state;`), `crates/jd-app/src/app.rs` (JdUi holds UiState; drain_events populates it)

**Interfaces:**
- Consumes: `VaultEvent`, `SessionState`, `Journal`, `jd_core::geom::Vec2`.
- Produces (for Tasks 7–12):

```rust
pub fn to_egui(v: jd_core::geom::Vec2) -> egui::Vec2;   // and to_pos2
pub fn from_egui(v: egui::Vec2) -> jd_core::geom::Vec2;

pub struct CachedBody { pub text: String }              // lex caching stays in LineCache (editor) / face cache (card)
pub struct BodyCache { /* map: HashMap<NoteId, CachedBody>, pending: HashSet<NoteId> */ }
impl BodyCache {
    /// Returns the cached body, or None after enqueueing ONE ReadBody request.
    pub fn get_or_request(&mut self, id: NoteId, commands: &Sender<VaultCommand>) -> Option<&CachedBody>;
    pub fn insert(&mut self, id: NoteId, content: String);   // clears pending
    pub fn invalidate(&mut self, id: NoteId);
    pub fn invalidate_all(&mut self);
}

pub struct UiState {
    pub session: SessionState,
    pub session_dirty_at: Option<std::time::Instant>,   // debounce anchor (1 s)
    pub focus: Option<NoteId>,
    pub editor: Option<crate::editor::EditorState>,     // Task 10 defines it; use a placeholder `pub struct EditorState;` in editor.rs NOW so state.rs compiles
    pub bodies: BodyCache,
    pub journal: jd_core::journal::Journal,
    pub scan_done: bool,
    pub last_error: Option<String>,
    pub pending_create: Option<PendingCreate>,          // Ctrl+N in flight
}
pub struct PendingCreate { pub at: jd_core::geom::Vec2, pub open_editor: bool }
```

- [ ] **Step 1: Failing unit tests** (in `state.rs` `#[cfg(test)]`): `get_or_request` on a missing id sends exactly one `ReadBody` across two calls (assert via the channel's receiver end and a second call while pending); `insert` then `get_or_request` returns the body and sends nothing; `invalidate` causes a re-request. Build the channel pair directly with `std::sync::mpsc::channel()` — no worker needed.

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use jd_core::worker::VaultCommand;
    use std::sync::mpsc;

    fn nid(n: u8) -> jd_core::id::NoteId {
        format!("01ARZ3NDEKTSV4RRFFQ69G5F{n:02}").parse().unwrap()
    }

    #[test]
    fn body_cache_requests_once_and_caches() {
        let (tx, rx) = mpsc::channel();
        let mut c = BodyCache::default();
        assert!(c.get_or_request(nid(1), &tx).is_none());
        assert!(c.get_or_request(nid(1), &tx).is_none()); // pending: no duplicate
        let sent: Vec<_> = rx.try_iter().collect();
        assert_eq!(sent.len(), 1);
        assert!(matches!(sent[0], VaultCommand::ReadBody { id } if id == nid(1)));
        c.insert(nid(1), "hello".into());
        assert_eq!(c.get_or_request(nid(1), &tx).unwrap().text, "hello");
        assert!(rx.try_iter().next().is_none());
        c.invalidate(nid(1));
        assert!(c.get_or_request(nid(1), &tx).is_none());
        assert_eq!(rx.try_iter().count(), 1);
    }
}
```

- [ ] **Step 2: Run to verify failure** — `cargo test -p jd-app --lib state` → FAIL.

- [ ] **Step 3: Implement** `state.rs` exactly per the Produces block (BodyCache backed by `HashMap` + `HashSet`; conversions are 4 one-liners). Move `scan_done`/`last_error` from `JdUi` fields into `UiState`; `JdUi` gains `pub state: UiState` and `drain_events` updates it:
  - `Body { id, content }` → `bodies.insert`
  - `External { changed, removed }` → invalidate each id (and removed ids drop from every desk's `cards` — session apply NOT journaled; external reality isn't undoable)
  - `OpDone { result, source: User }` → `journal.push(JournalEntry { label: result.label, inverse: result.inverse, context })` — check `OpResult`/`JournalEntry`/`OpContext` field shapes in jd-core and adapt; invalidate affected bodies (`result.created` and any id the op names).
  - `OpFailed { label, message }` → `last_error = Some(...)`.
  - `ScanComplete` → `scan_done = true`; `bodies.invalidate_all()`.

`SessionState` needs a default desk on first run: after `ScanComplete`, if `state.session.desks.is_empty()`, apply `SessionOp::CreateDesk { name: "Desk".into() }` (check the real variant shape in session.rs) and set `current_surface` to it. Session load itself happens in `JdUi::new` via `SessionState::load(&vault)` — **note**: `worker::start` consumes the `Vault`; load the session BEFORE calling `start` (open the Vault, `SessionState::load(&v)`, then hand `v` to `start`).

- [ ] **Step 4: Run tests** — `cargo test -p jd-app` → PASS (including Task 1 smoke, adapted to `state.scan_done`).

- [ ] **Step 5: Full check + commit**

```bash
git add -A
git commit -m "feat(app): UiState, BodyCache with single-request discipline, event wiring"
```

---

### Task 6: card/shape.rs — the geometric visual language

**Files:**
- Create: `crates/jd-app/src/card/mod.rs` (just `pub mod shape;` this task), `crates/jd-app/src/card/shape.rs`
- Modify: `crates/jd-app/src/lib.rs` (`pub mod card;`)

**Interfaces:**
- Consumes: `jd_core::note::{Status, Kind}`, `NoteId`, `theme::Theme`.
- Produces (consumed by Task 7 CardWidget and Task 8 desk):

```rust
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CardShape { Scrap, IndexCard, Literature, Divider }
pub fn shape_for(status: Status, kind: Kind) -> CardShape;
// fleeting → Scrap (regardless of kind); else literature → Literature,
// structure → Divider, note → IndexCard. Check the real Kind variant names in note.rs.

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CardStyle { Paper, Plain }          // settings arrive WP6; WP2 defaults Paper
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum RuledLines { None, Natural, Ink }   // index-card face decoration

pub fn card_size(shape: CardShape) -> egui::Vec2;
// Scrap 240×130 · IndexCard 300×200 (3×5) · Literature 300×224 (footer 24) · Divider 300×208 (tab 26 above body)
pub const DIVIDER_TAB: egui::Vec2 = egui::vec2(96.0, 26.0);
pub const FOOTER_H: f32 = 24.0;

/// Paper-style scrap outline: subtly irregular top edge, deterministic per
/// note (seeded from the NoteId bytes — same scrap, same tear, every frame,
/// every platform). Plain style returns a plain rounded rect path.
pub fn outline(shape: CardShape, style: CardStyle, rect: egui::Rect, id: NoteId) -> Vec<egui::Pos2>;

/// Ruled lines for an IndexCard/Literature face under Paper style.
/// Natural: red header rule at RULE_TOP_OFFSET-6, faint blue rules every
/// RULE_SPACING below RULE_TOP_OFFSET. Ink: rule_ink color, no red header.
/// Pure geometry: returns (y, color) pairs in card-local space.
pub fn rules(lines: RuledLines, height: f32, th: &Theme) -> Vec<(f32, egui::Color32)>;
```

- [ ] **Step 1: Failing unit tests** (`#[cfg(test)]` in shape.rs):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use eframe::egui;

    fn nid(n: u8) -> jd_core::id::NoteId {
        format!("01ARZ3NDEKTSV4RRFFQ69G5F{n:02}").parse().unwrap()
    }

    #[test]
    fn scrap_is_wider_than_tall_and_card_is_3x5() {
        let s = card_size(CardShape::Scrap);
        assert!(s.x / s.y > 1.5, "scrap reads as a torn strip");
        let c = card_size(CardShape::IndexCard);
        assert!((c.x / c.y - 1.5).abs() < 0.01, "3x5 proportions");
    }

    #[test]
    fn torn_edge_is_deterministic_and_paper_only() {
        let rect = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), card_size(CardShape::Scrap));
        let a = outline(CardShape::Scrap, CardStyle::Paper, rect, nid(1));
        let b = outline(CardShape::Scrap, CardStyle::Paper, rect, nid(1));
        assert_eq!(a, b, "same id, same tear");
        let c = outline(CardShape::Scrap, CardStyle::Paper, rect, nid(2));
        assert_ne!(a, c, "different id, different tear");
        let plain = outline(CardShape::Scrap, CardStyle::Plain, rect, nid(1));
        assert!(plain.len() <= 8, "plain = rounded rect, no tear vertices");
        // Semantic shape survives Plain: still scrap-sized (caller controls rect; the
        // outline never exceeds it).
        for p in &plain { assert!(rect.expand(0.1).contains(*p)); }
        for p in &a { assert!(rect.expand(0.1).contains(*p), "tear stays inside the rect"); }
    }

    #[test]
    fn natural_rules_have_red_header_then_blue() {
        let th = crate::theme::Theme::light();
        let r = rules(RuledLines::Natural, 200.0, &th);
        assert!(r.len() >= 6);
        assert_eq!(r[0].1, th.rule_red);
        assert!(r[1..].iter().all(|(_, c)| *c == th.rule_blue));
        assert!(r.windows(2).all(|w| w[1].0 > w[0].0), "descending down the card");
        assert!(r.last().unwrap().0 < 200.0);
        assert!(rules(RuledLines::None, 200.0, &th).is_empty());
        assert!(rules(RuledLines::Ink, 200.0, &th).iter().all(|(_, c)| *c == th.rule_ink));
    }
}
```

- [ ] **Step 2: Run to verify failure** — `cargo test -p jd-app --lib card` → FAIL.

- [ ] **Step 3: Implement.** Torn edge: seed a `jd_core::rng` xorshift (check its constructor — `Xorshift::new(seed)` or similar) from the first 8 bytes of the NoteId (`id.to_string()` bytes hashed via `DefaultHasher` is fine), then walk the top edge in ~14px steps jittering y by ±3px; other three edges straight with 4px corner rounding approximated by the polygon (Paper) — the polygon feeds `egui::Shape::convex_polygon`? No: tears are concave — use `egui::Shape::closed_line` for the stroke and a `Mesh`/`Path` fill; simplest robust approach: `egui::epaint::PathShape::closed_line` for the border plus `Shape::convex_polygon` is WRONG here — use `epaint::Shape::Path(PathShape { fill, .. })` (epaint fills non-convex paths acceptably for mild concavity; verify visually in the Task 7 snapshot — if fill artifacts appear, fall back to: filled rounded rect + jittered opaque "torn strip" polygon drawn over the top edge in desk_bg color, which is visually identical and always correct). Divider/Literature/IndexCard outlines are rounded rects (Divider's includes the tab as part of the polygon: up-left of the top edge).

- [ ] **Step 4: Run tests** — PASS. **Step 5:** `cargo clippy --workspace --all-targets -- -D warnings && cargo fmt --check` then:

```bash
git add -A
git commit -m "feat(app): card shape geometry - scrap tears, divider tabs, ruled lines"
```

---

### Task 7: CardWidget — faces, AccessKit node, snapshot matrix

**Files:**
- Modify: `crates/jd-app/src/card/mod.rs` (CardWidget)
- Create: `crates/jd-app/tests/card_faces.rs`

**Interfaces:**
- Consumes: shape.rs (Task 6), `theme::{Theme, text_format}`, `editor::layout_body` + `LineCache` (face body styling), `desk::card_a11y_label` (Task 3).
- Produces (for desk Task 8, Drawer/Map in WP4/5):

```rust
pub struct CardFace<'a> {
    pub id: NoteId,
    pub title: &'a str,          // "" for scraps
    pub body: Option<&'a str>,   // None → blank face (body not yet loaded)
    pub shape: CardShape,
    pub style: CardStyle,
    pub lines: RuledLines,
    pub source: Option<&'a str>, // literature footer text
    pub links: usize,
    pub tags: usize,
    pub focused: bool,
}
/// Renders at `rect` (desk-space already transformed by the caller), returns
/// the egui Response (click/drag sensing + AccessKit node with the spec label).
pub fn card_face(ui: &mut egui::Ui, rect: egui::Rect, face: &CardFace<'_>, th: &Theme, cache: &mut LineCache) -> egui::Response;
```

Rendering order: shadow (offset 3px) → outline fill (paper cream / plain bg; texture tint only under Paper) → rules (IndexCard/Literature + Paper only) → divider tab + title-on-tab / literature footer strip with `source` in `text_weak` → title galley (`inter-bold`, 17.0) → body galley via `layout_body` (readable size = BODY_SIZE; clipped to the rect with 10px margin; no scrolling on faces — cards are small by pedagogy) → focus ring (2px `focus_ring`) when `face.focused`. Task-list lines on faces render their checkbox glyphs from the lexer spans (`TaskBoxUnchecked/Checked` restyled as `☐`/`☑` via text_format — NOT clickable in WP2; face-click toggling is deferred to WP3 with the rest of card interaction polish — record as a deferred item).

The AccessKit node uses the Spike-B-proven pattern (whatever Step 5/6 of Task 3 landed on) with `card_a11y_label(...)`; `Sense::click_and_drag()`.

- [ ] **Step 1: Failing kittest + snapshot tests**

`crates/jd-app/tests/card_faces.rs`:

```rust
mod common;

use eframe::egui;
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use jd_app::card::shape::{CardShape, CardStyle, RuledLines};
use jd_app::card::{CardFace, card_face};
use jd_app::theme::Theme;

fn nid(n: u8) -> jd_core::id::NoteId {
    format!("01ARZ3NDEKTSV4RRFFQ69G5F{n:02}").parse().unwrap()
}

fn face_harness(face_owned: OwnedFace) -> Harness<'static> {
    Harness::builder()
        .with_size(egui::vec2(360.0, 280.0))
        .build_ui(move |ui| {
            jd_app::theme::install_fonts(ui.ctx());
            let th = Theme::light();
            let mut cache = jd_app::editor::LineCache::default();
            let rect = egui::Rect::from_min_size(egui::pos2(24.0, 24.0),
                jd_app::card::shape::card_size(face_owned.shape));
            card_face(ui, rect, &face_owned.borrow(), &th, &mut cache);
        })
}

// OwnedFace: a small owned mirror of CardFace so the closure can be 'static;
// implement in this test file (id, String title/body/source, the Copy fields,
// fn borrow(&self) -> CardFace<'_>).

/// The full legal matrix (architecture WP2: 4 shapes × 2 styles + 3 line
/// variants on the Paper index card).
#[test]
fn snapshot_card_face_matrix() {
    let combos: Vec<(&str, CardShape, CardStyle, RuledLines)> = vec![
        ("scrap_paper", CardShape::Scrap, CardStyle::Paper, RuledLines::None),
        ("scrap_plain", CardShape::Scrap, CardStyle::Plain, RuledLines::None),
        ("index_paper_none", CardShape::IndexCard, CardStyle::Paper, RuledLines::None),
        ("index_paper_natural", CardShape::IndexCard, CardStyle::Paper, RuledLines::Natural),
        ("index_paper_ink", CardShape::IndexCard, CardStyle::Paper, RuledLines::Ink),
        ("index_plain", CardShape::IndexCard, CardStyle::Plain, RuledLines::None),
        ("literature_paper", CardShape::Literature, CardStyle::Paper, RuledLines::Natural),
        ("literature_plain", CardShape::Literature, CardStyle::Plain, RuledLines::None),
        ("divider_paper", CardShape::Divider, CardStyle::Paper, RuledLines::None),
        ("divider_plain", CardShape::Divider, CardStyle::Plain, RuledLines::None),
        ("index_dark_ink", CardShape::IndexCard, CardStyle::Paper, RuledLines::Ink), // Theme::dark — see note
    ];
    for (name, shape, style, lines) in combos {
        let mut h = face_harness(OwnedFace::sample(shape, style, lines, name.contains("dark")));
        h.run_ok();
        h.snapshot(&format!("card_{name}"));
    }
}

#[test]
fn face_carries_the_spec_announcement() {
    let mut h = face_harness(OwnedFace::sample(CardShape::IndexCard, CardStyle::Paper, RuledLines::Natural, false));
    h.run_ok();
    h.get_by_label_contains("Card: 'Ideas want linking'");
}

#[test]
fn blank_face_while_body_loads_is_not_an_error() {
    let mut of = OwnedFace::sample(CardShape::IndexCard, CardStyle::Plain, RuledLines::None, false);
    of.body = None;
    let mut h = face_harness(of);
    h.run_ok();
    h.get_by_label_contains("Card: '"); // node exists even with no body
}
```

`OwnedFace::sample` uses title `"Ideas want linking"`, a body exercising the dialect (`"# Ideas want linking\nBody with **bold**, a [[Link]], and #tag\n- [ ] a task"`), `source: Some("Ahrens 2017")` for literature, links 3 / tags 2. Dark variant builds with `Theme::dark()` (thread a `dark: bool` through `face_harness`).

- [ ] **Step 2: Run to verify failure** — `cargo test -p jd-app --test card_faces` → FAIL (`card_face` missing).

- [ ] **Step 3: Implement `card_face`** per the rendering order above. Title for a Divider draws on the tab; scraps draw first body line as their visual first line (no separate title). Keep it ~150 lines; geometry from shape.rs, formats from theme.rs.

- [ ] **Step 4: Bless snapshots + run** — `UPDATE_SNAPSHOTS=force cargo test -p jd-app --test card_faces` then plain run → PASS. **Eyeball every PNG in `tests/snapshots/`** — this is the one task where the human-facing look gets set; report anything ugly (tear artifacts, rule collisions with text, unreadable footer) and fix before blessing. `git add` the goldens.

- [ ] **Step 5: Full check + commit** (`fmt` last):

```bash
git add -A
git commit -m "feat(app): CardWidget faces with full snapshot matrix and a11y labels"
```

**CI note for the controller:** first push after this task watches the 3-OS matrix; if macOS/Windows rasterize outside `kittest.toml` thresholds, raise per-platform thresholds (up to ~2.0) rather than per-test overrides; beyond that, gate snapshot tests to linux-only with `#[cfg(target_os = "linux")]` and record the decision. Interaction tests stay 3-OS regardless.

---

### Task 8: desk.rs — the pannable canvas

**Files:**
- Modify: `crates/jd-app/src/surfaces/desk.rs` (add the canvas below the Task 3 machinery)
- Create: `crates/jd-app/tests/desk_kittest.rs`
- Modify: `crates/jd-app/src/app.rs` (JdUi renders the desk as its central surface)

**Interfaces:**
- Consumes: Tasks 3–7 products, `UiState`, `SessionState`/`SessionOp`, `BodyCache`.
- Produces:

```rust
pub const ZOOM_MIN: f32 = 0.5;
pub const ZOOM_MAX: f32 = 2.0;

/// Desk-space → screen-space: screen = (world - viewport.center) * zoom + panel_center.
pub struct DeskCamera { pub center: egui::Vec2, pub zoom: f32 }   // mirrors jd_core Viewport
impl DeskCamera {
    pub fn to_screen(&self, panel: egui::Rect, world: egui::Pos2) -> egui::Pos2;
    pub fn to_world(&self, panel: egui::Rect, screen: egui::Pos2) -> egui::Pos2;
    pub fn zoom_to_fit(&mut self, cards: &[(NoteId, jd_core::geom::Vec2)], panel: egui::Rect);
}

pub enum DeskEvent { OpenCard(NoteId), SessionOp(jd_core::session::SessionOp), FocusChanged(Option<NoteId>) }

/// Render the active desk; returns events for app.rs to apply (desk itself
/// never touches SessionState directly — one mutation site, in app.rs).
pub fn desk_ui(ui: &mut egui::Ui, desk: &jd_core::session::Desk, state: &mut DeskUiDeps<'_>) -> Vec<DeskEvent>;
// DeskUiDeps bundles: focus, bodies (&mut BodyCache), commands sender, theme,
// line cache, index read guard data (title/links/tags per placed id, prefetched
// by app.rs into a Vec<FaceMeta> to keep the lock out of the render pass),
// drag: &mut Option<DragState { id, grab_offset }>.
```

Behaviors (each has a test in Step 1):
1. **Pan**: drag on empty background moves `viewport.center` (inverse of drag delta / zoom); plain scroll pans vertically, Shift+scroll horizontally; middle-drag pans. Viewport changes mark `session_dirty_at` (debounced save) but are NOT journaled (camera movement is not an undoable act).
2. **Zoom**: Ctrl+scroll multiplies zoom by `1.0015^scroll_delta` clamped to [0.5, 2.0], anchored at the pointer (world point under cursor stays put). A "Fit" button in the status line calls `zoom_to_fit` (also the fallback when a restored viewport contains no cards).
3. **Card drag**: click-drag a card updates its position live (screen delta / zoom); on release emit `SessionOp::Move { desk, id, from, to }` (from = drag start) → app.rs applies + journals it. A sub-4px total drag is a click, not a move (no journal spam).
4. **Click focuses; Enter opens** (`DeskEvent::OpenCard`); double-click opens too. Arrow keys use Task 3 `next_focus` — only when the editor is closed.
5. **Backspace puts away**: emits `SessionOp::PutAway { desk, id, was_at }` for the focused card (focus moves to next card in reading order).
6. **Culling**: cards whose screen rect misses `panel.expand(100.0)` are neither rendered nor given AccessKit nodes; their reading-order slots remain (focus can land on a culled card → Task 9's app loop pans the viewport to reveal it — implement `reveal(id)`: if focused card off-viewport, center on it).
7. **Faces**: body text from `bodies.get_or_request` (blank face on None — 1-frame gap by design).

- [ ] **Step 1: Failing kittest tests**

`crates/jd-app/tests/desk_kittest.rs` — build the full `JdUi` on a temp vault; create notes by driving the real worker (send `VaultCommand::Op { op: VaultOp::Create { .. }, source: User }`, pump until `OpDone`, collect created ids), then place them via the app's `place_card` (Task 9 exposes it — for THIS task, tests may apply `SessionOp::Place` directly to `ui.state.session` before the first frame):

First the shared helper (top of `desk_kittest.rs`):

```rust
mod common;

use eframe::egui;
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use jd_app::app::JdUi;
use jd_core::command::{Dest, OpSource, VaultOp};
use jd_core::geom::Vec2;
use jd_core::id::NoteId;
use jd_core::session::SessionOp;
use jd_core::worker::{VaultCommand, VaultEvent};

/// n notes titled "Card 1".."Card n" (2-line bodies), created through the real
/// worker, placed on the current desk at (i*350, (i/3)*250).
fn app_with_cards(n: usize) -> (common::TempDir, Harness<'static, JdUi>, Vec<NoteId>) {
    let vault = common::temp_vault();
    let mut app = JdUi::new(vault.path()).expect("JdUi::new");
    let mut ids = Vec::new();
    for i in 1..=n {
        // Build the NewNote seed per jd-core's note.rs (title, body, permanent note).
        let seed = common::new_note(&format!("Card {i}"), "# Card {i}\nsome body text");
        app.vault
            .commands
            .send(VaultCommand::Op { op: VaultOp::Create { seed, dest: Dest::Notes }, source: OpSource::User })
            .unwrap();
        // Collect the created id synchronously off the event channel.
        loop {
            match app.vault.events.recv_timeout(std::time::Duration::from_secs(5)).expect("OpDone") {
                VaultEvent::OpDone { result, .. } => {
                    ids.push(result.created.expect("Create yields an id"));
                    break;
                }
                _ => continue,
            }
        }
    }
    let mut h = Harness::builder()
        .with_size(egui::vec2(1200.0, 800.0))
        .build_ui_state(|ui, app: &mut JdUi| app.ui(ui), app);
    common::pump(&mut h, &mut |a: &JdUi| a.state.scan_done && !a.state.session.desks.is_empty(), 200, "scan + default desk");
    let desk_id = h.state().state.session.desks[0].id;
    // Direct placement pre-Task-9 (place_card replaces this once it exists).
    for (i, id) in ids.iter().enumerate() {
        let pos = Vec2 { x: (i as f32) * 350.0, y: ((i / 3) as f32) * 250.0 };
        let _ = h.state_mut().state.session.apply(&SessionOp::Place { desk: desk_id, id: *id, pos });
    }
    h.run_ok();
    (vault, h, ids)
}

/// Screen position of a card's center, computed via the camera the same way
/// the desk draws it (world → screen through DeskCamera::to_screen).
fn card_center_on_screen(h: &Harness<'_, JdUi>, id: NoteId) -> egui::Pos2 { /* use jd_app::surfaces::desk::DeskCamera with the current desk viewport and the harness's panel rect; ~8 lines */ }

#[test]
fn drag_moves_a_card_and_survives_in_session_state() {
    let (_v, mut h, ids) = app_with_cards(2);
    let from = card_center_on_screen(&h, ids[0]);
    let to = from + egui::vec2(200.0, 40.0);
    h.drag_at(from);
    h.run_ok();
    h.hover_at(to); // kittest drags are drag_at → (motion) → drop_at; verify its exact motion API
    h.run_ok();
    h.drop_at(to);
    h.run_ok();
    let desk = &h.state().state.session.desks[0];
    let placed = desk.cards.iter().find(|c| c.id == ids[0]).expect("still on desk");
    assert!((placed.pos.x - 200.0).abs() < 8.0 && (placed.pos.y - 40.0).abs() < 8.0,
        "world delta ≈ screen delta at zoom 1.0, got {:?}", placed.pos);
    assert_eq!(h.state().state.journal.undo_label(), Some("Move card"));
}

#[test]
fn pan_and_zoom_change_the_camera_and_clamp() {
    let (_v, mut h, _ids) = app_with_cards(1);
    let before = h.state().state.session.desks[0].viewport;
    h.hover_at(egui::pos2(600.0, 400.0));
    h.event(egui::Event::MouseWheel {
        unit: egui::MouseWheelUnit::Point,
        delta: egui::vec2(0.0, -120.0),
        modifiers: egui::Modifiers::NONE,
    });
    h.run_ok();
    assert!(h.state().state.session.desks[0].viewport.center.y != before.center.y, "scroll pans");
    for _ in 0..200 {
        h.event(egui::Event::MouseWheel {
            unit: egui::MouseWheelUnit::Point,
            delta: egui::vec2(0.0, 120.0),
            modifiers: egui::Modifiers::COMMAND,
        });
    }
    h.run_ok();
    let z = h.state().state.session.desks[0].viewport.zoom;
    assert!((z - 2.0).abs() < 1e-3, "zoom clamps at ZOOM_MAX, got {z}");
}

#[test]
fn offscreen_cards_are_culled_from_the_accesskit_tree() {
    let (_v, mut h, ids) = app_with_cards(1);
    let desk_id = h.state().state.session.desks[0].id;
    let far = "01ARZ3NDEKTSV4RRFFQ69G5F99".parse().unwrap(); // fabricate? NO — create a real second note via the worker, then:
    let _ = (desk_id, far, &ids); // place the real second card at (100_000.0, 0.0)
    h.run_ok();
    assert!(h.query_by_label_contains("Card: 'Far card'").is_none(), "culled card has no node");
    h.get_by_label_contains("Card: 'Card 1'");
    // zoom_to_fit brings it into view → node appears (drive the status-line Fit button by label).
    h.get_by_label("Fit").click();
    h.run_ok();
    assert!(h.query_by_label_contains("Card: 'Far card'").is_some());
}

#[test]
fn enter_opens_focused_card() {
    let (_v, mut h, ids) = app_with_cards(2);
    h.key_press(egui::Key::ArrowRight);
    h.run_ok();
    assert_eq!(h.state().state.focus, Some(ids[0]));
    h.key_press(egui::Key::Enter);
    common::pump(&mut h, &mut |a: &JdUi| a.state.session.open_card == Some(ids[0]), 100, "open card");
    // Task 10 upgrades this to assert the editor widget exists.
}

#[test]
fn backspace_puts_away_not_deletes() {
    let (_v, mut h, ids) = app_with_cards(2);
    h.key_press(egui::Key::ArrowRight);
    h.run_ok();
    h.key_press(egui::Key::Backspace);
    h.run_ok();
    let desk = &h.state().state.session.desks[0];
    assert!(!desk.cards.iter().any(|c| c.id == ids[0]), "off the desk");
    assert!(h.state().vault.index.read().unwrap().meta(ids[0]).is_some(), "note still exists");
    assert_eq!(h.state().state.journal.undo_label(), Some("Put card away"));
}
```

Fill the two `/* ... */` helper bodies and the `offscreen` test's second-note creation with real code (the patterns are all present in the helper above). `common::new_note` is a tiny helper you add to `tests/common/mod.rs` wrapping jd-core's `NewNote` construction — copy field names from `note.rs`. Harness state-access method names (`state()`, `state_mut()`, drag/motion API, `query_by_label_contains`) carry the usual verify-against-0.35 latitude; assertions do not.

- [ ] **Step 2: Run to verify failure.** — `cargo test -p jd-app --test desk_kittest` → FAIL.

- [ ] **Step 3: Implement `desk_ui` + camera + wire into `JdUi::ui`** (central panel renders current desk; status line gains the Fit button and zoom %). Keep the desk render pass allocation-light: prefetch `FaceMeta { title, first_line, links, tags, is_scrap, source }` for placed cards in app.rs under ONE index read lock per frame.

- [ ] **Step 4: Iterate to green, then full check + commit**

```bash
git add -A
git commit -m "feat(app): pannable desk - camera, drag, cull, focus, put-away"
```

---

### Task 9: app.rs — frame loop order, session persistence, Ctrl+N

**Files:**
- Modify: `crates/jd-app/src/app.rs`, `crates/jd-app/src/state.rs`
- Modify: `crates/jd-app/tests/desk_kittest.rs` (add scenarios)

**Interfaces:**
- Consumes: everything so far.
- Produces: `JdUi::place_card(&mut self, desk: DeskId, id: NoteId, pos: geom::Vec2)` (journaled Place — WP4's palette and WP3's inbox call this), the definitive frame-loop order in `JdUi::ui`, session debounce, `Ctrl+N` flow.

Frame-loop order in `JdUi::ui` (architecture §3, WP2 subset — pinned as code comments numbered 1–5):
1. `drain_events()` (all pending; already built — extend: `OpDone` with `result.created = Some(id)` while `pending_create` is set → `place_card(current_desk, id, pending.at)` + open editor if `pending.open_editor` (editor opening = set `session.open_card`; Task 10 renders it), clear `pending_create`).
2. (IPC — WP7; skip.)
3. Global shortcut dispatch: if editor open → only editor keys (Task 10); else `Ctrl+N` → send `Create { seed: NewNote for an empty fleeting scrap, dest: Dest::Inbox }` + set `pending_create { at: camera.to_world(pointer or panel center), open_editor: true }`. Check `NewNote`'s real fields in note.rs (title/body/kind/status seeds) — a scrap is untitled, empty body, `Status::Fleeting`.
4. Render: central desk (Task 8) + status line; editor overlay when open (Task 10).
5. Debounced saves: if `session_dirty_at` elapsed > 1 s → `session.save(&vault_ref)` and clear. **`worker::start` consumed the Vault** — so `JdUi::new` keeps a second lightweight `Vault` (call `Vault::open` twice; it's just paths — verify it does no exclusive locking, it doesn't) for session load/save. Save-on-drop too (`impl Drop for JdUi`): best-effort `let _ = self.state.session.save(..)`.

Session ops flow through ONE function: `fn apply_session(&mut self, op: SessionOp, journal: bool)` — applies via `SessionState::apply`, pushes the returned inverse to the journal when `journal` (wrap in the journal's entry type with a human label: "Move card", "Put card away", "Place card"; exact `JournalEntry`/`InverseAction` shapes come from jd-core — if `JournalEntry.inverse` only holds `VaultOp`, add a WP2-local `enum AppInverse { Vault(VaultOp), Session(SessionOp) }` journal in state.rs INSTEAD of forcing jd-core's Journal — check `journal.rs` first; the architecture pins "one InverseAction enum" — if jd-core's JournalEntry already accommodates SessionOp, use it; otherwise implement `AppInverse` here and note it for WP3's undo wiring), marks `session_dirty_at`.

- [ ] **Step 1: Failing tests** (append to desk_kittest.rs):

```rust
#[test]
fn ctrl_n_creates_a_scrap_on_the_desk_with_editor_open() {
    // Ctrl+N → pump until the card exists on the desk (pending_create consumed),
    // session.open_card is the new id, and the index shows a Fleeting note in inbox/.
}

#[test]
fn session_survives_restart_exactly() {
    // Build app, place 3 cards, pan+zoom, OPEN one card (open_card set), drop
    // the harness/JdUi (Drop saves), build a NEW JdUi on the same vault root,
    // pump scan; assert desks/positions/viewport/open_card round-trip exactly
    // (compare Desk fields; f32 exact — the session format's write/parse must
    // round-trip values like 120.5; if a float fails exactness, fix the
    // format's float printing in... nowhere — session.rs is jd-core and FROZEN;
    // pick test values that round-trip and note any imprecision for WP3).
}

#[test]
fn session_save_is_debounced_not_per_frame() {
    // Move a card, read session.jd mtime/content immediately (unchanged),
    // pump past 1s (std::thread::sleep(1100ms) + step), assert file updated.
    // NOTE: this is a real-time test; keep the single sleep, mark #[ignore]
    // if it flakes on CI and note it (the restart test already covers save).
}
```

The comment bodies pin the exact assertions — write them as real code with the Task 8 helpers (`app_with_cards`, `pump`); the assertions themselves have no latitude. For the restart test, dropping the harness drops `JdUi` (state-consuming: use `harness.into_state()` then `drop`).

- [ ] **Step 2: FAIL run. Step 3: implement per above. Step 4: green + full check + commit**

```bash
git add -A
git commit -m "feat(app): frame loop, journaled session ops, debounced persistence, Ctrl+N"
```

---

### Task 10: editor.rs — the floating editor window

**Files:**
- Modify: `crates/jd-app/src/editor.rs` (replace the Task 5 placeholder `EditorState`), `crates/jd-app/src/app.rs` (open/close wiring, key routing)
- Create: `crates/jd-app/tests/editor_kittest.rs`

**Interfaces:**
- Consumes: `layout_body`/`LineCache` (Task 2), theme, worker commands, `BodyCache`.
- Produces (WP3 hooks onto this for promotion/split):

```rust
pub struct EditorState {
    pub id: NoteId,
    pub buffer: String,
    pub dirty: bool,
    pub last_edit: Option<Instant>,        // autosave anchor (1 s) + recovery anchor (2 s)
    pub last_journaled: Option<Instant>,
    pub cache: LineCache,
    pub undo: crate::text_undo::TextUndo,  // Task 12 (placeholder struct until then)
}
impl EditorState { pub fn open(id: NoteId, body: String) -> EditorState }

/// Renders the floating modal editor over the desk. Returns close request.
pub enum EditorEvent { KeepOpen, CloseAndSave }
pub fn editor_ui(ui: &mut egui::Ui, ed: &mut EditorState, deps: &mut EditorDeps<'_>) -> EditorEvent;
// EditorDeps: theme, commands sender, index (for the resolve closure:
// |title| index.read().unwrap().resolve(title).is_some() — check Index's real
// resolve signature), reduced_motion: bool (unused in WP2 rendering; plumbed).
```

Mechanics:
- **Window**: `egui::Modal` (egui 0.35 has it) or `egui::Window` fixed+centered with a dimmed backdrop — pick Modal if it plays well with kittest focus; size 540×440 ("comfortably fits a healthy card"), card-like frame (paper fill, shadow, 8px rounding). One editor at a time (`Option<EditorState>` in UiState — already the case).
- **Open**: on `DeskEvent::OpenCard`, app.rs requests the body (`bodies.get_or_request`); the editor opens when the body arrives (`Body` event while `session.open_card == Some(id)` and editor is None → `EditorState::open`). `TextEdit` gets focus on open (`request_focus`).
- **The buffer is the BODY ONLY** (frontmatter never enters the editor — `ReadBody` must return body-sans-frontmatter; **verify** what `ReadBody` actually returns in worker.rs Task-1-style before building: if it returns the full file, split with `jd_core`'s doc/frontmatter API (`NoteDoc::parse(&content)` then use its body accessor) on receipt in app.rs, and reattach nothing on save — `SaveBody` op takes body-only by contract (check `command.rs` `SaveBody` semantics from WP1e tests). Get this right; it's the round-trip law's front line.)
- **Layouter**: `TextEdit::multiline(&mut ed.buffer).layouter(...)` using `layout_body` with the editor's own `LineCache` and the real resolve closure.
- **Esc / Ctrl+Enter**: `CloseAndSave` → if dirty send `SaveBody { id, content: buffer.clone() }`, clear recovery is worker-side, close (editor = None, `session.open_card = None`, focus returns to the card).
- **Autosave**: dirty && `last_edit` > 1 s ago → send `SaveBody`, `dirty = false` (keep editor open). **Recovery journal**: buffer changed && `last_journaled` > 2 s ago → `VaultCommand::JournalBuffer { id, content }`.
- **Dirty detection**: compare buffer to its state last frame via `response.changed()`.

- [ ] **Step 1: Failing kittest tests**

`crates/jd-app/tests/editor_kittest.rs` (full JdUi + temp vault, helper reuse from desk tests):

```rust
#[test]
fn enter_opens_editor_esc_saves_and_closes() {
    // create+place a card with body "hello world"; focus it; Enter;
    // pump until editor visible (get_by_role MultilineTextInput);
    // type " again"; Esc; pump until OpDone(SaveBody);
    // read the note file from disk (test may read disk directly — tests aren't
    // the UI thread): body ends with "hello world again".
    // Round-trip check: frontmatter untouched byte-for-byte.
}

#[test]
fn autosave_fires_after_a_quiet_second() {
    // open editor, type, DON'T close; sleep 1.1s + pump; disk shows the edit;
    // editor still open. (Real-time test — single sleep, acceptable.)
}

#[test]
fn editor_styles_headings_larger() {
    // open a card whose body starts with "# Big"; grab the TextEdit galley row
    // heights via the layouter (reuse Spike A's criterion helper) — heading row taller.
    // (This is the integration proof that theme fonts + layouter survived wiring.)
}

#[test]
fn ctrl_enter_closes_too() { /* same as Esc path; promotion lands WP3 */ }
```

(As in Tasks 8–9: comment bodies pin exact assertions — write real code with the shared helpers; assertions have no latitude.)

- [ ] **Step 2: FAIL run. Step 3: implement per mechanics above. Step 4: green.**

- [ ] **Step 5: Full check + commit**

```bash
git add -A
git commit -m "feat(app): floating editor - modal TextEdit, autosave, recovery journaling"
```

---

### Task 11: Editor behaviors — continuation, indent, autocomplete, URL paste

**Files:**
- Modify: `crates/jd-app/src/editor.rs`
- Modify: `crates/jd-app/tests/editor_kittest.rs`

**Interfaces:**
- Consumes: `Index::{fuzzy, resolve, all_tags}` (check exact names/signatures in `index/mod.rs` + `fuzzy.rs` — WP1c produced `fuzzy_match` and index-level query helpers).
- Produces: behavior only. Pure helpers (unit-testable without egui) live in editor.rs:

```rust
/// Given the line before the cursor, what prefix should a fresh Enter insert?
/// "- item" → Some("- ") · "3. x" → Some("4. ") · "- [ ] x"/"- [x] x" → Some("- [ ] ")
/// "> q" → Some("> ") · empty item ("- " alone) → None-and-remove (end the list:
/// returns EnterAction::EndList { strip_from: usize }).
pub enum EnterAction { Plain, Continue(String), EndList { strip_from: usize } }
pub fn enter_action(line_before_cursor: &str) -> EnterAction;

/// Tab/Shift+Tab on a list line: returns the new line (2-space indent unit).
pub fn indent_line(line: &str, outdent: bool) -> Option<String>;   // None if not a list line

/// Detect an open autocomplete context at the cursor: "[[par" → Link("par"),
/// "#ta" → Tag("ta") (only when '#' starts a word, not "a#b", not headings at col 0).
pub enum AcContext { None, Link { start: usize, query: String }, Tag { start: usize, query: String } }
pub fn ac_context(buffer: &str, cursor_byte: usize) -> AcContext;

pub fn is_probably_url(s: &str) -> bool;   // http:// or https:// prefix — that's the whole rule
```

Behavior wiring in `editor_ui` (each intercepts BEFORE TextEdit consumes the key, via `ui.input_mut(|i| i.consume_key(..))` — the established egui pattern for TextEdit key overrides; cursor position comes from `TextEdit::show(...)`'s `TextEditOutput::cursor_range`):
- **Enter**: apply `enter_action` (insert continuation prefix / strip the empty item). Plain Enter passes through.
- **Tab / Shift+Tab** on a list line: `indent_line`; non-list Tab passes through (TextEdit inserts \t — acceptable v1).
- **Autocomplete**: when `ac_context` is Link/Tag, an `egui::Popup` anchored at the cursor rect shows up to 8 candidates — Link: `index.fuzzy(query)` over titles, plus a final row `Link as new card: '<query>'` when no exact match (inserts the text verbatim → unresolved link); Tag: fuzzy over `all_tags`. Up/Down navigate, Enter/Tab accept (replaces `[[query` with `[[Title]]` — and closing `]]` handling: if the buffer already has `]]` ahead, don't double it), Esc dismisses (and does NOT close the editor — key routing: Esc goes to the popup first).
- **URL paste**: intercept paste events (`egui::Event::Paste`) in `editor_ui` before TextEdit: if clipboard text `is_probably_url` AND there's a nonempty selection → replace selection with `[<selection>](<url>)`. Bare-URL-into-empty-card guidance is WP6; do nothing special.
- **No smart quotes**: nothing transforms typed text, ever. The test asserts it stays `"straight"`.

- [ ] **Step 1: Failing unit tests for the pure helpers** — table-driven over the doc-comment cases above plus: numbered continuation increments correctly at `9.`→`10.`; task continuation always unchecked; `ac_context` at `"see [[Zettel"` cursor-at-end → Link query "Zettel"; `"# not a tag"` col-0 → None; `"word #ta"` → Tag "ta". Write ~20 rows.

- [ ] **Step 2: FAIL. Step 3: implement helpers. Step 4: green (helpers).**

- [ ] **Step 5: Failing kittest behavior tests** (append to editor_kittest.rs):

```rust
#[test] fn enter_continues_lists_and_empty_item_ends() { /* type "- a\n" → next line auto "- "; Enter again on the empty item → prefix removed, list ended */ }
#[test] fn link_autocomplete_inserts_a_resolved_link() { /* vault has "Target Note"; type "[[Tar", popup shows it (get_by_label), Enter → buffer contains "[[Target Note]]" */ }
#[test] fn typed_quotes_stay_ascii() { /* type "he said \"hi\"" → buffer identical, no U+201C anywhere */ }
#[test] fn url_paste_over_selection_makes_md_link() { /* select "docs", paste "https://example.com" via harness.event(egui::Event::Paste(..)) → buffer has "[docs](https://example.com)" */ }
```

(Comment bodies pin exact assertions — expand to real code with the shared helpers; assertions have no latitude.)

- [ ] **Step 6: FAIL → implement wiring → green. Step 7: full check + commit**

```bash
git add -A
git commit -m "feat(app): editor behaviors - continuation, indent, autocomplete, url paste"
```

---

### Task 12: text_undo.rs — per-card text undo

**Files:**
- Create: `crates/jd-app/src/text_undo.rs`
- Modify: `crates/jd-app/src/lib.rs`, `crates/jd-app/src/editor.rs` (feed edits; consume Ctrl+Z/Ctrl+Shift+Z/Ctrl+Y), `crates/jd-app/src/state.rs` (`text_undo: HashMap<NoteId, TextUndo>` — survives editor close/reopen within the session; `EditorState.undo` becomes a take/put from this map on open/close)
- Modify: `crates/jd-app/tests/editor_kittest.rs`

**Interfaces:**
- Produces:

```rust
pub struct TextUndo { /* undo: Vec<Snapshot>, redo: Vec<Snapshot>, group state */ }
pub struct Snapshot { pub text: String, pub cursor: usize }
impl TextUndo {
    pub fn new(initial: &str) -> TextUndo;
    /// Record buffer state after an edit. GROUPING: consecutive edits merge
    /// into one entry until (a) a word boundary is crossed (the edit inserts
    /// whitespace after non-whitespace), (b) 800 ms since the previous edit,
    /// (c) the edit is a deletion after inserts or vice versa, or (d) cursor
    /// jumped (|new - expected| > 1). Then the pending group commits.
    pub fn record(&mut self, text: &str, cursor: usize, now_ms: u64);
    pub fn undo(&mut self, current: &str) -> Option<Snapshot>;
    pub fn redo(&mut self) -> Option<Snapshot>;
}
```

egui's TextEdit has its own undoer — **disable/bypass it** by consuming Ctrl+Z/Ctrl+Shift+Z/Ctrl+Y in `editor_ui` before TextEdit sees them (consume_key), applying our stacks (set buffer + cursor via `TextEditState`/`CCursorRange` — check egui 0.35's `TextEditState::load/store` API for setting the cursor programmatically; this same API is what autocomplete-insert uses in Task 11, so the pattern already exists in the file). `now_ms` comes from `ui.input(|i| i.time)` seconds → ms — no `Instant` in the pure type (testability).

- [ ] **Step 1: Failing unit tests** — typing "hello world" as 11 single-char records (30 ms apart) then undo → one group boundary at the space: first undo → "hello ", second → "" (wait — grouping rule (a) commits the group at the whitespace: "hello" is one group, " world" the next; assert undo yields "hello " then "" — wait once more: record() stores post-edit states; define precisely in the test what the stack holds and make the impl match: after typing all of "hello world", undo₁ → "hello " (start of the current group), undo₂ → "" (initial), redo₁ → "hello ", redo₂ → "hello world"). Plus: 800ms-gap grouping; deletion starts a new group; redo cleared by a fresh edit; undo returns None at the bottom.
- [ ] **Step 2: FAIL. Step 3: implement (a compact ~120-line state machine). Step 4: green.**
- [ ] **Step 5: Kittest** (append to editor_kittest.rs):

```rust
#[test] fn text_undo_survives_close_and_reopen() { /* open, type "alpha beta", close (Esc), reopen the same card, Ctrl+Z → buffer "alpha " (the map preserved the stack) */ }
#[test] fn ctrl_z_in_editor_never_touches_the_app_journal() { /* journal len unchanged by editor Ctrl+Z */ }
```

(Comment bodies pin exact assertions — expand to real code; assertions have no latitude.)

- [ ] **Step 6: green → full check + commit**

```bash
git add -A
git commit -m "feat(app): per-card text undo with word-granularity grouping"
```

---

### Task 13: Integration scenarios, docs, ledger

**Files:**
- Modify: `crates/jd-app/tests/desk_kittest.rs` (final scenarios)
- Modify: `docs/superpowers/plans/2026-07-06-technical-architecture.md` (spike decisions §6.13/§6.14, WP3 handoffs)
- Modify: `README.md` if it exists / else skip

**Steps:**

- [ ] **Step 1: End-to-end kittest scenario** (one test, the M2 story): fresh vault → `Ctrl+N` → type a thought → Esc → card on desk → drag it somewhere → Enter reopens → edit with a `[[link]]` via autocomplete → Esc → restart (new JdUi) → everything exactly where it was, edits on disk, link resolved=false styled (assert via… the buffer content and index — visual style is covered by unit/snapshot layers).

- [ ] **Step 2: Architecture doc updates** (controller may do this instead — coordinate):
  - **Decision §6.13** — Spike A outcome: mixed-size layouter proven (or fallback taken), the one-LayoutJob-tiling-the-buffer mechanism, the "unresolved wikilink = weak + solid underline (no dashed in egui)" styling note, heading sizes 24/20/17 on 15 body.
  - **Decision §6.14** — Spike B outcome: the AccessKit node pattern that worked, no-wrap arrow policy, `BAND_HEIGHT = 120.0`.
  - **WP3 handoff list additions**: face checkbox click-to-toggle deferred; `Ctrl+Enter` promotion hook point (`EditorEvent`); `AppInverse`/journal shape actually used (Task 9 outcome); manual IME pass → WP8 checklist; anything the reviews flagged.

- [ ] **Step 3: Full-workspace check**: `cargo test --workspace && cargo clippy --workspace --all-targets -- -D warnings && cargo fmt --check` — including jd-core's suites (nothing in WP2 may touch jd-core; `git diff --stat main -- crates/jd-core` must be empty).

- [ ] **Step 4: Commit**

```bash
git add -A
git commit -m "test(app): WP2 end-to-end scenario; docs: spike decisions and WP3 handoffs"
```

---

## Verification (whole-WP definition of done)

- All 13 tasks' tests green locally AND on the 3-OS CI matrix (perf suites from WP1 still green — WP2 must not regress core).
- Snapshots: committed goldens for the full card matrix; thresholds in `kittest.toml` only (no per-test fudging without a recorded reason).
- Both spikes have recorded outcomes in the architecture doc.
- `crates/jd-core` untouched.
- The M2 milestone bar (spec §14): "One desk, real cards, styled-source editor in its floating window."
