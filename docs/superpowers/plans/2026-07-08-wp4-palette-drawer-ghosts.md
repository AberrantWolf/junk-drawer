# WP4 — Palette, Drawer, Ghost Fan — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** M4 — the connection step: `Ctrl+K` palette (find/jump/create), the Drawer (storage for wandering), and the ghost fan + on-desk edges (local connections view).

**Architecture:** Same patterns as WP2/WP3: events-out surfaces, app.rs single mutation site, kittest everything through AccessKit. Search machinery is all WP1c (`fuzzy_match`, `Index::query`, `make_snippet`, `Index::similar`) — WP4 is UI over it. ONE sanctioned jd-core change (Task 3): `VaultEvent::ScanComplete` gains the quarantine list (shape-only event extension).

**Tech Stack:** unchanged. NO new dependencies.

## Global Constraints

- Dependency policy unchanged. jd-core changes ONLY the Task 3 event extension (+ its call sites/tests).
- Journal discipline (established): palette placement = journaled "Place card" via `place_card`; pan-to-existing = NOT journaled (navigation). Drawer take-to-desk reuses the WP3 composite path.
- **Spatial layout is sacred:** the palette NEVER moves an already-placed card — it pans/zooms to it.
- Single-writer, round-trip law, AccessKit-on-everything as before. `cargo fmt` last before every commit.
- Commit trailer: `Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>`
- SIGNING: 1Password is intermittently available; try normal signed commits with a ~45s expectation — if a commit hangs or fails twice, fall back to `--no-gpg-sign` and note it (batch re-sign pre-merge). Do NOT push (controller pushes).
- BRANCH GUARD: work on `feat/palette-drawer`. `git status -sb` before start AND commit; never checkout/switch; wrong → STOP, BLOCKED.

## Scope boundaries (WP4 does NOT include)

- Map (WP5). Guidance banner incl. the post-promotion "New cards earn their keep linked" banner text (WP6 owns banner rendering; the ghost fan itself IS WP4). Settings, menus beyond what exists (WP6). Saved searches/smart folders: never.
- The literature-suggestion margin hint on URL paste (WP6 guidance).

## Interfaces consumed (verified in-tree)

- `fuzzy_match(query, candidate) -> Option<FuzzyScore>` (index/fuzzy.rs); `Index::query(&Query, limit) -> Vec<SearchHit>` + `parse_query` (SearchHit carries matched terms, NOT snippets — decision §6.10); `make_snippet(body, terms, radius) -> Snippet` (search.rs:524) — bodies come from BodyCache (visible rows only); `Index::similar(id, k) -> Vec<(NoteId, f32)>` (cached norms).
- App: `place_card`, `apply_session`, BodyCache, `card_face`/FaceMeta, `reveal`, DeskCamera, editor open path, `execute_undo` conventions, rail `RailDropTarget`, `status_echo`.
- Read app.rs + surfaces/desk.rs + palette-relevant WP3 code FIRST in every task; follow the established idioms.

## File Structure

```
crates/jd-app/src/
├── palette.rs        # CREATE: Ctrl+K overlay — input, three strata, actions
├── surfaces/drawer.rs # CREATE (+ mod.rs line): mini grid + chips
├── surfaces/desk.rs  # MODIFY: ghost fan + edges-on-select
├── app.rs, state.rs  # MODIFY: palette state, quarantine/conflict tracking, routing
crates/jd-core/src/worker.rs  # MODIFY (Task 3 ONLY): ScanComplete carries quarantined list
crates/jd-app/tests/palette_kittest.rs, drawer_kittest.rs  # CREATE
```

---

### Task 1: Palette — overlay, three strata, rendering

**Files:** create `palette.rs`; modify `app.rs` (Ctrl+K opens; overlay renders above surfaces; gates: not while editor/confirm open), `state.rs` (`palette: Option<PaletteState>`); create `tests/palette_kittest.rs`.

