# Junk Drawer — Design Document

*2026-07-03. Status: agreed section-by-section with the project owner; this document is the assembled result.*

Junk Drawer is a desktop Zettelkasten app in Rust (egui) for people whose brains don't cooperate with Obsidian. You throw thoughts in fast, and the app — not you — keeps track of what's unfinished, what connects to what, and what to do next. The name is the promise: everything goes in one drawer, and you can still find it.

---

## 1. Vision & Principles

**Who it's for.** ADHD and otherwise neurodivergent note-takers are the primary users, and the success criterion: if it isn't good for keeping *their* knowledge straight, it hasn't succeeded. Neurotypical users are served fine by the same choices; the reverse is not true.

**Design principles.** These settle arguments later in the doc; every feature decision should trace to one.

1. **Capture beats working memory.** The path from "thought occurs" to "thought is safe" is one action and zero decisions. No filename, no folder, no template, no tags required at capture time. Ever.
2. **The app holds the structure.** Unelaborated notes resurface on their own; "what was I doing?" always has a visible answer; nothing depends on the user remembering to come back.
3. **One obvious way.** Each task has a single blessed path. The app is excellent with all defaults untouched; no configuration is required to reach a working system, and no customization is deep enough to become a project of its own.
4. **Invitational, never shaming.** The inbox shows what's ready to work on, not a red badge of failure. No streaks, no overdue counts, no guilt mechanics.
5. **Plain files, no lock-in.** The vault is readable markdown that outlives the app. Obsidian, git, and grep all work on it.
6. **Trust through boredom.** Never lose a keystroke, never corrupt a file, never surprise the user. Undo everything undoable, confirm everything destructive.
7. **Accessible by requirement.** Keyboard-completable, screen-reader-usable (AccessKit), respectful of reduced motion. Not a v2 promise.
8. **The app teaches the method.** Guidance is a first-class subsystem — ambient, matter-of-fact, and unobtrusive enough that most users never turn it off. Using the app is its own reward: no congratulations, no modal tutorials, no Clippy.

