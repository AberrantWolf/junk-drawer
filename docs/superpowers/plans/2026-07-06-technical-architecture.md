# Junk Drawer — Technical Architecture & Work Breakdown

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. This is the **master architecture document**; each work package (WP) below gets its own detailed implementation plan (`docs/superpowers/plans/YYYY-MM-DD-wpN-*.md`) generated from this document before execution. Interfaces defined here are contracts — a WP implementer may not change a signature another WP consumes without updating this document.

**Goal:** Build Junk Drawer v1 — a desktop Zettelkasten app in Rust/egui per the design doc at `docs/superpowers/specs/2026-07-03-junk-drawer-design.md` — as independently implementable work packages with pinned interfaces.

**Architecture:** A two-crate Cargo workspace: `jd-core` (headless: vault I/O, parsers, index, search, undo journal, settings — zero egui) and `jd-app` (eframe/egui binary: surfaces, cards, editor). Files on disk are the only persistent truth; a parallel startup scan builds an in-memory index behind `Arc<RwLock<Index>>`; a single vault-worker thread owns all writes; the UI thread never blocks.

**Tech Stack:** Rust stable (pin latest at M0), `eframe`/`egui` (glow), `notify`, `rfd`, `muda` (mac-only feature), `global-hotkey` + `tray-icon` (`resident` feature), `egui_kittest` (dev-dep). Everything else written in-house per Appendix B of the spec.

## Global Constraints

These apply to **every** work package; each WP's requirements implicitly include this list.

- **Crate names are `jd-core` and `jd-app`. Never name any crate `junk-*`.**
- **Dependency policy (spec §1, Appendix B):** no crates beyond the approved list above. Explicitly rejected: SQLite/`rusqlite`, `tantivy`, `serde`/`serde_yaml`, any YAML crate, `chrono`/`time`, `rand`/`getrandom`, `ulid`, `dirs`/`directories`, graph-layout crates, auto-updater frameworks, `wgpu` backend. Adding any dependency requires updating this document and spec Appendix B first.
- **The UI thread never blocks** — no filesystem, no locks held across frames, no thread joins. All waiting happens on channels; long operations get a labeled progress modal. A frozen frame or unlabeled spinner is a bug by definition.
- **All vault mutations go through the vault worker** (one background thread, serial execution). `jd-app` never writes a note file directly.
- **Saves are atomic:** temp file in the same directory, fsync, rename over the original.
- **Round-trip law:** parse → serialize is byte-identical for any file not deliberately changed. Unknown frontmatter keys, unknown markdown constructs, CRLF — all preserved. One sanctioned normalization (decision §6.9): a leading UTF-8 BOM is tolerated on parse and dropped on save.
- **Guidance voice (spec §11):** every user-facing string states the practice, names the action, assumes competence. No questions, offers, praise, or exclamation marks. Copy strings in the spec are verbatim requirements.
- **Accessibility is a requirement, not a v2 promise:** every widget lands in the AccessKit tree with deliberate semantics; no information or action exists only in spatial/visual form; reduced motion honored.
- **Performance budgets are failing tests** (WP1d ships them): cold scan of 20k synthetic notes < 1 s · incremental reindex of one file < 5 ms · palette query < 10 ms.
- **TDD throughout:** failing test first, minimal implementation, frequent commits.
- Keyboard shortcuts are **fixed** per spec Appendix A; `Cmd` replaces `Ctrl` on macOS (use `egui::Modifiers::COMMAND`, which maps correctly per-OS).

---

## 1. Workspace Layout

```
junk-drawer/
├── Cargo.toml                  # [workspace] members = ["crates/jd-core", "crates/jd-app"]
├── rust-toolchain.toml         # pinned stable
├── .github/workflows/ci.yml    # fmt + clippy -D warnings + test, matrix: ubuntu/macos/windows
├── .github/workflows/release.yml  # artifacts on tag (WP8)
├── crates/
│   ├── jd-core/
│   │   ├── Cargo.toml          # deps: notify only
│   │   └── src/
│   │       ├── lib.rs          # pub mod declarations, crate docs
│   │       ├── error.rs        # CoreError and per-module error enums
│   │       ├── time.rs         # Timestamp: RFC3339 UTC parse/format (ours)
│   │       ├── id.rs           # NoteId: ULID generation + parsing (ours)
│   │       ├── rng.rs          # Xorshift128+ (test generator + ULID entropy)
│   │       ├── tag.rs          # Tag: normalization, plural-insensitive matching
│   │       ├── note.rs         # Status, Kind, NoteMeta, NewNote
│   │       ├── frontmatter.rs  # FrontmatterDoc: fixed-schema parser, unknown-key preservation
│   │       ├── doc.rs          # NoteDoc = FrontmatterDoc + body; title/first-line/link/tag extraction
│   │       ├── lexer.rs        # markdown line lexer → StyledSpan (no egui types)
│   │       ├── index/
│   │       │   ├── mod.rs      # Index: notes, titles, adjacency, tag map; upsert/remove/queries
│   │       │   ├── search.rs   # inverted index, BM25, query parser, similarity
│   │       │   └── fuzzy.rs    # fzf-style scorer with acronym tier
│   │       ├── vault/
│   │       │   ├── mod.rs      # Vault: root, inbox/ & notes/ paths, open/scan
│   │       │   ├── io.rs       # atomic_save, filename sanitization, collision suffix
│   │       │   ├── scan.rs     # parallel startup scan (std::thread::scope)
│   │       │   ├── watcher.rs  # notify wrapper: 200 ms debounce → WatchEvent
│   │       │   ├── trash.rs    # .junkdrawer/trash/: toss, restore, purge-by-retention
│   │       │   └── recovery.rs # .junkdrawer/recovery/: journal unsaved buffers
│   │       ├── command.rs      # VaultOp + Inverse: every mutation as command with computed inverse
│   │       ├── journal.rs      # app undo stack: Vec<JournalEntry>, ~200 cap, labels
│   │       ├── worker.rs       # vault worker thread: VaultCommand in, VaultEvent out
│   │       ├── settings.rs     # Settings, key=value parser, unknown-key preservation
│   │       ├── paths.rs        # config-dir resolution per platform (ours)
│   │       ├── session.rs      # per-vault session state (.junkdrawer/session/): desks, positions
│   │       ├── guidance.rs     # banner rule list + card-margin detectors (pure functions)
│   │       ├── maplayout.rs    # force-directed layout: spring-repulsion + spatial grid
│   │       ├── ipc.rs          # single-instance socket protocol (UDS / named pipe)
│   │       └── geom.rs         # Vec2, Rect (core-side, no egui; From/Into impls live in jd-app)
│   └── jd-app/
│       ├── Cargo.toml          # deps: eframe, jd-core, rfd; features: resident, mac-menu
│       └── src/
│           ├── main.rs         # arg parsing (--capture, vault path), single-instance claim, eframe launch
│           ├── app.rs          # JdApp: eframe::App impl, event drain, surface routing, shortcut dispatch
│           ├── state.rs        # UiState: current surface, focus, editor state, body cache
│           ├── theme.rs        # palettes (light/dark), WCAG-checked constants, card tints
│           ├── card/
│           │   ├── mod.rs      # CardWidget: face rendering, focus, drag, AccessKit node
│           │   └── shape.rs    # scrap/index-card/literature/divider geometry, Paper/Plain
│           ├── surfaces/
│           │   ├── desk.rs     # pannable canvas, spatial focus order, ghost fan, edges
│           │   ├── inbox.rs    # self-arranging pile, oldest-first
│           │   ├── drawer.rs   # mini grid + filter chips
│           │   ├── map.rs      # map rendering over jd_core::maplayout
│           │   └── trash.rs
│           ├── editor.rs       # floating editor window: TextEdit + layouter, autocompletes, split
│           ├── text_undo.rs    # per-card text undo stacks, word-granularity grouping
│           ├── palette.rs      # Ctrl+K overlay: three strata
│           ├── rail.rs         # left rail: desks, Inbox, Drawer, Map, Trash
│           ├── banner.rs       # guidance banner (renders jd_core::guidance output)
│           ├── menus.rs        # egui menu bar; muda behind mac-menu feature
│           ├── shortcuts.rs    # fixed shortcut table + Ctrl+/ overlay
│           ├── settings_ui.rs  # the one settings dialog
│           ├── capture.rs      # capture popup window
│           └── platform.rs    # tray + global hotkey (resident feature), Wayland detection
├── docs/superpowers/specs/2026-07-03-junk-drawer-design.md
├── docs/superpowers/plans/     # this file + per-WP implementation plans
└── tests-data/golden/          # foreign-file corpus for round-trip tests (WP1a)
```

---

## 2. Core Types & Interfaces (the contracts)

Everything in this section is a **pinned interface**. Signatures use real Rust; implementers fill in bodies and private details. Error types may gain variants; public function signatures may not change without editing this document.

### 2.1 `time.rs` — Timestamp