- `PaletteState { query: String, selected: usize, results: Vec<PaletteRow> }`; `PaletteRow::{Title{id, score}, Body{id, snippet}, NewScrap}`. Recompute results on query change (cheap at our scale; recompute in the palette fn under one index read lock).
- **Stratum 1** — fuzzy over titles (skip untitled scraps), ranked by `FuzzyScore` + recency tiebreak (modified desc); cap ~8.
- **Stratum 2** — `Index::query(parse_query(&query), ~8)`, snippet per row via `make_snippet(body, hit.matched_terms, ~30)` for rows whose body is cached (`get_or_request`; blank snippet while loading — same discipline as faces). Dedupe ids already in stratum 1.
- **Stratum 3** — always-last `New scrap: '<query>'` (when query nonempty).
- Row rendering: miniature face cues — status shape glyph (scrap vs card silhouette — small painted rect using shape.rs metrics at ~20% scale or a simple glyph; keep it cheap), kind glyph (literature/divider markers), top 2 tags. Selected row highlighted; Up/Down move; all rows AccessKit-labeled (`"Result: '<title>'"` / `"New scrap: '<query>'"`).
- **Empty palette** (no query): show the query syntax help verbatim: `plain words (AND) · "quoted phrases" · #tag · -word`.
- Esc dismisses (palette only — gate like other overlays; palette suppresses surface keys while open, same confirm_pending pattern). Ctrl+K toggles.
- Kittest: open palette, type; strata ordering pinned (a title-match note, a body-only-match note, NewScrap last); empty state shows syntax; Esc closes; surface keys suppressed while open.

### Task 2: Palette actions — place / pan-to-existing / place-and-open

**Files:** modify `palette.rs`, `app.rs`; extend `tests/palette_kittest.rs`.

- Enter on Title/Body row: if id NOT on current desk → `place_card(current_desk, id, viewport_center)` (journaled "Place card") + palette closes. If ALREADY on this desk → pan/zoom to center it (`reveal`-style; NOT journaled; the card does not move) + **highlight pulse** (~600ms fading ring; skipped when reduced_motion — the `reduced_motion` flag is plumbed in EditorDeps already, reuse the source of truth) + palette closes.
- `Ctrl+Enter` = place (or pan) AND open the editor (session.open_card path).
- Enter on NewScrap: dispatch the Ctrl+N create path with the query as the initial body (`pending_create` w/ open_editor; seed body = query text — check NewNote shape; the scrap lands in inbox/ AND places at viewport center like Ctrl+N).
- If the current surface is not a desk (palette from Inbox/Drawer/Trash): place targets the first desk and SWITCHES to it (navigation + placement — placement journaled, switch not), echo mentions the desk (reuse the WP3 split-fallback idiom).
- Kittest: place → on desk at center + journaled; palette on already-placed card → position UNCHANGED + viewport centered on it (assert viewport.center ≈ card pos) + no new journal entry; Ctrl+Enter → also open_card set; NewScrap → fleeting note in inbox with the query as body, editor open.

### Task 3: jd-core event extension + Needs-Attention plumbing

**Files:** modify `crates/jd-core/src/worker.rs` (+ any type in vault/scan.rs re-exported), jd-core tests touching ScanComplete; modify `state.rs` (app-side tracking).

- `VaultEvent::ScanComplete { quarantined_count: usize }` → `{ quarantined: Vec<QuarantinedFile> }` (or add the vec alongside the count — pick the minimal churn; QuarantinedFile { rel_path, reason } already exists in scan.rs — make it public/cloneable as needed). Update all constructors + jd-core tests + jd-app drain_events (store `state.quarantined: Vec<QuarantinedFile>`; count derives).
- Conflict tracking app-side: `state.conflicts: Vec<NoteId>` appended on `VaultEvent::Conflict { id, .. }` (dedup; cleared per-id when that note is next saved by the user — simplest: leave the list session-scoped and document).
- Inverse-law + vault_worker suites stay green. This is the ONLY jd-core change in WP4.

### Task 4: The Drawer

**Files:** create `surfaces/drawer.rs` (+ mod.rs); modify `app.rs` (route SurfaceId::Drawer, replace placeholder; rail Drawer row already navigates); create `tests/drawer_kittest.rs`.