**Non-goals (v1 and mostly ever):** plugins and theming beyond what §10 lists, sync (files + the user's own sync tool), mobile, collaboration, WYSIWYG rich text, PDF/web clipping, a database. Plugins get reconsidered only if real users demonstrate a need. "Output" (drafting from cards) happens outside the app; the one affordance is Export Desk as Outline (§10).

**Dependency policy.** Keep the tree as tight as possible. Take the mature crate where the problem is genuinely hard or platform-cursed; write it ourselves where it's small and we control the scope. Appendix B has the approved list and the write-it-ourselves inventory.

---

## 2. The Vault: On-Disk Format

**Vault = one folder** the user picks (suggested default `~/JunkDrawer`). The app manages two note subdirectories — `inbox/` for fleeting scraps and `notes/` for permanent cards — which is workflow state made visible, not an organization scheme the user maintains. **There are no user-managed folders.** Hierarchy is what tags, links, and divider cards are for; folder-taxonomy paralysis is an Obsidian failure mode we're deliberately not importing.

**A note is one `.md` file**: YAML frontmatter with a fixed schema, then a markdown body.

```markdown
---
id: 01J8ZQ4KF3T9M2X7C5VBNAE8RD
created: 2026-07-03T10:22:00Z
modified: 2026-07-04T09:10:00Z
status: permanent
kind: literature
source: "Ahrens, How to Take Smart Notes (2017)"
tags: [zettelkasten, method]
---

# Elaboration is what turns a note into knowledge

Body with [[Wiki Links]] and #inline-tags.
```

Field semantics:

- `id` — ULID, assigned at creation, never changes. The note's true identity.
- `status: fleeting | permanent` — **lifecycle**, the axis the workflow engine runs on. Two values in reality: unprocessed vs processed.
- `kind: note | literature | structure` — **what the note is**, orthogonal to lifecycle. Optional; absent means `note`. Never required by any flow; usually suggested by detection (§11) or set explicitly (§10 Card menu). `literature` implies a `source:` field (URL, citation, book). Rationale: the formal Fleeting → Literature → Permanent → Structure pipeline conflates lifecycle and kind; separating the axes lets a two-state beginner and a full-orthodoxy user share the same files with no migration.
- `tags` — frontmatter list; `#inline-tags` in the body are also honored (union). Flat, lowercase, plural-insensitive matching. No nesting, colors, or hierarchy in v1.
- **Unknown frontmatter keys are preserved byte-for-byte on save** — round-trip safety for files that visited Obsidian or any other tool.

**Filenames** = sanitized title + short ID suffix only on collision (`Egui immediate mode tradeoffs.md`; `… (01J8ZQ4K).md` if a title clash occurs). Untitled scraps use a timestamp-flavored name. Renaming a title renames the file and rewrites `[[links]]` in referring notes — safe because IDs, not paths, are identity.

**Links** are `[[Title]]` wikilinks, resolved by title (case-insensitive); `[[Title|display text]]` supported. A link matching nothing is an **unresolved link** — a first-class concept, shown dashed-underlined; activating it creates the note.

**`.junkdrawer/`** holds vault-local machine state, all of it disposable (deleting the folder loses no notes): settings overrides, session state (desks, card positions, viewports, open card), guidance dismissals/cooldowns, Map position cache, `trash/` (deleted notes with timestamps, purged per the retention setting), `recovery/` (journaled unsaved buffers), and — only if the §3 escape hatch ever activates — the index snapshot.

**External edits are legal.** The watcher picks up changes from other tools. If a file changes on disk while also modified in the app, keep both (conflict copy with clear naming, surfaced in Needs Attention) — never silently clobber either side.

---

## 3. Architecture

**Workspace layout.** A Cargo workspace, two crates:

- **`jd-core`** — everything that isn't pixels: vault I/O, frontmatter + markdown-token parsers, in-memory index, link graph, search, command/undo journal for vault operations, settings persistence, watcher integration. No egui dependency; fully testable headless.
- **`jd-app`** — the eframe/egui binary (glow backend): surfaces, card widgets, editor, Map rendering, guidance, menus, shortcuts. Holds UI state; delegates every vault mutation to `jd-core`.
- (A future `jd` CLI would be a third tiny crate over `jd-core`; nothing in v1 requires it.)

**Index strategy — files + RAM.** The vault on disk is the *only* persistent truth. Startup does a parallel scan parsing every note's frontmatter, links, and tags into an in-memory index; a watcher keeps it fresh. No database, no schema migrations, no two-sources-of-truth reconciliation; if the index is ever wrong, restart rebuilds it perfectly. Zettelkasten notes are small (~1 KB), so even 50k notes is ~50 MB of parsing — well under a second in parallel Rust, tens of MB resident.

> **Escape hatch (sanctioned, dormant):** if cold start exceeds 1 s at 20k notes (enforced as a CI perf test, §13), add a versioned binary snapshot of the index in `.junkdrawer/`, loaded on launch, discarded and rebuilt if stale or corrupt. Never SQLite; the snapshot is disposable by design.

**Threading model — three lanes:**

1. **UI thread** — egui's loop. **Invariant, stated as a rule: the UI thread never blocks — no filesystem, no locks held across frames, no joins. All waiting happens on channels.** Long operations get a *labeled* progress modal ("Rescanning your notes… 3,200 of 12,000"); an unlabeled spinner or a frozen frame is a bug by definition.
2. **Vault worker** — one background thread owning all writes. Commands arrive on an `std::sync::mpsc` channel (`SaveNote`, `RenameNote`, `PromoteNote`, `DeleteNote`, …), execute serially (no write races by construction), results post back on a channel drained once per frame. Saves are atomic: temp file, fsync, rename over the original.
3. **Watcher/indexer** — `notify` events debounced ~200 ms; re-parse changed files, update the index incrementally. Full parallel rescan only at startup or on demand.

The index lives behind `Arc<RwLock<Index>>`; UI takes brief read locks, worker/indexer take write locks for incremental updates. **Note bodies are not held in the index** — only metadata, link adjacency, tag maps, and search postings; bodies load on card open (a 1 KB read).

**Index contents:** `notes: HashMap<NoteId, NoteMeta>` (title, path, status, kind, timestamps, tags, out-link spans, word count) · `titles: HashMap<lowercased title, NoteId>` · forward + reverse link adjacency · `tags: HashMap<Tag, Set<NoteId>>` · inverted search index (unicode-segmented lowercased terms → postings with positions).

**Hot path for a keystroke:** buffer mutates in UI state → incremental re-lex of the edited region for styling → debounced autosave (~1 s; also on editor close, surface switch, quit) → worker writes atomically → indexer updates postings/links → connection displays refresh. No save button exists. The worker additionally journals unsaved buffers to `.junkdrawer/recovery/`, so a crash loses nothing, including the debounce window.

**Error posture.** `jd-core` returns typed errors; the UI renders human sentences with a next action ("Couldn't save 'Egui tradeoffs' — the file is read-only. [Retry] [Save a copy]"). Background failures surface in the status line, never as modal stacks. A file that fails to parse is quarantined into Needs Attention; it never fails the scan.

---

## 4. The Desk

**The whole app is a surface.** No document panes, no center editor. The window is a pannable 2D workspace — a **desk** — with a slim rail on the left, the thin guidance banner on top, a status line on the bottom. You put cards down, drag them around, read them in place, put them away when done.

**Cards.** Every note renders as a card: title + body at readable size, subtle shadow. **Closed cards are fully readable in place** — reading requires no clicks. Pan with scroll/middle-drag; zoom with Ctrl+scroll (gentle limits, zoom-to-fit available). A desk holds a *working set* — dozens of cards, not thousands (offscreen cards are culled; the Drawer is where everything lives). **Card positions persist per desk, forever** — your mess stays exactly where you left it, which *is* the "where was I?" answer.

**Editing.** Enter (or double-click) on a card opens **the editor window**: a floating modal card centered over the desk — drop shadow, clean chrome, card-sized. §5 describes the inside. Esc closes, always saving; `Ctrl+Enter` also closes a permanent card's editor, but on a scrap it promotes (§6) — "complete this card" in both cases. One editor open at a time. The window comfortably fits a healthy card (~150–200 words); its size is the note-size pedagogy.

**New cards.** `Ctrl+N` (and a fixed `+` button) drops a fresh fleeting scrap at the cursor and puts you straight into its editor: hotkey, type, Esc — done before the thought fades. The global capture hotkey (§12) feeds the same path without the main window appearing.

**Putting away vs deleting.** Backspace (or drag to the rail) **puts a card away** — off this desk, safely in the Drawer. Delete is a different, explicit act (Del → confirm for permanent cards → trash). The desk is never storage, so clearing it is always safe, and the affordances make that obvious.

**Connections are spatial.** Select or open a card and its edges materialize: lines to linked/linking cards already on the desk, plus a small fan of **ghost cards** at its edge — the strongest off-desk connections (links, backlinks, shared tags, text similarity), faded and small. Click a ghost to pull the real card onto the desk. This is the local connections view; there is no separate backlinks panel.

**Desks, plural.** The left rail lists named desks (creatable, renamable, reorderable), each remembering its cards, layout, and viewport — this is what tabs and workspaces collapse into: not forty documents, but a handful of *places*. **No tabs, anywhere, ever** — tab proliferation is executive-dysfunction flypaper. The rail also holds two special surfaces: the **Inbox** (§6) and the **Drawer** (§7), plus the **Map** (§8) and a quiet Trash entry.

**Keyboard-first.** Cards are real egui widgets in the AccessKit tree; focus order is spatial reading order; arrows move card focus, Enter opens, `Shift+F10` opens the card menu. The whole desk drives without a mouse — a hard requirement (§12), costed deliberately because canvas UIs don't get it for free.

**Session restore:** reopening the app restores desk, viewport, and open card exactly.

### 4.5 Card Visual Language

The card's *shape* teaches its role before you read a word:

- **Fleeting cards are scraps** — wider than tall, like paper torn off to catch a thought; no ruled lines; a subtly irregular top edge. Pleasantly *temporary*: a scrap wants to be processed or discarded, and the visual says so without a word.
- **Permanent cards are index cards** — classic 3×5 proportions, crisp corners, **optional ruled lines**: **None · Natural** (cream card, red header rule, faint blue rules) **· Ink** (faint luminous rules on a dark card). Default follows theme (Natural in light, Ink in dark). Lines are pure decoration — text never snaps to them.
- **Literature cards** (kind: literature) are index cards with a **citation footer** — the `source:` in a small strip at the bottom edge — plus a small corner bookmark mark.
- **Structure cards are divider cards** (kind: structure) — the tabbed divider from a card catalog: a tab on the top edge carrying the title. Reads as "this organizes the cards behind it." Links inside render as a clean list; when selected on a desk, its edges fan out to everything it references.

**Two visual styles** — the setting is **Card style: Paper / Plain** (Paper is the skeuomorphic treatment, and the default):
- **Paper**: torn edges, ruled lines (with the None/Natural/Ink sub-setting), cream tints, texture.
- **Plain**: flat cards — no tear, no rules, no tint. **Distinctions that carry meaning survive**: scrap proportions, the divider tab, the citation footer. Rule for all future features: *shape is semantic and universal; texture is Paper-only.*

---

## 5. The Editor

**Foundation.** egui `TextEdit` with a custom `layouter` — we keep its cursor, selection, IME, clipboard, and base editing machinery, and supply the styling. A line-oriented markdown lexer (ours) produces styled spans over raw source, cached per line and re-lexed only for edited regions; multi-line constructs (code fences) carry line-state forward so incremental re-lexing stays honest.

**The dialect** — styled, supported, and documented as the *complete* list. A fixed dialect is a feature: nothing silently unstyled, nothing to configure.

- Headings `#`–`###` (larger/bolder, marker dimmed).
- `**bold**`, `*italic*`, `~~strike~~`, `` `inline code` ``, fenced code blocks (monospace, shaded; **no syntax highlighting** in v1).
- Lists `-`, `1.`, and `- [ ]` tasks (rendered as clickable checkboxes).
- Blockquotes `>`.
- `[[Wikilinks]]` (accent, clickable; unresolved = dashed underline), `#tags` (pill tint), bare URLs and `[md](links)`.
- **Not in the dialect** (v1, deliberate): tables, footnotes, embeds/transclusion, math, HTML. Files containing them round-trip untouched and render as plain text.

**Editing behavior:** Enter continues lists/quotes (empty item ends the list); Tab/Shift-Tab indent/outdent items; `[[` opens link autocomplete (fuzzy over titles; a no-match offers "link as new card" → unresolved link); `#` offers tag autocomplete; pasting a URL over selected text makes a markdown link; pasting a bare URL into an empty card triggers the literature suggestion (§11). **No smart quotes/dashes — the file is source; we never rewrite what you typed.**

**Titles.** A card's title is its first `#` heading, edited in place; the filename follows it (§2 rename machinery). Scraps don't need titles — they list by first line. **The title is the thought**: not a label ("egui notes") but the claim itself ("Immediate mode trades layout power for state simplicity"), so linking to a card is citing its idea. The promotion mechanic (§6) makes this happen without saying it. (Historical note for the doc's honesty: Luhmann's cards had numbers, not titles; titles-as-claims is the digital-era adaptation, and it's what makes a desk of cards read as an argument.)

**Split** — `Ctrl+Shift+Enter` splits the open card at the cursor: text after the cursor becomes a new card (fleeting, or titled-permanent if it starts with a heading), a `[[link]]` to it replaces it at the split point, and both cards sit side by side on the desk. The long-card affordance (§11) is just the reminder of this command.

**Text undo/redo.** Per-card undo stacks, grouped at word/operation granularity, surviving card close/reopen within the session. Routing between this and the app stack: §9.

---

## 6. The Workflow: Inbox, Promotion, Lifecycle

**One rule defines the inbox: every fleeting card is in it.** No matter where a scrap was created, it appears in the Inbox until it stops being fleeting. There is no way to file, snooze, or hide a scrap while leaving it fleeting — that would be organizing the junk instead of processing it. (A scrap can *also* sit on a desk; inbox membership is a fact about status, not location.)

**Capture paths** — both land identically (new file in `inbox/`, `status: fleeting`, no title required); there is no third:
1. `Ctrl+N` anywhere in the app — scrap at cursor, editor open, already typing.
2. Global hotkey (where available, §12) — floating capture box; Enter saves, Shift+Enter adds a line, Esc discards; the main window never appears.

**The Inbox surface** is a self-arranging pile: **oldest scraps surface first** (the opposite of email — old thoughts don't sink), loosely scattered under Paper, a tidy column under Plain. Each scrap offers exactly three acts:

- **Promote** — see below; also available explicitly as `Ctrl+Enter`.
- **Toss** (`Del`, no confirmation for scraps — trash-backed, recoverable per retention setting). Guidance states it once: *"Not every scrap becomes a card. Toss freely."*
- **Take to a desk** (drag, or `Ctrl+D` → desk picker) — still fleeting, still inboxed, but now in context.

**Promotion is typing.** A scrap holds one line: the thought. Open it and the cursor lands at the end. **Pressing Enter there — starting a second line — is promotion.** The transformation happens in front of you: the first line restyles into the title (the claim), the scrap stretches from torn-paper into index-card proportions, and you're writing the body under the idea. Status and the `inbox/` → `notes/` file move commit when the editor closes; `Ctrl+Z` immediately after the Enter reverses the whole thing. The trigger is the *edit action* — a new line created in a fleeting card's editor — not line-count as a static property, so a multi-line capture stays fleeting until worked on. `Ctrl+Enter` (promote-without-typing, seeding the first line as title with the cursor on it) is the same code path. This is the pedagogy at its best: the app never says "your first line should be the idea" — the interaction makes it so, and the card faces across the desk prove it.

**After promotion — the connection step.** The fresh card shows its ghost fan (§4): nearest candidates by text similarity (cosine over existing search postings — no new machinery) plus shared tags. The banner states the practice: *"New cards earn their keep linked — Ctrl+K finds related cards."* An unlinked card is never an error and never nags; **Unlinked** exists as a Drawer view for when *you* go looking.

**Resurfacing, bounded.** The banner draws on inbox age, never counts-as-pressure, one suggestion at a time, rotating rather than repeating (§11). The quiet inbox count in the rail is the only number in the app.

**Status is one-way by default, reversible by intent:** demotion back to fleeting exists (Card menu, no shortcut) because undo must exist for everything — but no UI path encourages it.

---

## 7. The Drawer & Search

**`Ctrl+K` — the palette.** One overlay field, three strata rendered as one list:
1. **Title matches** — fuzzy, instant, ranked by match quality + recency.
2. **Full-text matches** — BM25 with highlighted snippets; prefix-matching on the final query term for search-as-you-type.
3. Always last: **"New scrap: '…'"** — the failure mode of search is capture, never a dead end.

Rows show the card's face-in-miniature cues (status shape, kind glyph, top tags). **Enter places the card on the current desk** at viewport center; **if it's already on this desk, the view pans/zooms to center it instead — the card never moves; the spatial layout is sacred** (brief highlight pulse, skipped under reduced motion). `Ctrl+Enter` places-and-opens. Esc dismisses.

**Fuzzy matching** (title stratum) is a scorer we own (~200 lines, fzf-style dynamic programming). Tiers: exact > prefix > **acronym** (query letters matching word-initials in order: `nasa` → *National Aeronautics and Space Administration*) > in-order subsequence; bonuses for consecutive runs and word-boundary hits, gap penalties. This layer is what makes the palette feel psychic; the ranking tables are pinned as tests (§13).

**Query syntax — small, fixed, documented in the empty palette itself:** plain words (AND), `"quoted phrases"`, `#tag`, `-word`. That's the whole language.

**The Drawer surface** — storage, for wandering rather than hunting. A dense grid of readable card minis (the same card widgets as the desk), newest-modified first, with one row of **filter chips**: status (scraps/cards), kind (notes/literature/dividers), a tag picker (all tags with counts), and the attention views — **Unlinked** and **Needs Attention** (parse quarantine, conflict copies). Chips compose and dismiss individually; the chip row *is* the current query, always visible. **No saved searches, no smart folders, no query builder.**

**Reading and acting in place.** Enter on a Drawer card opens the editor overlay right there — reading or editing never requires desk placement. `Ctrl+D` (or drag onto a desk name in the rail) takes it to a desk. The distinction stays crisp: the Drawer is where cards *live*; desks are where cards *work*.

**Trash** — bottom of the rail, quiet. Tossed scraps and deleted cards with their dates, one-click restore, purged per the retention setting (stated in the view, matter-of-factly). Manual empty via menu; nothing ever prompts about it.

---

## 8. The Map

The rail's last surface: every card in the vault as a **map** — dots and lines, force-directed. (Named "Map," not "Graph": it's for orienting, and that's the honest promise.)

- **Nodes** are cards: size scales gently with link degree; shape/tint follows the visual language (dividers slightly larger — they're hubs by nature). **Edges are links only** — no tag-similarity edges; they turn maps into felt.
- **Layout is ours**: spring-repulsion with a spatial grid for the repulsion pass — real-time to tens of thousands of nodes. It runs until settled, then **freezes** — a map, not a lava lamp. Positions cache in `.junkdrawer/` so the map is instant and *stable across sessions* — looking the same tomorrow is what makes spatial memory work on it. New cards ease in near their neighbors (fade, not physics-explosion; under reduced motion they appear settled).
- **Interactions mirror the Drawer**: hover shows the title, click selects and shows the card mini, Enter opens the editor overlay in place, `Ctrl+D` takes to desk, `Ctrl+K` works *within* the map — matches light up, everything else dims. Orphans ring the edge: findable, not shamed.
- **Equivalence rule (hard requirement):** nothing is learnable *only* from the map. Clusters ≈ tag/link browsing; orphans ≈ the Unlinked view. The map is a lens, never the sole path.

---

## 9. Undo/Redo

**Two stacks, one routing rule.** If an editor is open, `Ctrl+Z` is **text undo** (§5 per-card stacks). Otherwise it's the **app stack**: one global, session-long journal of structural operations — place/move/put-away on desks, toss, delete, restore, promote, demote, split, rename, tag edits made through UI affordances. Redo: `Ctrl+Shift+Z` (+ `Ctrl+Y` on Windows).

**Implementation**: command pattern in `jd-core` — every vault mutation is a `Command` with a computed inverse, executed by the single vault worker (§3's serialized writes make inverses race-free). ~200 ops held in memory per session. This isn't fragile, because every destructive endpoint is *independently* recoverable (trash for content, per-desk position history): undo is a convenience layer over a safety floor, not the safety floor itself.

**Undo and redo are legible.** The Edit menu names both — "Undo Toss scrap 'egui layouter idea'", "Redo Toss scrap '…'" — the status line echoes what happened, and if the change happened elsewhere (another desk), the view travels there so you *see* it. Silent undo is how users learn to distrust Ctrl+Z.

**Compound entry:** promotion-by-Enter spans text and status; it's one compound entry on the active (editor) stack, so a single Ctrl+Z reverses the whole transformation — matching what the user perceives as one action.

---

## 10. Menus, Settings, Shortcuts

**Menu bar.** macOS gets the native top-of-screen bar via `muda` (mac-only feature flag; integration spike in §14). Windows/Linux use egui's drawn in-window menu bar. Identical semantics everywhere:

- **File** — New Scrap · New Desk · Open Vault… · Recent Vaults ▸ · Export Desk as Outline… (cards in reading order, links resolved → clipboard or `.md`) · Settings · Quit. Multiple vaults supported, one per window.
- **Edit** — Undo *[named]* · Redo *[named]* · Cut/Copy/Paste · Split Card · Find (`Ctrl+K`).
- **Card** — Promote · Toss · Take to Desk ▸ · Put Away · Set Source… · Make Divider · Demote to Scrap · Copy Link (`[[Title]]` to clipboard) · Reveal in File Manager.
- **View** — Inbox / Drawer / Map / desks · Back · Forward · Zoom In/Out/Reset · Toggle Rail · Appearance ▸.
- **Help** — Keyboard Shortcuts (`Ctrl+/`) · User Guide · About. No tips-of-the-day machinery.

**Settings — one small dialog, three groups, ~a dozen items total (the ceiling is deliberate):**

- **General:** vault folder (create/switch) · global capture hotkey (enable + binding — the one remappable shortcut, since it lives among other apps' claims) · start in tray on login *(only on platforms with the `resident` feature)* · trash retention (7 / 30 / 90 days / manual only).
- **Appearance:** theme (System/Light/Dark) · card style (Paper/Plain) · ruled lines (None/Natural/Ink; enabled only under Paper) · UI scale · reduced motion (follow OS / on).
- **Guidance:** suggestion banner (on/off) · in-card practice reminders (on/off). Two switches, not a tuning console.

Storage: a flat, versioned, hand-parsed `key = value` file in the platform config dir (paths computed ourselves); per-vault overrides in `.junkdrawer/`. Unknown keys preserved on rewrite — same round-trip courtesy as frontmatter.

**Shortcuts: fixed in v1** (global hotkey is the sole remap). A remap UI is real scope; "one obvious way" spends that effort on excellent defaults instead: `Cmd` on macOS / `Ctrl` elsewhere, conflict-free, and every menu item and tooltip carries its shortcut so the app teaches its own keyboard layer passively. `Ctrl+/` overlays the cheat-sheet grouped by surface. Full table: Appendix A.

---

## 11. Guidance (consolidated)

**Voice — governs every string:** state the practice, name the action, assume competence. No questions, no offers, no praise, no exclamation marks, nothing modal, no animation to attract attention. Matter-of-fact, like a pencil note in the margin: *"Full cards work better split up — Ctrl+Shift+Enter between two sections."* Present but unobtrusive: most users should never feel the need to turn it off.

**Four surfaces, no others:**

1. **The banner** — one suggestion at a time, derived from vault state by an ordered rule list, first match wins:
   - empty vault → *"This drawer is empty — Ctrl+N catches whatever's in your head."*
   - aging inbox → *"Some scraps have waited a while — turn a few into cards."*
   - fresh unlinked cards → *"New cards earn their keep linked — Ctrl+K finds related ones."*
   - mature tag cluster without a divider → *"14 cards share #rust — a divider card could map them."*
   - otherwise → **empty; silence is a valid suggestion.**

   Clicking navigates to the work. A rule that fired won't re-fire within a cooldown measured in days. The banner is evaluated at surface-switch, never on a timer — it never changes while you watch it, and it never acknowledges whether you acted.
2. **Card-margin affordances** — detected-criterion suggestions: URL pasted into an empty card → *set as source*; card that's become mostly links → *make this a divider card*; long card → the split reminder. Rendered in the card footer: small, low-contrast, static. Dismissing one is permanent for that card.
3. **Empty states** — every surface explains itself when blank (new desk: *"Bring cards here with Ctrl+K, or start a scrap with Ctrl+N."*).
4. **Tooltips** — every control, always with its shortcut.

**State:** dismissals and cooldowns persist in `.junkdrawer/guidance` (disposable; worst case a suggestion repeats once). Settings switches map to surfaces 1 and 2; empty states and tooltips are just *the UI* and have no switch.

---

## 12. Accessibility, Platform Integration, Shipping

**Accessibility — requirements, not aspirations:**
- AccessKit (via eframe) with deliberate semantics: a card announces *"Card: 'Immediate mode trades layout power for state simplicity', 3 links, 2 tags"* — never "group." Scraps announce their first line.
- Every desk fully keyboard-traversable in spatial reading order (top-left → bottom-right, stable under small position changes); Enter opens; `Shift+F10`/context key for the card menu.
- **General law (the Map rule, §8, generalized): no information or action exists only in a spatial or visual form.**
- Reduced motion honored from the OS, overridable in Appearance: kills the pulse highlight, ease-ins, and texture animation — never the information.
- Contrast: both themes hit WCAG AA for text and affordances, *including guidance strings* (quiet ≠ illegible) — verified by automated palette checks in CI.
- VoiceOver + NVDA smoke pass is a release-gate checklist item (§13).

**Platform integration:**
- **Single instance per vault:** a second launch hands off over a local socket (hand-rolled: one named pipe/UDS, a 3-message protocol) and focuses the running window. `jd-app --capture` rides the same channel → capture popup from the running instance.
- **Global hotkey + tray** (`global-hotkey`, `tray-icon`) behind the `resident` cargo feature: full behavior on Windows/macOS/X11. **On Wayland** the hotkey setting hides, and the documented fallback — inline, where the setting would be — is binding a compositor shortcut to `jd-app --capture`. The global hotkey is a tiered feature by design: nothing load-bearing depends on it.
- Cmd/Ctrl mapping, native file dialogs (`rfd`), OS theme detection (eframe), IME correctness (inherited from `TextEdit` — a major reason we kept it).
- **Fonts bundled** (one UI/body face + one mono, e.g., Inter + JetBrains Mono): identical cards on every OS, ruled-line metrics ours. System-font fallback for uncovered scripts.

**Shipping:**
- macOS: universal binary, `.app` in `.dmg`, signed + notarized. Windows: NSIS installer + portable `.zip`, signed. Linux: AppImage (primary) + `.tar.gz`; Flathub later.
- CI: GitHub Actions matrix — fmt/clippy/test on all three platforms, release artifacts on tag.
- **Updates v1 = notification only:** manual-and-weekly check against the releases feed, surfaced as a quiet status-line/About note. No self-updating binary (an entire dependency tree and attack surface; revisit post-1.0).

---

## 13. Testing & Quality

**`jd-core` owns correctness:**
- **Parsers:** table-driven tests for the frontmatter mini-parser and markdown lexer, plus a **golden corpus** of foreign files (Obsidian-authored, weird-but-legal YAML, missing frontmatter, CRLF, BOM, emoji, RTL). Load-bearing invariant, tested exhaustively: **parse → serialize is byte-identical** for everything not deliberately changed.
- **Randomized round-trips** with a ~40-line xorshift generator (ours) producing adversarial bodies; asserts round-trip identity and lexer-span sanity (spans cover the line, never overlap, never split UTF-8). `cargo-fuzz` is an optional local tool, not in the tree.
- **Vault engine integration tests** on temp dirs: rename-rewrites-referrers, title collisions, conflict copies, trash lifecycle, **atomic-save torture** (kill between temp-write and rename; the original must always be intact), watcher debounce, and a zoo of "how editors save files" cases (rename-swap writers, etc.).
- **Undo:** for every `Command`, execute-then-inverse restores index state exactly.
- **Search:** fuzzy-scorer ranking tables (acronym cases pinned), BM25 sanity, palette-strata ordering.
- **Performance budgets as failing tests** on a synthetic 20k-note vault in CI: cold scan < 1 s (the tripwire that legally activates the §3 snapshot cache) · incremental reindex of one file < 5 ms · palette query < 10 ms. Budgets enforced by `#[test]`s so drift is a red build, not a slow decay.

**`jd-app`** tests through **`egui_kittest`** (drives the UI via the AccessKit tree — every UI test exercises accessibility semantics as a side effect). Scenarios: capture → inbox, Enter-promotes, split, put-away vs toss, palette placement + already-on-desk centering, undo/redo naming. **Snapshot tests** for card faces: scrap/card/literature/divider × Paper/Plain × three line styles.

**Release gate (manual checklist, in-repo):** VoiceOver + NVDA pass · keyboard-only full-workflow pass · platform-matrix smoke from the actual artifacts on fresh VMs · 50k-note vault open · reduced-motion sweep.

---

## 14. Build Order & Risks

**Milestones — each ends usable:**

| # | Deliverable |
|---|---|
| M0 | Workspace skeleton (`jd-core`, `jd-app`), CI green on all three platforms from day one |
| M1 | Vault engine headless: parse, index, watch, search, atomic writes — fully tested before a single pixel |
| M2 | One desk, real cards, styled-source editor in its floating window (the two riskiest UI pieces, proven early) |
| M3 | The workflow: capture, inbox pile, Enter-promotes, split, toss/trash, both undo stacks |
| M4 | Palette + Drawer + ghost-fan connections |
| M5 | The Map |
| M6 | Guidance, settings, menus, shortcut overlay |
| M7 | Platform residency (tray, hotkey, single-instance socket), accessibility hardening |
| M8 | Packaging, signing, update check, user guide |

**Risk register — each gets a front-loaded spike:**

1. **Mixed-size text in `TextEdit`'s layouter** — heading-vs-body sizing inside one editable galley is the styled-source bet. Spike in M2 week one. Fallback: uniform size, styling via weight/color only — degrades gracefully, doesn't kill the design.
2. **`muda` × eframe event loop on macOS** — known fiddly. Spike early in M6. Fallback: egui-drawn menu bar on mac for v1, explicitly temporary.
3. **AccessKit spatial focus order on a free-form canvas** — no framework precedent. Prototype in M2 *alongside* the desk, not after.
4. **Wayland** — hotkey fallback already designed; tray varies by DE; test on GNOME + KDE VMs in M7.
5. **Watcher edge cases** (rename-swap editors, network drives) — covered by the M1 editor-zoo integration tests.

---

## Appendix A — Keyboard Shortcuts (v1, fixed)

`Cmd` replaces `Ctrl` on macOS. Every menu item and tooltip carries its shortcut; `Ctrl+/` shows this table grouped by surface.

| Shortcut | Action | Scope |
|---|---|---|
| `Ctrl+N` | New scrap (editor opens, typing immediately) | Anywhere in-app |
| *(global hotkey)* | Capture popup (Enter saves · Shift+Enter newline · Esc discards) | System-wide, `resident` feature |
| `Ctrl+K` | Palette: find / jump / create | Anywhere in-app |
| `Enter` | Open focused card in editor / act on palette row | Desk · Drawer · Map · palette |
| `Ctrl+Enter` | Promote scrap (card focus or its editor) · close a card's editor · place-and-open from palette | Card focus · editor · palette |
| `Ctrl+Shift+Enter` | Split card at cursor | Editor |
| `Esc` | Close editor (saves) · dismiss overlay | Editor · overlays |
| `Backspace` | Put card away (off desk; never deletes) | Desk |
| `Del` | Toss scrap (no confirm) / delete card (confirm) → trash | Card focus |
| `Ctrl+D` | Take card to desk (picker) | Card focus, any surface |
| `Ctrl+[` / `Ctrl+]` | Back / Forward | Anywhere in-app |
| `Ctrl+Z` | Undo (editor open → text; else → app stack, named) | Per §9 routing |
| `Ctrl+Shift+Z` / `Ctrl+Y` | Redo (named) | Per §9 routing |
| `Ctrl+scroll` | Zoom desk/Map | Desk · Map |
| Arrow keys | Move card focus in spatial reading order | Desk · Drawer · Map |
| `Shift+F10` / context key | Card menu | Card focus |
| `Ctrl+/` | Shortcut overlay | Anywhere in-app |
| `Tab` / `Shift+Tab` | Indent / outdent list item | Editor |

## Appendix B — Dependency Policy: The Lists

**Approved crates (hard or platform-cursed problems):**
`eframe`/`egui` (glow backend) · `notify` (file watching) · `rfd` (native dialogs) · `muda` (macOS native menus; mac-only feature) · `global-hotkey` + `tray-icon` (`resident` feature) · `egui_kittest` (dev-dependency).

**Written ourselves (small, scope-controlled):**
ULID generation (~50 lines) · frontmatter mini-parser (fixed schema + unknown-key preservation) · markdown line lexer (styled spans; a different job than rendering parsers like `pulldown-cmark`) · fuzzy scorer (~200 lines, fzf-style DP) · inverted index + BM25 (~500 lines) · Map force layout (spring-repulsion + spatial grid) · settings `key = value` parser · config-dir path resolution · single-instance socket protocol · xorshift test generator.

**Explicitly rejected:** SQLite/`rusqlite` (Approach B, declined) · `tantivy` · `serde`/`serde_yaml` · YAML crates · graph-layout crates · auto-updater frameworks · `wgpu` backend (glow suffices).

Rule of thumb going forward: take the mature crate when the problem is hard or platform-specific; write it when it's small and we control the scope; and when in doubt, the tree stays tight.