We write RFC3339 ourselves (UTC only, `Z` suffix, second precision — matches the spec's frontmatter examples). ~80 lines.

```rust
/// Milliseconds since Unix epoch, always UTC.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct Timestamp(pub i64);

impl Timestamp {
    pub fn now() -> Self;                       // via std::time::SystemTime
    pub fn parse_rfc3339(s: &str) -> Result<Self, TimeError>;  // accepts fractional secs & offsets; normalizes to UTC
    pub fn to_rfc3339(&self) -> String;         // always "2026-07-03T10:22:00Z" form
    pub fn days_since(&self, other: Timestamp) -> f64;
}
```

Round-trip caveat: a frontmatter timestamp is only re-serialized when the field is deliberately changed (the `FrontmatterDoc` line-preservation mechanism below guarantees this), so accepting more formats than we emit is safe.

### 2.2 `id.rs` — NoteId (ULID)

```rust
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Debug)]
pub struct NoteId(pub [u8; 16]);   // 48-bit ms timestamp + 80-bit randomness

impl NoteId {
    /// Monotonic within the same millisecond (increment random part).
    pub fn generate(gen: &mut IdGen) -> Self;
    pub fn parse(s: &str) -> Result<Self, IdError>;   // 26-char Crockford base32
    pub fn short(&self) -> String;                    // first 8 chars, for filename collision suffix
}
impl fmt::Display for NoteId { /* 26-char Crockford base32, uppercase */ }

/// Holds RNG state + last timestamp for monotonicity. One per process, owned by the worker.
pub struct IdGen { /* Xorshift128+ seeded from SystemTime nanos ^ process id ^ stack address */ }
impl IdGen { pub fn new() -> Self; }
```

Entropy note: IDs are collision-resistant identifiers in a single-user app, not security tokens; the non-cryptographic seed is a documented, accepted trade (keeps `rand`/`getrandom` out of the tree).

### 2.3 `rng.rs` — Xorshift

```rust
pub struct Xorshift128(pub [u64; 2]);
impl Xorshift128 {
    pub fn new(seed: u64) -> Self;
    pub fn next_u64(&mut self) -> u64;
    pub fn gen_range(&mut self, range: Range<u64>) -> u64;
}
```

Shared by `IdGen` and the test-corpus generator (spec §13). ~40 lines.

### 2.4 `tag.rs` — Tag

```rust
/// Stored lowercase, original text discarded. Flat — no nesting.
#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Debug)]
pub struct Tag(String);

impl Tag {
    pub fn new(raw: &str) -> Option<Tag>;   // lowercase, trim '#', reject empty/whitespace
    pub fn as_str(&self) -> &str;
    /// Plural-insensitive: "book" matches "books" and vice versa (trailing-'s' fold; "es" for s/x/z/ch/sh stems).
    pub fn matches(&self, other: &Tag) -> bool;
    /// Canonical form used as the map key so "book" and "books" share one index bucket.
    pub fn fold_key(&self) -> String;
}
```

### 2.5 `note.rs` — Status, Kind, NoteMeta

```rust
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Status { Fleeting, Permanent }

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Kind { #[default] Note, Literature, Structure }

/// Everything the index holds about a note. Bodies are NOT here (spec §3).
#[derive(Clone, Debug)]
pub struct NoteMeta {
    pub id: NoteId,
    pub rel_path: PathBuf,           // relative to vault root, e.g. "notes/Egui tradeoffs.md"
    pub title: Option<String>,       // first `#` heading in body; None for untitled scraps
    pub first_line: String,          // first non-empty body line (scrap display; a11y announcement)
    pub status: Status,
    pub kind: Kind,
    pub source: Option<String>,
    pub created: Timestamp,
    pub modified: Timestamp,
    pub tags: BTreeSet<Tag>,         // union of frontmatter list + #inline-tags
    pub links_out: Vec<LinkRef>,
    pub word_count: u32,
}

#[derive(Clone, Debug, PartialEq)]
pub struct LinkRef {
    pub target: String,              // raw title text inside [[...]], pipe part excluded
    pub display: Option<String>,     // text after '|', if any
    pub span: Range<usize>,          // byte range in body, including brackets
}

/// Seed for creating a note (capture paths, palette "New scrap", split).
#[derive(Clone, Debug)]
pub struct NewNote {
    pub body: String,
    pub status: Status,              // Fleeting for all capture paths
    pub kind: Kind,
    pub source: Option<String>,
    pub tags: Vec<Tag>,
}
```

### 2.6 `frontmatter.rs` — FrontmatterDoc

The byte-identity mechanism: the parser keeps **every original line**; known keys get a parsed view; setters rewrite only the affected line (or append a new one before the closing `---`). Serialization concatenates lines. Files with no frontmatter get a block synthesized only when the app first mutates them.

```rust
pub struct FrontmatterDoc {
    // private: Vec<FmLine> where FmLine { raw: String, parsed: Option<(KnownKey, ..)> }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum KnownKey { Id, Created, Modified, Status, Kind, Source, Tags }

impl FrontmatterDoc {
    /// `input` starts at the first `---`. Returns doc + byte length consumed (through closing `---\n`).
    pub fn parse(input: &str) -> Result<(FrontmatterDoc, usize), FmError>;
    pub fn synthesize(id: NoteId, created: Timestamp, status: Status) -> FrontmatterDoc;

    pub fn id(&self) -> Option<NoteId>;
    pub fn created(&self) -> Option<Timestamp>;
    pub fn modified(&self) -> Option<Timestamp>;
    pub fn status(&self) -> Option<Status>;      // missing/unparseable → None; the CALLER (doc.rs to_meta)
                                                 // applies the path default: inbox/ → Fleeting, notes/ → Permanent
    pub fn kind(&self) -> Kind;                  // absent → Kind::Note
    pub fn source(&self) -> Option<String>;      // owned: value extraction unquotes, so a borrow can't work
    pub fn tags(&self) -> Vec<Tag>;

    pub fn set_status(&mut self, s: Status);
    pub fn set_kind(&mut self, k: Kind);         // Kind::Note removes the line (absent = note)
    pub fn set_source(&mut self, src: Option<&str>);
    pub fn set_modified(&mut self, t: Timestamp);
    pub fn set_tags(&mut self, tags: &[Tag]);    // canonical inline form: `tags: [a, b]`