- Dense grid of card minis (reuse `card_face` at reduced size — pass a scaled rect; faces stay readable), **newest-modified first** (index meta modified desc).
- **Filter chips row** (the row IS the query, always visible): status (Scraps/Cards), kind (Notes/Literature/Dividers), tag picker (popup listing all tags with counts — `Index` tag map; check the API for tags+counts), **Unlinked** (no outgoing links AND no backlinks — compute from index adjacency), **Needs Attention** (Task 3's quarantined + conflicts; quarantined rows render as inert rows with the reason — they're not in the index, so no face; label `"Quarantined: '<filename>' — <reason>"`). Chips compose (AND) and dismiss individually (each chip is a toggle button with an × affordance; AccessKit-labeled with active state).
- Enter on a mini opens the editor in place (same open path; works from Drawer). `Ctrl+D` → desk picker (reuse the inbox picker component — extract it if needed); drag-to-rail desk rows already works via the WP3 gesture (rail hits are global — verify it works from the Drawer surface; if the drag originates in drawer grid, wire the same release hit-test or defer with a note).
- Keyboard: linear focus over the grid (row-major), same key discipline (gated on overlays).
- Kittest: newest-modified ordering; chip composition (status+tag AND); Unlinked chip (linked note excluded, unlinked included); Needs Attention (seed a quarantined file by writing garbage bytes pre-scan → row with reason present); Enter opens editor; chip dismiss restores full grid.

### Task 5: Ghost fan + edges-on-select + M4 scenarios + docs

**Files:** modify `surfaces/desk.rs`, `app.rs`; extend kittest; architecture doc.

- **Ranking** (architecture §3, pinned as a unit test): for the selected/open card, score every OFF-desk note: direct link = 3.0, backlink = 2.5, shared tag = 1.0 each, plus `Index::similar` cosine (0–1). Top k=5. Pure fn `ghost_candidates(index_meta_view, id, on_desk: &HashSet<NoteId>) -> Vec<(NoteId, f32)>` — unit-test the weights with a hand-built fixture (direct-link beats backlink beats two-shared-tags... pin ordering).
- **Render**: small faded minis (reuse card_face at ~40% scale w/ reduced opacity — check painter alpha; or tinted rect + title if alpha-on-galley is awkward — document) fanned at the selected card's nearest free edge (pick the edge with most free space among N/E/S/W of the card in world space; simple heuristic, document). Click a ghost → `place_card(desk, id, fan-position-world)` (journaled) — it becomes a real card where it stood. Ghosts have AccessKit labels (`"Ghost: '<title>'"`) but do NOT join the arrow-key reading order (they're previews; spec's no-spatial-only law is satisfied by the palette/drawer being keyboard paths to the same cards — note this in the code comment).
- **Edges-on-select**: when a card is selected (focused) or open, draw lines from it to linked/linking cards ON the desk (adjacency from index; both directions; subtle stroke from theme; drawn beneath cards). No edges when nothing selected.
- **M4 scenario kittest**: promote a card (WP3 path) → select it on desk → ghost fan shows the most-connected off-desk note first (fixture with a direct link) → click ghost → it places and joins the desk → edges now drawn between the two (assert edge count via a testable seam — expose the computed edge list from desk_ui deps or a pure fn, don't screenshot).
- **Docs**: architecture doc decision §6.16 (ghost ranking weights + fan edge heuristic + ghosts-not-in-focus-order rationale); WP4→WP5 handoffs block (whatever accumulates); mark WP3→WP4 items addressed where they were (inbox checkbox toggle if done — it is NOT in WP4 scope, leave).
- Full check: cargo test --workspace, clippy -D warnings, fmt --check, jd-core diff vs main = Task 3 files only.

---

## Verification (WP definition of done)

- All 5 tasks green locally; CI validation at PR time.
- M4 bar (spec §14): "Palette + Drawer + ghost-fan connections."
- Palette never moves a placed card (test-pinned). Chip row is the whole query language for the Drawer. Ghost weights pinned. No new deps; jd-core delta = Task 3 only.