    pub fn serialize(&self) -> String;           // byte-identical if no setter was called
}
```

Parsed value syntax (fixed YAML subset, documented as complete): `key: value` scalars, optional single/double quotes, inline lists `[a, b]`, block lists (`- item` continuation lines, parse-only — canonical write is inline). Everything else on unknown keys: preserved raw, never interpreted.

### 2.7 `doc.rs` — NoteDoc

The full-file view (parse on open/scan; index keeps only the extracted `NoteMeta`).

```rust
pub struct NoteDoc {
    pub fm: FrontmatterDoc,
    pub body: String,                // everything after frontmatter, byte-exact incl. line endings
}

impl NoteDoc {
    pub fn parse(input: &str) -> NoteDoc;        // no-frontmatter files: fm empty-marker, whole input = body
    pub fn serialize(&self) -> String;
    /// Extract meta. `id` comes from the caller (frontmatter id if present, else assigned at scan);
    /// `rel_path` supplies the status default (inbox/ → Fleeting, notes/ → Permanent)
    /// and file timestamps back-fill missing created/modified.
    pub fn to_meta(&self, id: NoteId, rel_path: &Path, fs_modified: Timestamp) -> NoteMeta;
}

// Body extraction helpers (pure, unit-tested; also used by the lexer tests):
pub fn extract_title(body: &str) -> Option<(String, Range<usize>)>;   // first `# ` heading + its byte span
pub fn extract_links(body: &str) -> Vec<LinkRef>;                     // skips code fences/inline code
pub fn extract_inline_tags(body: &str) -> Vec<Tag>;                   // #word, not inside links/code
pub fn word_count(body: &str) -> u32;                                 // unicode word segmentation (ours, ~60 lines)
```

### 2.8 `lexer.rs` — Markdown line lexer

Produces styling spans over raw source for the editor and card faces. No egui types — `jd-app` maps `SpanStyle → egui::TextFormat`.

```rust
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum LineState { #[default] Normal, InCodeFence }

#[derive(Clone, PartialEq, Debug)]
pub struct StyledSpan { pub range: Range<usize>, pub style: SpanStyle }

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SpanStyle {
    Text,
    Heading(u8),            // 1..=3; the text after the marker
    HeadingMarker,          // the '#'s + space, dimmed
    Bold, Italic, BoldItalic, Strike, InlineCode,
    CodeFenceMarker, CodeBlock,
    ListMarker,             // '-', '1.', including task-box brackets
    TaskBoxUnchecked, TaskBoxChecked,
    QuoteMarker, Quote,
    WikiLink { resolved: bool },   // resolved flag injected by caller via `resolve: &dyn Fn(&str) -> bool`
    Tag,
    Url, MdLinkText, MdLinkUrl,
}

/// Lex one line. `entry` carries fence state from the previous line.
/// Invariants (randomized-tested): spans cover the whole line, never overlap,
/// never split a UTF-8 boundary, and are in ascending order.
pub fn lex_line(line: &str, entry: LineState, resolve: &dyn Fn(&str) -> bool)
    -> (Vec<StyledSpan>, LineState);
```

The dialect is exactly spec §5's list; anything else lexes as `Text`. Pinned span
semantics (decision §6.11): emphasis/strike/inline-code spans cover delimiters +
content as ONE span, with no nested styling inside; `HeadingMarker` covers the
hashes + following space and the heading rest is ONE `Heading(n)` span (no inline
styling inside headings in v1 — links in headings are still indexed by doc.rs, just
not interactive in the editor; revisit post-v1); quote lines inline-lex their rest
with plain runs emitted as `Quote`; only fences carry state across lines.

### 2.9 `index/` — Index, search, fuzzy

```rust
pub struct Index { /* private maps per spec §3 */ }

pub type SharedIndex = std::sync::Arc<std::sync::RwLock<Index>>;

impl Index {
    pub fn new() -> Index;

    // Mutation (worker/indexer only):
    /// Upsert meta + body terms. Re-resolves links (title map may have changed).
    pub fn upsert(&mut self, meta: NoteMeta, body: &str);
    pub fn remove(&mut self, id: NoteId);

    // Lookups (UI thread, brief read locks):
    pub fn get(&self, id: NoteId) -> Option<&NoteMeta>;
    pub fn resolve_title(&self, title: &str) -> Option<NoteId>;      // case-insensitive
    pub fn backlinks(&self, id: NoteId) -> Vec<NoteId>;
    pub fn outlinks(&self, id: NoteId) -> Vec<(LinkRef, Option<NoteId>)>;  // None = unresolved
    pub fn notes_with_tag(&self, tag: &Tag) -> Vec<NoteId>;          // plural-insensitive via fold_key
    pub fn all_tags(&self) -> Vec<(Tag, usize)>;                     // with counts, for Drawer chips
    pub fn unlinked(&self) -> Vec<NoteId>;                           // no outlinks AND no backlinks
    pub fn fleeting(&self) -> Vec<NoteId>;                           // = the Inbox, oldest-first
    pub fn count(&self) -> usize;
    pub fn iter_meta(&self) -> impl Iterator<Item = &NoteMeta>;

    // Search (search.rs):
    pub fn query(&self, q: &Query, limit: usize) -> Vec<SearchHit>;  // BM25; must run < 10 ms at 20k notes
    /// Cosine over tf-idf vectors from existing postings. For ghost fans & post-promotion suggestions.
    pub fn similar(&self, id: NoteId, k: usize) -> Vec<(NoteId, f32)>;
}

// search.rs
pub struct Query { /* terms (AND), phrases, tags, negated terms; last term prefix-matched */ }
pub fn parse_query(input: &str) -> Query;     // the whole language: words, "phrases", #tag, -word

/// NOTE (decision §6.10): hits carry matched terms, NOT a prebuilt snippet — bodies are
/// not in the index (spec §3), and building snippets inside query() would force disk reads
/// on the UI thread. The app loads bodies for visible rows via the worker and calls
/// make_snippet.
pub struct SearchHit {
    pub id: NoteId,
    pub score: f32,
    pub matched_terms: Vec<String>,           // lowercased terms that hit, incl. prefix expansions
}
/// Pure snippet builder for the app layer: best window (~radius chars each side of the
/// densest match cluster), with byte-range highlights of term occurrences.
pub fn make_snippet(body: &str, terms: &[String], radius: usize) -> Snippet;
pub struct Snippet { pub text: String, pub highlights: Vec<Range<usize>> }

// fuzzy.rs — title stratum scorer (spec §7 tiers pinned as ranking-table tests)
pub struct FuzzyScore { pub tier: FuzzyTier, pub score: i32, pub matched: Vec<usize> /* char indices */ }
#[derive(PartialEq, Eq, PartialOrd, Ord, Clone, Copy, Debug)]
pub enum FuzzyTier { Exact, Prefix, Acronym, Subsequence }   // ordering = ranking
pub fn fuzzy_match(query: &str, candidate: &str) -> Option<FuzzyScore>;
```

Tokenization for the search index: unicode-segmented words (same segmentation module as `word_count`), lowercased, positions kept for phrase queries and snippets.

### 2.10 `vault/` — Vault, scan, io, watcher, trash, recovery

```rust
// mod.rs
pub struct Vault { /* root: PathBuf */ }
impl Vault {
    /// Creates root, inbox/, notes/, .junkdrawer/{trash,recovery,session}/ as needed.
    pub fn open(root: &Path) -> Result<Vault, VaultError>;
    pub fn root(&self) -> &Path;
    pub fn abs(&self, rel: &Path) -> PathBuf;
}

// scan.rs
pub struct ScanOutcome {
    pub metas: Vec<(NoteMeta, String /* body, consumed by index build then dropped */)>,
    pub quarantined: Vec<QuarantinedFile>,     // parse failures → Needs Attention, never fail the scan
}
pub struct QuarantinedFile { pub rel_path: PathBuf, pub error: String }
/// Parallel over available cores (std::thread::scope). Progress via callback for the labeled modal.
pub fn scan(vault: &Vault, progress: &(dyn Fn(usize, usize) + Sync)) -> Result<ScanOutcome, VaultError>;

// io.rs
/// temp file (same dir, dot-prefixed) → write → fsync → rename. The torture test kills between steps.
pub fn atomic_save(abs_path: &Path, content: &str) -> Result<(), IoError>;
pub fn sanitize_filename(title: &str) -> String;   // strip /\:*?"<>| + control chars, trim dots/spaces, cap 120 chars
/// "Title.md", or "Title (01J8ZQ4K).md" on collision with a *different* note's file.
pub fn filename_for(title: &str, id: NoteId, dir: &Path) -> PathBuf;

// watcher.rs
pub enum WatchEvent {
    Changed(PathBuf),         // rel paths; rename-swap editor saves normalized to Changed
    Removed(PathBuf),
    Renamed { from: PathBuf, to: PathBuf },
}
/// notify-based, ~200 ms debounce, coalesces bursts. Events for paths the worker itself
/// just wrote are suppressed via a write-ledger the worker maintains (path + expected mtime).
pub struct VaultWatcher { /* ... */ }
impl VaultWatcher {
    pub fn start(vault: &Vault, tx: mpsc::Sender<WatchEvent>) -> Result<VaultWatcher, WatchError>;
}

// trash.rs — .junkdrawer/trash/<ULID>.md + one "<ULID>.meta" line-file (original rel_path, deleted-at)
pub struct TrashEntry { pub id: NoteId, pub title_or_first_line: String, pub deleted: Timestamp }
pub fn trash_note(vault: &Vault, meta: &NoteMeta) -> Result<(), IoError>;
pub fn list_trash(vault: &Vault) -> Vec<TrashEntry>;
pub fn restore(vault: &Vault, id: NoteId) -> Result<PathBuf, IoError>;  // back to original dir; re-collision-checked
pub fn purge_older_than(vault: &Vault, days: Option<u32>) -> Result<usize, IoError>;  // None = manual only

// recovery.rs — crash safety for the autosave debounce window
pub fn journal_buffer(vault: &Vault, id: NoteId, content: &str) -> Result<(), IoError>;
pub fn clear_buffer(vault: &Vault, id: NoteId);
pub fn pending_recoveries(vault: &Vault) -> Vec<(NoteId, String)>;      // checked at startup
```

### 2.11 `command.rs` + `journal.rs` — Undo

```rust
/// Every structural mutation, expressed with its inverse computed at execution time.
pub enum VaultOp {
    Create { seed: NewNote, dest: Dest },                    // Dest::Inbox | Dest::Notes
    SaveBody { id: NoteId, content: String },
    RenameTitle { id: NoteId, new_title: String },           // renames file + rewrites [[links]] in referrers
    Promote { id: NoteId },                                  // status flip + inbox/ → notes/ move
    Demote { id: NoteId },
    SetKind { id: NoteId, kind: Kind },
    SetSource { id: NoteId, source: Option<String> },
    SetTags { id: NoteId, tags: Vec<Tag> },
    Toss { id: NoteId },                                     // → trash
    Delete { id: NoteId },                                   // → trash (UI confirmed already)
    Restore { id: NoteId },
    Split { id: NoteId, at_byte: usize },                    // → new note + [[link]] splice, spec §5
}

pub struct OpResult {
    pub inverse: Option<VaultOp>,        // None for non-undoable ops
    pub label: String,                   // "Toss scrap 'egui layouter idea'" — Edit-menu naming, spec §9
    pub created: Option<NoteId>,         // for Create/Split
}

// journal.rs — the app stack (session-long, in-memory, ~200 entries). Text undo lives in jd-app.
pub struct Journal { /* undo: Vec<JournalEntry>, redo: Vec<JournalEntry> */ }
pub struct JournalEntry { pub label: String, pub inverse: VaultOp, pub context: OpContext }
/// Where it happened, so undo can travel the view there (spec §9 legibility).
pub struct OpContext { pub desk: Option<DeskId>, pub note: Option<NoteId> }
impl Journal {
    pub fn push(&mut self, e: JournalEntry);                 // clears redo
    pub fn undo_label(&self) -> Option<&str>;
    pub fn redo_label(&self) -> Option<&str>;
    pub fn pop_undo(&mut self) -> Option<JournalEntry>;      // caller executes inverse via worker, pushes redo
    pub fn push_redo(&mut self, e: JournalEntry);
    pub fn pop_redo(&mut self) -> Option<JournalEntry>;
}
```

Desk placement/move/put-away are **session-state** operations (no file touched) — they get journal entries too, with inverses executed against `session.rs` state, not the worker. `JournalEntry.inverse` is therefore really `enum InverseAction { Vault(VaultOp), Session(SessionOp) }` — see 2.13.

### 2.12 `worker.rs` — The vault worker

```rust
pub enum VaultCommand {
    Op { op: VaultOp, source: OpSource },    // OpSource::User (→ journal) | OpSource::UndoRedo
    RescanAll,
    JournalBuffer { id: NoteId, content: String },   // recovery journaling, no index effect
    PurgeTrash { older_than_days: Option<u32> },
    Shutdown,
}

pub enum VaultEvent {
    OpDone { result: OpResult, source: OpSource },
    OpFailed { label: String, error: CoreError },        // human sentence + retry action decided in UI
    External { changed: Vec<NoteId>, removed: Vec<NoteId> },   // watcher-driven index updates
    Conflict { id: NoteId, conflict_copy: PathBuf },     // both kept, surfaced in Needs Attention
    ScanProgress { done: usize, total: usize },
    ScanComplete { quarantined: Vec<QuarantinedFile> },
}

pub struct VaultHandle {
    pub commands: mpsc::Sender<VaultCommand>,
    pub events: mpsc::Receiver<VaultEvent>,    // drained once per frame by jd-app
    pub index: SharedIndex,
}

/// Spawns worker + watcher + initial parallel scan. `wake` is called after posting any event
/// (jd-app passes egui::Context::request_repaint; core stays egui-free).
pub fn start(vault: Vault, wake: Box<dyn Fn() + Send + Sync>) -> Result<VaultHandle, VaultError>;
```

Serialization guarantees: one worker thread executes ops in arrival order — inverses are race-free by construction. The worker owns the `IdGen`, the write-ledger (watcher echo suppression), and applies index write-locks incrementally per op.

**Conflict rule (spec §2):** before writing, the worker compares the file's current mtime+size against the ledger; if the file changed externally since last read, write the app version to `Title (conflict YYYY-MM-DD HHMM).md` alongside, leave the external version in place, emit `Conflict`.

### 2.13 `session.rs` — Desks & session state

```rust
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct DeskId(pub NoteId);   // reuse ULID machinery; not a note

pub struct SessionState {
    pub desks: Vec<Desk>,                    // rail order
    pub current_surface: SurfaceId,          // Desk(DeskId) | Inbox | Drawer | Map | Trash
    pub open_card: Option<NoteId>,
}
pub struct Desk {
    pub id: DeskId,
    pub name: String,
    pub viewport: Viewport,                  // center: Vec2, zoom: f32
    pub cards: Vec<PlacedCard>,              // PlacedCard { id: NoteId, pos: Vec2 }
}

pub enum SessionOp {                          // journaled (2.11) but never touch note files
    Place { desk: DeskId, id: NoteId, pos: Vec2 },
    Move  { desk: DeskId, id: NoteId, from: Vec2, to: Vec2 },
    PutAway { desk: DeskId, id: NoteId, was_at: Vec2 },
    CreateDesk { name: String }, RenameDesk { .. }, ReorderDesk { .. }, DeleteDesk { .. },
}

impl SessionState {
    pub fn apply(&mut self, op: &SessionOp) -> SessionOp;    // returns inverse
    pub fn load(vault: &Vault) -> SessionState;              // .junkdrawer/session/*.jd; missing/corrupt → default
    pub fn save(&self, vault: &Vault) -> Result<(), IoError>;  // debounced by caller; atomic_save
}
```

File format (hand-parsed, versioned, disposable): one `session.jd`, line-based —
```
jd-session 1
surface = desk 01J8ZQDESK...
[desk 01J8ZQDESK...]
name = Reading
viewport = 120.5 -80.0 1.0
card = 01J8ZQ4KF3... 100.0 200.0
```

### 2.14 `guidance.rs` — Rules engine (pure)

```rust
pub struct GuidanceState { /* per-rule last-fired Timestamp, per-card dismissals; loads/saves .junkdrawer/guidance.jd */ }

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum BannerRule { EmptyVault, AgingInbox, FreshUnlinked, TagClusterNoDivider }

pub struct Suggestion {
    pub rule: BannerRule,
    pub text: String,                        // verbatim spec §11 strings
    pub target: NavTarget,                   // where clicking navigates: Inbox | Drawer(filter) | Note(id) | None
}

/// Ordered rule list, first match wins; None = silence (a valid suggestion).
/// Cooldowns: a fired rule is suppressed for RULE_COOLDOWN_DAYS (const, 3.0).
/// Called at surface-switch only — never on a timer.
pub fn evaluate_banner(index: &Index, state: &GuidanceState, now: Timestamp) -> Option<Suggestion>;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MarginHint { SetAsSource, MakeDivider, SplitReminder }
/// Card-margin detectors (spec §11): URL-in-empty-card, mostly-links, long card (> LONG_CARD_WORDS = 300).
pub fn margin_hints(meta: &NoteMeta, body: &str, dismissed: &(dyn Fn(NoteId, MarginHint) -> bool)) -> Vec<MarginHint>;
```

Thresholds pinned as constants with tests: `AGING_INBOX_DAYS = 3.0`, `TAG_CLUSTER_MIN = 10`, `MOSTLY_LINKS_RATIO = 0.6`, `FRESH_UNLINKED_DAYS = 2.0`.

### 2.15 `maplayout.rs` — Force layout (pure math, in core for headless testing)

```rust
pub struct LayoutParams { pub spring_k: f32, pub repulsion: f32, pub damping: f32, pub settle_eps: f32 }
impl Default for LayoutParams { /* tuned in WP5, then frozen */ }

pub struct ForceLayout { /* nodes, springs, spatial grid, velocities */ }
impl ForceLayout {
    /// `pinned` = positions restored from the .junkdrawer cache; they participate but keep spatial identity.
    pub fn new(node_ids: &[NoteId], edges: &[(NoteId, NoteId)], pinned: &HashMap<NoteId, Vec2>, params: LayoutParams) -> Self;
    pub fn step(&mut self, dt: f32) -> f32;      // returns max displacement; run per-frame until settled
    pub fn is_settled(&self) -> bool;            // then FREEZE — a map, not a lava lamp
    pub fn positions(&self) -> &HashMap<NoteId, Vec2>;
    /// New card since last session: seed near the centroid of its neighbors (ease-in handled by jd-app).
    pub fn add_node(&mut self, id: NoteId, edges: &[NoteId]);
}
```

Complexity requirement: repulsion via uniform spatial grid (cell = ~2× node spacing), O(n·k) per step; must remain real-time at 20k nodes (bench test in WP5).

### 2.16 `ipc.rs` — Single instance

```rust
/// Socket path derived from a hash of the canonical vault root:
/// UDS at $XDG_RUNTIME_DIR|/tmp/jd-<hash>.sock (unix), named pipe \\.\pipe\jd-<hash> (windows).
pub enum Claim {
    Primary(IpcServer),                  // we own this vault; serve messages
    Secondary(IpcClient),                // an instance exists; send it a message and exit
}
pub fn claim(vault_root: &Path) -> Result<Claim, IpcError>;   // stale-socket detection: connect-probe, unlink on ECONNREFUSED

/// The whole protocol — 3 messages, one line each: "FOCUS\n", "CAPTURE\n", "ACK\n".
pub enum IpcMessage { Focus, Capture }
impl IpcServer { pub fn poll(&mut self) -> Option<IpcMessage>; }   // non-blocking; jd-app polls per frame
impl IpcClient { pub fn send(&mut self, msg: IpcMessage) -> Result<(), IpcError>; }  // waits for ACK, 1 s timeout
```

### 2.17 `settings.rs` + `paths.rs`

```rust
#[derive(Clone, PartialEq, Debug)]
pub struct Settings {
    pub vault_path: Option<PathBuf>,
    pub recent_vaults: Vec<PathBuf>,                  // max 5
    pub capture_hotkey: Option<String>,               // e.g. "Ctrl+Shift+Space"; None = disabled
    pub start_in_tray: bool,
    pub trash_retention: TrashRetention,              // Days7 | Days30 | Days90 | ManualOnly (default Days30)
    pub theme: ThemePref,                             // System | Light | Dark
    pub card_style: CardStyle,                        // Paper | Plain (default Paper)
    pub ruled_lines: RuledLines,                      // None | Natural | Ink | FollowTheme (default FollowTheme)
    pub ui_scale: f32,                                // 1.0
    pub reduced_motion: MotionPref,                   // FollowOs | On
    pub banner_enabled: bool,                         // true
    pub margin_hints_enabled: bool,                   // true
}

impl Settings {
    pub fn load() -> Settings;                        // global file, then vault overrides applied by caller
    pub fn save(&self) -> Result<(), IoError>;        // unknown keys in the file preserved on rewrite
    pub fn apply_overrides(&mut self, vault: &Vault); // .junkdrawer/settings.jd, same format
}

// paths.rs (ours, ~40 lines):
pub fn config_dir() -> PathBuf;
// macOS: ~/Library/Application Support/JunkDrawer
// Windows: %APPDATA%\JunkDrawer
// Linux: $XDG_CONFIG_HOME/junkdrawer | ~/.config/junkdrawer
pub fn default_vault_dir() -> PathBuf;   // ~/JunkDrawer
```

File format: `jd-settings 1` header line, then `key = value` lines; `#` comments and unknown keys preserved byte-for-byte on rewrite (same line-preservation mechanism as frontmatter — extract it into a shared private helper if convenient).

### 2.18 `error.rs`

```rust
/// Typed; every variant carries enough context for the UI to render
/// "a human sentence with a next action" (spec §3). No `anyhow`, no `Box<dyn Error>` in public APIs.
pub enum CoreError { Io(IoError), Parse(FmError), Vault(VaultError), Watch(WatchError), Ipc(IpcError), Time(TimeError), Id(IdError) }
```

---

## 3. jd-app Architecture Notes (contracts internal to the app crate)

These matter for WP2–WP7 implementers; they're app-internal but pinned so parallel WPs compose.

**Frame loop (`app.rs::update`)** — strict order:
1. Drain `VaultHandle::events` (all pending), apply to `UiState` (body-cache invalidation, journal pushes for `OpDone{source: User}`, error toasts).
2. Poll `IpcServer` (focus / capture requests).
3. Global shortcut dispatch (respecting focus: editor-open routes text keys to editor).
4. Render: rail → banner → active surface → editor overlay (if open) → palette overlay (if open) → status line.
5. Debounced writes: autosave tick, session-state save tick.

**BodyCache (`state.rs`)** — `HashMap<NoteId, CachedBody { text: String, lex: Vec<(Vec<StyledSpan>, LineState)> }>`. Populated on card placement/open by a **read done on the worker? No — reads are cheap (1 KB) but the invariant is absolute:** body loads go through a small read-request channel to the vault worker (`VaultCommand` gains `ReadBody { id }` → `VaultEvent::Body { id, content }`); the card renders a blank face for the 1-frame gap. Invalidated by `VaultEvent::External/OpDone`.

**Editor (`editor.rs`)** — `egui::TextEdit::multiline` with `layouter` closure that: splits buffer into lines, pulls cached `(spans, exit_state)` per line from a line-cache keyed by `(line_hash, entry_state)`, re-lexes only lines whose hash or entry state changed, maps `SpanStyle → TextFormat` via `theme.rs`. Autocomplete popups (`[[` links, `#` tags) are egui popups anchored at cursor rect, fed by `Index::resolve/fuzzy` under a read lock. Autosave: dirty-flag + 1 s debounce → `SaveBody`; every keystroke also feeds the recovery journal (2 s debounce, `JournalBuffer`).

**Promotion detection (`editor.rs`)** — on Enter keypress in a `Fleeting` card's editor when the cursor sits at end-of-first-line and the buffer has one line: perform the visual restyle immediately (title formatting, card reshape animation unless reduced motion), mark editor state `PendingPromotion`; on editor close, emit `Promote` + `SaveBody` as one compound op (worker executes both; journal gets one compound entry). `Ctrl+Z` while `PendingPromotion` reverts in-editor without any vault op.

**Spatial focus order (`surfaces/desk.rs`)** — sort cards by `(row_band, x)` where `row_band = (y / BAND_HEIGHT).round()` with `BAND_HEIGHT = 0.6 × card_height` — stable under small drags. Focus is a `NoteId` in `UiState`; arrows move to nearest card in direction within band logic; every card is an AccessKit node labeled per spec §12 (`"Card: '<title>', N links, M tags"`).

**Ghost fan (`surfaces/desk.rs`)** — for the selected/open card: `k = 5` strongest off-desk connections ranked by: direct link (weight 3) > backlink (2.5) > shared-tag count (1 each) > `Index::similar` cosine (0–1). Rendered small + faded at the card's nearest free edge; click → `Place` at fan position.

**Theme (`theme.rs`)** — all colors as named constants in one file with a `#[test]` computing WCAG AA contrast ratios (≥ 4.5:1 text, ≥ 3:1 UI affordances) for every (foreground, background) pair actually used — including guidance-banner text. Paper texture/tints and ruled-line metrics live here too.

---

## 4. Work Packages

Each WP is independently implementable given this document + the spec + the interfaces of its dependencies. **Before starting a WP, generate its detailed TDD implementation plan** (bite-sized steps, complete code, per the writing-plans skill) from this section's requirements.

Dependency graph:

```
WP0 ─→ WP1a ─→ WP1b ─┐
         │            ├─→ WP1d ─→ WP1e ─→ WP2 ─→ WP3 ─→ WP4 ─┐
         └─→ WP1c ────┘                     │                  ├─→ WP6 ─→ WP7 ─→ WP8
                                            └────────→ WP5 ────┘
```

WP1b/WP1c parallelize after WP1a. WP5 needs only WP2's surfaces scaffolding + WP1. WP4 and WP5 parallelize after WP3.

---

### WP0 — Workspace skeleton & CI  *(spec M0)*

**Files:** root `Cargo.toml`, `rust-toolchain.toml`, `.gitignore` (exists — extend), `crates/jd-core/{Cargo.toml, src/lib.rs}`, `crates/jd-app/{Cargo.toml, src/main.rs}`, `.github/workflows/ci.yml`. Move the existing root `Cargo.toml`/`src/main.rs` into this shape.

**Deliverable:** `cargo build && cargo test && cargo fmt --check && cargo clippy -- -D warnings` green locally and in a 3-OS GitHub Actions matrix. `jd-app` opens an empty eframe window titled "Junk Drawer". `jd-core` has one placeholder test.

**Interfaces produced:** the workspace itself; CI as the merge gate.

---

### WP1a — Foundation types & parsers  *(spec §2, M1 part 1)*

**Files:** `jd-core/src/{time, id, rng, tag, note, frontmatter, doc, error}.rs`; `tests-data/golden/` corpus; `jd-core/tests/roundtrip.rs`.

**Requirements:**
- Everything in §2.1–2.7 above, implemented and table-driven-tested.
- **Golden corpus** (committed test data): ≥ 20 files — Obsidian-authored frontmatter, weird-but-legal YAML (block lists, quoted scalars, odd spacing), missing frontmatter, CRLF, BOM, emoji titles, RTL body text, unknown keys, duplicate keys, frontmatter-only file, empty file. Test: `NoteDoc::parse(x).serialize() == x` byte-identical for every corpus file.
- **Randomized round-trips:** xorshift generator producing adversarial frontmatter+bodies (≥ 1000 cases, fixed seed); asserts round-trip identity.
- Setter tests: mutate one field → only that line differs; unknown keys and ordering intact.

**Produces (consumed by everything):** `Timestamp`, `NoteId`, `IdGen`, `Tag`, `Status`, `Kind`, `NoteMeta`, `LinkRef`, `NewNote`, `FrontmatterDoc`, `NoteDoc`, extraction helpers.

---

### WP1b — Markdown line lexer  *(spec §5 dialect, M1 part 2)*  — parallel with WP1c

**Files:** `jd-core/src/lexer.rs`; `jd-core/tests/lexer.rs`.

**Requirements:** §2.8 exactly; the dialect is spec §5's complete list. Table-driven tests per construct; fence-state carry tests (fence opens on line 3 → line 4 is `CodeBlock`, closing fence resets); randomized span-sanity tests (cover-the-line / no-overlap / no-UTF-8-split invariants, xorshift bodies). Tables/footnotes/HTML lex as `Text` (round-trip untouched happens for free — the lexer never rewrites).

**Produces:** `lex_line`, `StyledSpan`, `SpanStyle`, `LineState` — consumed by WP2 (editor + card faces).

---

### WP1c — Index, search, fuzzy  *(spec §3 index, §7 search, M1 part 3)* — parallel with WP1b

**Files:** `jd-core/src/index/{mod, search, fuzzy}.rs`; `jd-core/tests/{index, search, fuzzy}.rs`.

**Requirements:**
- §2.9 exactly. Incremental `upsert`/`remove` keep link resolution consistent when titles appear/disappear (test: unresolved link resolves when target created; re-unresolves on delete).
- Query language: words AND, `"phrases"` (position-verified), `#tag` (plural-insensitive), `-word`; final term prefix-matched. That's the whole grammar; anything else is a term.
- Fuzzy ranking tables pinned as tests, including the acronym case (`nasa` → "National Aeronautics and Space Administration" ranks above subsequence matches) and consecutive-run/word-boundary bonuses.
- `similar()` sanity tests (shared-vocabulary notes rank above disjoint ones).

**Produces:** `Index`, `SharedIndex`, `Query`, `parse_query`, `SearchHit`, `Snippet`, `fuzzy_match` — consumed by WP1d, WP2 (autocomplete), WP4 (palette/drawer), WP5 (map search), WP6 (guidance).

---

### WP1d — Vault engine: io, scan, watcher, worker  *(spec §3, M1 part 4)*

**Files:** `jd-core/src/vault/{mod, io, scan, watcher, trash, recovery}.rs`, `jd-core/src/worker.rs` (worker only for ops that exist so far: Create/SaveBody/ReadBody + scan/watch plumbing; the full `VaultOp` set lands in WP1e); `jd-core/tests/vault_engine.rs`.

**Requirements:**
- §2.10 + §2.12 (worker skeleton: thread, channels, wake callback, write-ledger, conflict rule).
- Integration tests on temp dirs: atomic-save torture (simulated kill between temp-write and rename via injected failpoint closure — original always intact) · title collision suffixing · conflict-copy creation · trash lifecycle incl. retention purge · recovery journal survives simulated crash · watcher debounce coalescing · **editor-zoo**: rename-swap saves (vim-style), truncate-rewrite, create-then-rename — all normalize to correct index updates.
- **Performance budget tests** (`#[test]`, CI): generate 20k synthetic notes (xorshift), cold `scan` < 1 s; single-file re-index < 5 ms; `query` < 10 ms. These are the tripwire that legally activates the spec §3 snapshot escape hatch — if one goes red and can't be optimized, the snapshot WP gets scheduled; never SQLite.
- **Perf-harness hot spots flagged by the WP1c review** — measure these three explicitly: (a) `Index::query` with a large-membership tag filter (member-set clone per query tag), (b) `unwire`'s whole-tag-map scan during a full 20k rebuild (fix = per-note tag key list if it shows), (c) `similar()` on a high-degree note (norm recomputation per call; fix = cached norms).

**Consumes:** WP1a types, WP1c `Index`.
**Produces:** `Vault`, `scan`, `atomic_save`, `filename_for`, `VaultWatcher`, trash/recovery APIs, `start() → VaultHandle`, `VaultCommand`/`VaultEvent`.

---

### WP1e — Command layer & journal  *(spec §9 core, M1 part 5 / M3 prerequisite)*

**Files:** `jd-core/src/{command, journal, session}.rs`; extend `worker.rs` to execute every `VaultOp`; `jd-core/tests/{commands, session}.rs`.

**Requirements:**
- §2.11 + §2.13 in full. `RenameTitle` rewrites `[[links]]` in all referrers (case-preserving where the link text differed only by case? No — links resolve case-insensitively; rewrite replaces the bracketed target text with the new title verbatim, preserving any `|display` part).
- `Split` semantics (spec §5): body after `at_byte` becomes a new note (Fleeting; or Permanent-with-title if it starts with `# `), a `[[New Title]]` (or `[[<first-line>]]` link-as-created for fleeting) replaces the removed text.
- **Inverse law tested for every op:** execute → execute-inverse → index state (and on-disk tree) exactly restored. Property-tested with randomized op sequences where feasible.
- `SessionState` load/save round-trip; corrupt file → default state, no error surfaced (disposable by design).

**Consumes:** WP1a/1c/1d.
**Produces:** `VaultOp`, `OpResult`, `Journal`, `SessionState`, `SessionOp`, `DeskId` — the API surface WP3 wires to keys.

**Handoffs inherited from WP1d's reviews (address here, don't drop):**
- Wrap `trash_note`/`restore`/`purge_older_than`/`journal_buffer`/`clear_buffer` in `VaultCommand`s — restores the single-writer invariant STRUCTURALLY (WP1d enforces it by documented contract only).
- Refactor the worker's command set into the `Op { op: VaultOp, source }` shape pinned in §2.12.
- Carry `created` forward from the prior index entry when an external edit loses frontmatter (WP1d preserves the id but resets `created` to fs mtime).
- Add an `Index::replace_at_path`-style atomic swap for the watcher's path-reuse case (WP1d does remove+upsert under separate locks — transient absence for concurrent readers).
- Setter behavior tests through the worker: `set_source` inner-quote escaping, `set_tags(&[])`, bare-scalar `tags:` form (WP1a review gaps).

**Milestone gate:** M1 complete — the vault engine is fully exercised headless before a single pixel.

---

### WP2 — Desk, cards, editor  *(spec §4, §4.5, §5, M2 — the two riskiest UI pieces)*

**Files:** `jd-app/src/{app, state, theme}.rs`, `card/{mod, shape}.rs`, `surfaces/desk.rs`, `editor.rs`, `text_undo.rs`; `jd-app/tests/desk_kittest.rs`; snapshot tests for card faces.

**Requirements:**
- Frame-loop order, BodyCache, editor architecture per §3 of this document.
- **Two front-loaded spikes, week one, before full implementation** (spec §14 risks 1 & 3):
  - *Spike A — mixed-size layouter:* headings at heading size inside one editable `TextEdit` galley. Exit criteria: cursor/selection/IME correct across a size boundary. Fallback if failed: uniform size, weight/color styling only — record the decision here.
  - *Spike B — AccessKit spatial focus on a free-form canvas:* arrow-key traversal in spatial reading order with correct screen-reader announcements, validated in `egui_kittest` (which drives the AccessKit tree). Built alongside the desk, not after.
- Desk: pan (scroll/middle-drag), zoom (Ctrl+scroll, limits + zoom-to-fit), card drag, positions persisted via `SessionState` (debounced), offscreen culling, closed-card readable faces, session restore (desk + viewport + open card).
- Card visual language: all four shapes × Paper/Plain × three line styles — **snapshot-tested** (that's 4×2 + 3 line variants; enumerate all legal combos). Shape is semantic and survives Plain; texture is Paper-only.
- **Fonts bundled** (spec §12): Inter + JetBrains Mono embedded via `egui::FontDefinitions` in `theme.rs`, system-font fallback for uncovered scripts; ruled-line metrics computed from the bundled face.
- Editor: dialect styling via `lex_line` line-cache; Enter list/quote continuation; Tab indent; `[[`/`#` autocompletes; URL-paste behaviors; **no smart quotes ever**; Esc closes-and-saves; autosave + recovery journaling; per-card text undo (word-granularity grouping, survives close/reopen within session).
- **Layouter line-length guard (WP1b review finding):** `lex_line` is O(n²) on pathological single lines of unclosed delimiters (~seconds at 200k chars). The layouter must cap per-line lexing (lex the first ~8 KB of a line, style the rest as `Text`) — cheap, invisible for human-authored notes, and keeps the UI thread safe against adversarial pastes.
- `Ctrl+N` → scrap at cursor + editor open (files into `inbox/` via `Create`).

**Consumes:** WP1 everything.
**Produces (for WP3–WP6):** `CardWidget` (face rendering reused by Drawer/Map minis), `EditorState` + open/close API, `UiState` conventions, `theme.rs` constants, desk surface with placement API (`place_card(desk, id, pos)`).

---

### WP3 — The workflow: inbox, promotion, undo wiring  *(spec §6, §9, M3)*

**Hardening list inherited from WP1e's final review (address or consciously defer here):**
- Batch rollback failures are silently swallowed (`let _ =` on rollback inverses) — emit an `Error` event so a mixed-state vault is observable.
- `RenameTitle` has no rollback for mid-referrer-loop failures (self already renamed, some referrers rewritten) — wrap its multi-file writes in Batch-style rollback discipline.
- Body-derived filenames make some undo paths rel_path-unstable (untitled-note Batch case; RenameTitle-undo when the old name got re-claimed) — decide: accept-and-document, or carry paths in inverses.
- Undo of Split leaves the split-off note in trash (consistent with Create-undo; document in undo UX copy).

**WP2 → WP3 handoffs:**
- Face checkbox ☐/☑ glyph substitution + click-to-toggle (needs face-only text transform; editor must keep raw source — TODO(WP3) in theme.rs). ✓ (WP3)
- Ctrl+Enter promotion hook: editor_ui returns EditorEvent; promotion branches there. ✓ (WP3)
- Stale pending_create if a Create's OpDone precedes ScanComplete (comment at consumption site in app.rs); consider a ScanComplete sweep. ✓ (WP3)
- ac_dismissed persists for the whole [[ context after Esc — consider re-showing on query change. ✓ (WP3)
- Editor autosave/debounce tests use one real sleep each — watch for CI flake; #[ignore] escape hatch documented.
- jd-core worker sets `modified` on every SaveBody — frontmatter is deliberately NOT byte-identical after a dirty save (the round-trip law's one sanctioned field change; don't "fix" set_modified).

**Files:** `jd-app/src/surfaces/{inbox, trash}.rs`, `rail.rs`, promotion logic in `editor.rs`, journal wiring in `app.rs`, `menus.rs` (Edit-menu subset + Card context menu); kittest scenarios.

**Requirements:**
- Inbox surface: every `Fleeting` note, oldest-first, scattered pile under Paper / tidy column under Plain; the three acts (Promote / Toss / Take-to-desk with `Ctrl+D` picker); quiet rail count.
- **Enter-promotes** exactly per spec §6 and this doc's promotion-detection design; `Ctrl+Enter` same code path; compound undo entry (one `Ctrl+Z` reverses text + status + file move); `inbox/` → `notes/` move commits on editor close.
- Toss (`Del`, no confirm for scraps) / Delete (confirm for permanent) → trash; Trash surface with restore + retention notice; demote via Card menu only.
- App-stack undo/redo: routing rule (`editor open → text stack, else app stack`), **named** Edit-menu entries, status-line echo, view-travel to the op's `OpContext`.
- **The left rail** (`rail.rs`): named desks — create, rename, reorder — plus Inbox (with its quiet count), Drawer, Map, and Trash entries; drag-a-card-to-rail = put away (onto a desk name = take to that desk). Drawer/Map rows navigate to placeholder surfaces until WP4/WP5 land.
- **Split UI** (`Ctrl+Shift+Enter` in the editor): dispatches `VaultOp::Split` at the cursor byte; both cards placed side by side on the current desk.
- **Card context menu** (`Shift+F10`/right-click, spec §10 Card menu): Promote · Toss · Take to Desk ▸ · Put Away · Set Source… · Make Divider · Demote to Scrap · Copy Link · Reveal in File Manager.
- Kittest scenarios: capture → appears in inbox; Enter-promotes restyles + moves file; split; toss + undo restores; put-away vs toss distinction; promotion undo as single step.

**Consumes:** WP1e ops/journal, WP2 desk/editor.
**Produces:** complete capture→promote→link loop; the app is now dogfoodable.

**WP3 → WP4 handoffs:**
- Inbox faces don't click-toggle checkboxes (desk faces only) — extend the face hit-test to the inbox pile if wanted.
- Drag pointer-path to the rail is untested headless (kittest pointer-drag over panels is unreliable); the event-level path (`CardDroppedOnInbox`/`CardDroppedOnDesk`) is authoritative and covered.
- Cut/Copy/Paste Edit-menu items are disabled pending an egui programmatic clipboard path (revisit at WP6 menus).
- Shift+F10 popup can open and close within one frame if a click lands the same frame — cosmetic only.
- Multibyte-indent glyph offset drift in checkbox face substitution (exotic content; tracked, not blocking).
- Real-sleep tests (editor debounce/autosave) remain a CI-flake watch item; #[ignore] escape hatch documented.
- `pending_create` sweep keeps only the FIRST orphaned Create that lands pre-scan; later orphans are dropped.
- `created` timestamps persist at second precision on disk — post-restart same-second inbox ordering falls back to ULID order.

---

### WP4 — Palette, Drawer, ghost fan  *(spec §7, §6 connection step, M4)*

**Files:** `jd-app/src/palette.rs`, `surfaces/drawer.rs`, ghost-fan in `surfaces/desk.rs`; kittest scenarios.

**Requirements:**
- Palette: three strata in one list (fuzzy titles / BM25 snippets / always-last "New scrap: '…'"), miniature face cues per row, Enter places at viewport center **or pans to the existing card (never moves it — spatial layout is sacred)** with highlight pulse (skipped under reduced motion), `Ctrl+Enter` places-and-opens, empty-palette shows the query syntax.
- Drawer: mini grid (reusing `CardWidget`), newest-modified first, filter chips (status, kind, tag picker with counts, **Unlinked**, **Needs Attention** = quarantine + conflicts), chips compose/dismiss, chip row is the visible query. No saved searches. Enter opens editor in place; `Ctrl+D`/drag to desk.
- Ghost fan per §3 of this doc (ranking weights pinned as a unit test on `jd-core` side if extracted, else app-side test).
- Kittest: palette placement, already-on-desk centering, strata ordering, drawer chip composition.

**Consumes:** WP1c search/fuzzy, WP2 widgets, WP3 workflow.

**WP4 → WP5 handoffs:**
- Drag-from-grid (Drawer mini → desk) deferred; the event-level path (`DrawerEvent::PlaceOnDesk` via Ctrl+D picker) is authoritative and covered.
- Drawer grid does not scroll yet (rows below the fold are culled, not reachable by mouse; keyboard focus still walks them) — WP6 alongside its other chrome work.
- Drawer chips remain clickable while the tag/desk-picker popups are open (keyboard is gated; mouse is not) — cosmetic, same class as the WP3 popup one-frame quirk.
- Unresolved `[[links]]` don't count as outgoing for the **Unlinked** chip (pre-existing jd-core `unlinked()` semantics) — product decision note: a note whose only links are dangling shows as Unlinked.
- Whitespace-only "New scrap: '…'" palette activation silently no-ops (guarded, no echo) — decide whether it deserves a status echo.
- Palette placement from a non-desk surface pans to the FIRST desk with a status echo; a "most recently used desk" target may be friendlier — revisit with WP5's Map navigation.
- Ghost fan does not avoid overlapping other placed cards (only the anchor); the freest-edge heuristic keeps it usually clear — revisit if the Map's layout tooling (WP5) makes a cheap avoid-pass available.
- The overlay-gating matrix is now uniform for the desk: every mouse mutation path (drag-start, checkbox toggle, double-click-to-open, keyboard, context menu) checks `palette_open`. WP5's map surface must gate its own mouse mutations on `palette_open` from day one — the palette overlay does not swallow pointer events for the surface beneath it.

---

### WP5 — The Map  *(spec §8, M5)*  — parallel with WP4 after WP3

**Files:** `jd-core/src/maplayout.rs` + bench test; `jd-app/src/surfaces/map.rs`; position cache in `.junkdrawer/map.jd`.

**Requirements:**
- §2.15 layout: settles then freezes; positions cached and stable across sessions; new nodes ease in near neighbors (appear-settled under reduced motion); real-time at 20k nodes (bench `#[test]`: one `step()` at 20k nodes/40k edges < 16 ms).
- Rendering: dots sized by link degree (gentle), shapes/tints per visual language, dividers slightly larger, **edges are links only**, orphans ringed at the edge.
- Interactions mirror Drawer: hover title, click → mini, Enter → editor in place, `Ctrl+D` → desk, `Ctrl+K` within-map (matches light, rest dims).
- Equivalence rule holds by construction (everything reachable via Drawer views) — verify in review, note in the WP plan.

**Consumes:** WP1, WP2 widgets, WP3.

---

### WP6 — Guidance, settings, menus, shortcuts  *(spec §10, §11, M6)*

**Files:** `jd-core/src/{guidance, settings, paths}.rs` (+tests), `jd-app/src/{banner, menus, shortcuts, settings_ui}.rs`; kittest for banner rules + menu semantics.

**Requirements:**
- §2.14 guidance engine: ordered rules, first-match, cooldown-days, surface-switch evaluation only, verbatim spec strings, click-navigates, dismissals persisted; card-margin hints (footer, low-contrast, static, per-card-permanent dismissal); empty states for every surface; tooltips always with shortcut.
- §2.17 settings: file format, unknown-key preservation, per-vault overrides; the one dialog, three groups, ~a dozen items, ceiling deliberate; conditional visibility (ruled lines only under Paper, hotkey row hidden on Wayland, tray only under `resident`).
- Menus: full spec §10 tree, egui-drawn everywhere first; **muda spike** (spec §14 risk 2) behind `mac-menu` feature — exit criteria: native mac bar + working accelerators alongside eframe's event loop; fallback: egui bar on mac, explicitly temporary.
- `Ctrl+/` shortcut overlay grouped by surface (renders Appendix A); Export Desk as Outline (cards in spatial reading order, `[[links]]` resolved to titles → clipboard or `.md` via `rfd`); Open Vault / Recent Vaults (one vault per window: second vault spawns a new process with the path argument).
- **Back/Forward navigation** (`Ctrl+[` / `Ctrl+]`, View menu): a surface-visit history stack in `UiState` (surface + viewport), capped at 50 entries.

**Consumes:** everything prior.

---

### WP7 — Platform residency & accessibility hardening  *(spec §12, M7)*

**Files:** `jd-core/src/ipc.rs` (+tests), `jd-app/src/{platform, capture}.rs`, `main.rs` arg handling; a11y audit fixes across all surfaces.

**Requirements:**
- §2.16 single-instance: second launch → `Focus` handoff; `jd-app --capture` → capture popup from the running instance (or a fresh minimal process if none). Protocol integration-tested with two real processes on all three OSes in CI where runners allow (unix at minimum; windows named-pipe test).
- `resident` feature: `global-hotkey` + `tray-icon` on Windows/macOS/X11; capture popup (Enter saves / Shift+Enter newline / Esc discards, main window never appears); Wayland: hotkey setting hidden, inline fallback text pointing at compositor-shortcut + `jd-app --capture`; tray tested on GNOME + KDE VMs.
- Accessibility hardening pass: card announcements per spec §12, full keyboard-only workflow (release-gate checklist scripted where possible in kittest), reduced-motion sweep (pulse, ease-ins, texture animation all gated), theme contrast test extended to every string added since WP2.

**Consumes:** WP2–WP6.

---

### WP8 — Packaging, signing, update check, user guide  *(spec §12 shipping, M8)*

**Files:** `.github/workflows/release.yml`, packaging scripts (`packaging/{macos,windows,linux}/`), update-check in `jd-app` (status-line/About note; manual + weekly against the GitHub releases feed via OS-native HTTP? **No HTTP crate is approved** — resolve at WP8 planning: options are (a) approve `ureq` in Appendix B, (b) shell out to `curl`/`PowerShell`, (c) drop auto-check to manual-only "Check for updates" opening the releases page in the browser. Recommendation: (c) for v1 — zero dependencies, spec's "notification only" posture kept by the weekly nag being a local date check + prompt to visit).
- macOS universal binary, `.app` in `.dmg`, signed + notarized; Windows NSIS + portable zip, signed; Linux AppImage + tar.gz.
- Release-gate checklist committed at `docs/release-checklist.md` (VoiceOver + NVDA pass · keyboard-only pass · artifact smoke on fresh VMs · 50k-note vault open · reduced-motion sweep).
- User guide (`docs/guide.md`, opened by Help ▸ User Guide).

---

## 5. Cross-Cutting Test Inventory (who owns what)

| Test class | Owner | Gate |
|---|---|---|
| Golden-corpus byte-identity round-trip | WP1a | CI |
| Randomized round-trip + lexer span sanity | WP1a/WP1b | CI |
| Fuzzy ranking tables (acronym pinned) | WP1c | CI |
| Atomic-save torture, editor zoo, watcher debounce | WP1d | CI |
| Perf budgets: scan < 1 s / reindex < 5 ms / query < 10 ms @ 20k | WP1d | CI (red build on drift) |
| Op inverse law (every `VaultOp`) | WP1e | CI |
| Card-face snapshots (shapes × styles × lines) | WP2 | CI |
| WCAG contrast computation over theme constants | WP2 (extended WP6/7) | CI |
| Kittest scenarios (capture, promote, split, palette, undo naming) | WP3/WP4 | CI |
| Map layout bench @ 20k nodes | WP5 | CI |
| VoiceOver/NVDA, keyboard-only, fresh-VM smoke, 50k vault | WP8 checklist | Release gate (manual) |

## 6. Decisions Made Here (not in the spec — flag disagreements early)

1. **RFC3339 + config paths + word segmentation written in-house** (consistent with the rejected-crates list; adds `time.rs`, `paths.rs`, segmentation helper to the write-ourselves inventory).
2. **ULID entropy** from a non-cryptographic xorshift seeded by time/pid/address — acceptable for a single-user app; keeps `rand` out.
3. **Byte-identity via line preservation**: frontmatter (and settings files) keep raw lines; setters rewrite only their line. This is the mechanism that makes the round-trip law cheap to uphold.
4. **Body reads route through the worker** (`ReadBody` command) to keep the no-FS-on-UI-thread invariant absolute, at the cost of a one-frame blank face.
5. **Desk operations are session ops, not vault ops** — journaled alongside vault ops under one `InverseAction` enum; no note file is touched by placement.
6. **Update check v1 = manual link-out + local weekly reminder** (no HTTP dependency); finalize at WP8 planning.
7. **Guidance thresholds** (aging = 3 days, cluster = 10 cards, long card = 300 words, mostly-links = 60 %) are pinned constants — tune before WP6 ships, then freeze.
8. **One vault per window = one process per vault**; “Open Vault” spawns a process, single-instance claim is per-vault (socket name hashes the vault path).
9. **UTF-8 BOM (owner ruling, 2026-07-06):** a leading `EF BB BF` is tolerated on parse and dropped on save — the one sanctioned normalization of the round-trip law. Output files are always BOM-less UTF-8. Non-UTF-8 files fail `read_to_string` and quarantine at scan (WP1d).
10. **SearchHit carries matched terms, not snippets** — snippets need bodies, bodies aren't in the index, and query() runs on the UI thread. `make_snippet(body, terms, radius)` is a pure helper the app calls after loading bodies via the worker (visible rows only).
11. **Lexer span semantics** — emphasis spans include their delimiters, no nesting; heading rest is a single `Heading(n)` span (no inline styling in headings, v1); token positions in search postings are token indices (phrase adjacency), not byte offsets.
12. **Title collisions in the index** — `titles` maps lowercased title → the most recently upserted note; duplicate titles are legal on disk (filename suffixing handles files), and links resolve to the latest holder.
13. **Mixed-size layouter PROVEN (Spike A, WP2, 2026-07-07):** real heading sizes (24/20/17 on 15pt body, JetBrains Mono 14) inside one editable TextEdit galley. Mechanism: one LayoutJob tiling every buffer byte (per-line lex cache keyed on (line-hash, fence-entry-state), 8KB per-line lex cap, explicit '\n' appends); egui's cursor/selection/IME machinery handles mixed row heights natively. Heading-marker spans derive their level from leading '#' count at layout time (SpanStyle::HeadingMarker carries none). Unresolved wikilinks: text_weak color + solid underline (egui has no dashed) vs accent+underline for resolved. Exit criteria (taller row, cursor-exact typing across the boundary, boundary-spanning select-all) are pinned as tests in jd-app/tests/spike_layouter.rs.
14. **AccessKit spatial focus on a free-form canvas PROVEN (Spike B, WP2, 2026-07-07):** `ui.allocate_rect(rect, Sense::click_and_drag())` + `response.widget_info(|| WidgetInfo::labeled(WidgetType::Button, true, label))` yields fully queryable/actionable AccessKit nodes at arbitrary canvas positions — no framework workaround needed. Reading order = (y-band rounded at BAND_HEIGHT 120px = 0.6×card-height, then x, then NoteId); Left/Right walk the reading order globally across bands; Up/Down seek nearest |Δx| outward band-by-band; no wrap at the true ends. Culled (offscreen) cards get no nodes; focusing one auto-reveals (viewport centers via the cached real panel rect). Labels per spec §12, singular/plural correct.
15. **WP3 mechanisms (2026-07-08):** `pending_label` (UiState) overrides the worker's generic Batch label for compound acts (promotion "Promote scrap '<line1>'", split) — consumed by the next matching User OpDone, cleared on OpFailed. `InverseAction::Sessions(Vec<SessionOp>)` added to the jd-core journal for multi-step session inverses (the composite move-to-desk); undo applies the ops in order, redo = reverse-ordered collected inverses. Split placement rides the op un-journaled (the Split undo trashes the split-off); inbox-origin splits fall back to the first desk with a status echo. Checkbox faces use □/■ (Inter-covered glyphs), fence-aware recognition shared with the lexer, ordinal-mapped toggles via SaveBody.
16. **WP4 ghost fan + edges (2026-07-08):** Ghost ranking weights pinned by unit test (`ghost_score`, jd-app/surfaces/desk.rs): direct link 3.0 > backlink 2.5 > shared tag 1.0 each, relations stack, plus `Index::similar` cosine clamped 0–1. Blend: structural relations scored exhaustively from index adjacency; cosine only for `similar(id, 32)`'s top-N (outside it, cosine contributes 0 — deliberate approximation), cosine-only neighbours still enter the pool. Top k=5, OFF-desk notes only, tie-break by id. Fan edge heuristic: most free world-space between the anchor card and the visible panel bounds among N/E/S/W; E/W fans stack vertically, N/S horizontally. Ghost minis are tinted-rect-plus-title at 40% scale (sanctioned fallback: `card_face` has no opacity parameter and laid-out galleys can't be alpha-multiplied without re-tessellation). Ghosts carry AccessKit labels ("Ghost: '<title>'") but are NOT in the arrow-key reading order: they're previews, the palette/Drawer are the keyboard paths to the same notes (no-spatial-only law holds), and transient fan members would destabilize arrow traversal. Clicking a ghost = `place_card` at the ghost's world position — journaled "Place card" like every placement. Edges-on-select: `selected_edges` (pure fn = the kittest seam) returns deduped both-direction link edges between the focused-or-open card and on-desk cards; painted beneath the card faces with a subtle theme stroke. The blend is additive: cosine (≤1.0) can reorder candidates whose structural tiers differ by less than 1.0 (e.g. a highly similar backlink can outrank a dissimilar direct link) — accepted; the structural ordering is pinned only at zero/equal cosine. "Off-desk" means off the CURRENT desk: a card placed on another desk can appear as a ghost here, and clicking gives it membership on both desks — consistent with the palette's place semantics.
