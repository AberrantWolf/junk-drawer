# WP1c — Index, Search, Fuzzy Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Implement `jd-core`'s in-memory index: fuzzy title scorer, inverted search index with BM25 + query language + snippets + similarity, and the `Index` façade with link/tag/title bookkeeping — per architecture doc §2.9 (as amended by decisions §6.10–12) and spec §3/§7.

**Architecture:** Three files under `crates/jd-core/src/index/`: `fuzzy.rs` (self-contained scorer), `search.rs` (tokenizer, postings, BM25, query parser, `make_snippet`, cosine similarity), `mod.rs` (the `Index` struct wiring notes/titles/links/tags around `SearchIndex`). Bodies are NOT stored (spec §3); `SearchHit` carries matched terms, and `make_snippet` is a pure helper the app calls with bodies it loads itself (decision §6.10).

**Tech Stack:** Rust stable, std only. Branch: `feat/index` (worktree; parallel to WP1b on `feat/lexer`).

## Global Constraints

- **Zero new dependencies in `jd-core`** (`tantivy` explicitly rejected).
- Every commit leaves `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace` green.
- Public signatures match architecture doc §2.9 exactly (with §6.10's `SearchHit { id, score, matched_terms }` and `make_snippet`).
- **Touch ONLY `crates/jd-core/src/index/` (new directory), `crates/jd-core/tests/index_integration.rs`, and the one `pub mod index;` line in `lib.rs`** — WP1b runs in parallel on other files; anything else you touch is a merge conflict.
- Query language is small and fixed (spec §7): plain words (AND), `"quoted phrases"`, `#tag`, `-word`; the final bare term is prefix-matched. That's the whole grammar.
- Postings positions are TOKEN indices, not byte offsets (decision §6.11).
- Title collisions: `titles` maps to the most recently upserted holder (decision §6.12).
- Perf budgets (query < 10 ms @ 20k) are WP1d's CI tests — don't add them here, but don't write anything obviously super-linear per query either.
- TDD: failing test first; RED evidence is a deliverable.

---

### Task 1: `index/fuzzy.rs` — the title scorer

**Files:**
- Create: `crates/jd-core/src/index/mod.rs` (for now just `pub mod fuzzy;` + `pub mod search;` stub comes later — start with `pub mod fuzzy;` only)
- Create: `crates/jd-core/src/index/fuzzy.rs`
- Modify: `crates/jd-core/src/lib.rs` (add `pub mod index;`)

**Interfaces:**
- Consumes: nothing.
- Produces: `fuzzy_match(query, candidate) -> Option<FuzzyScore>`, `FuzzyScore { tier, score, matched }`, `FuzzyTier { Exact, Prefix, Acronym, Subsequence }`. **Ordering contract:** lower tier = better; within a tier, higher score = better. Consumers sort by `(tier, Reverse(score))`. `matched` holds candidate CHAR indices (for the palette's highlight rendering).

- [ ] **Step 1: Write the failing tests** (in `fuzzy.rs`'s test module)

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn tier(q: &str, c: &str) -> FuzzyTier {
        fuzzy_match(q, c).expect("should match").tier
    }

    #[test]
    fn tiers() {
        assert_eq!(tier("egui", "egui"), FuzzyTier::Exact);
        assert_eq!(tier("Egui", "egui"), FuzzyTier::Exact); // case-insensitive
        assert_eq!(tier("egui", "egui tradeoffs"), FuzzyTier::Prefix);
        assert_eq!(
            tier("nasa", "National Aeronautics and Space Administration"),
            FuzzyTier::Acronym
        );
        assert_eq!(tier("etf", "egui tradeoffs"), FuzzyTier::Subsequence);
    }

    #[test]
    fn tier_ordering_is_the_ranking() {
        assert!(FuzzyTier::Exact < FuzzyTier::Prefix);
        assert!(FuzzyTier::Prefix < FuzzyTier::Acronym);
        assert!(FuzzyTier::Acronym < FuzzyTier::Subsequence);
    }

    #[test]
    fn acronym_beats_subsequence_ranking_table() {
        // The spec's pinned example: 'nasa' must rank the acronym candidate
        // above candidates that merely contain the letters in order.
        let a = fuzzy_match("nasa", "National Aeronautics and Space Administration").unwrap();
        assert_eq!(a.tier, FuzzyTier::Acronym);
        // a true subsequence candidate (word initials f/n/a — NOT an acronym for nasa):
        let c = fuzzy_match("nasa", "front nasal anatomy").unwrap();
        assert_eq!(c.tier, FuzzyTier::Subsequence);
        assert!(a.tier < c.tier);
        // and a prefix candidate outranks the acronym
        let b = fuzzy_match("nasa", "nasal decongestants and their history").unwrap();
        assert_eq!(b.tier, FuzzyTier::Prefix);
        assert!(b.tier < a.tier);
    }

    #[test]
    fn consecutive_run_beats_scattered() {
        let tight = fuzzy_match("abc", "xx abcdef").unwrap();
        let scattered = fuzzy_match("abc", "xx a1b2c3").unwrap();
        assert_eq!(tight.tier, FuzzyTier::Subsequence);
        assert_eq!(scattered.tier, FuzzyTier::Subsequence);
        assert!(tight.score > scattered.score, "consecutive bonus must dominate");
    }

    #[test]
    fn word_boundary_hits_score_higher() {
        let boundary = fuzzy_match("st", "note structure").unwrap(); // 's' starts a word
        let inside = fuzzy_match("st", "faster").unwrap(); // 'st' interior only
        assert_eq!(boundary.tier, FuzzyTier::Subsequence);
        assert_eq!(inside.tier, FuzzyTier::Subsequence);
        assert!(boundary.score > inside.score);
    }

    #[test]
    fn matched_indices_are_char_indices() {
        let m = fuzzy_match("nasa", "National Aeronautics and Space Administration").unwrap();
        assert_eq!(m.matched, vec![0, 9, 25, 31]); // N, A(eronautics), S(pace), A(dministration)
        let m = fuzzy_match("ab", "áxab").unwrap(); // multibyte before the match
        assert_eq!(m.matched, vec![2, 3]); // CHAR indices, not bytes
    }

    #[test]
    fn no_match_and_edge_cases() {
        assert!(fuzzy_match("xyz", "egui tradeoffs").is_none());
        assert!(fuzzy_match("", "anything").is_none());
        assert!(fuzzy_match("longer", "log").is_none()); // query longer than candidate
    }
}
```

(While transcribing `word_boundary_hits_score_higher`, clean it to exactly two candidates: `boundary` = `("st", "note structure")`, `inside` = `("st", "faster")`, assert `boundary.score > inside.score`. The intermediate lines above are noise from drafting — do not include shadowed unused variables.)

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p jd-core fuzzy`
Expected: compile error — `fuzzy_match` not defined.

- [ ] **Step 3: Implement**

```rust
//! Fuzzy title scorer for the palette (spec §7): fzf-style, with a distinct
//! acronym tier. Written in-house per Appendix B. Ordering contract: lower
//! tier is better; within a tier, higher score is better — sort by
//! (tier, Reverse(score)).

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum FuzzyTier {
    Exact,
    Prefix,
    Acronym,
    Subsequence,
}

#[derive(Clone, PartialEq, Debug)]
pub struct FuzzyScore {
    pub tier: FuzzyTier,
    pub score: i32,
    /// Candidate CHAR indices that matched (for highlight rendering).
    pub matched: Vec<usize>,
}

const WORD_BOUNDARY_BONUS: i32 = 10;
const CONSECUTIVE_BONUS: i32 = 8;
const GAP_PENALTY_CAP: i32 = 10;

pub fn fuzzy_match(query: &str, candidate: &str) -> Option<FuzzyScore> {
    if query.is_empty() {
        return None;
    }
    let q: Vec<char> = query.chars().flat_map(char::to_lowercase).collect();

    // Candidate chars, lowercased 1:1 (multi-char lowercase expansions keep
    // only the first char — an accepted approximation for titles).
    let mut chars: Vec<char> = Vec::new();
    let mut word_start: Vec<bool> = Vec::new();
    let mut prev: Option<char> = None;
    for ch in candidate.chars() {
        chars.push(ch.to_lowercase().next().unwrap_or(ch));
        word_start.push(ch.is_alphanumeric() && prev.is_none_or(|p| !p.is_alphanumeric()));
        prev = Some(ch);
    }
    if q.len() > chars.len() {
        return None;
    }

    if chars.len() == q.len() && chars == q {
        return Some(FuzzyScore { tier: FuzzyTier::Exact, score: i32::MAX, matched: (0..q.len()).collect() });
    }
    if chars[..q.len()] == q[..] {
        let score = (q.len() as i32) * 100 / chars.len() as i32;
        return Some(FuzzyScore { tier: FuzzyTier::Prefix, score, matched: (0..q.len()).collect() });
    }

    // Acronym: every query char consumed at a word start, in order.
    let mut matched = Vec::with_capacity(q.len());
    let mut qi = 0;
    for (i, (&ch, &ws)) in chars.iter().zip(&word_start).enumerate() {
        if qi < q.len() && ws && ch == q[qi] {
            matched.push(i);
            qi += 1;
        }
    }
    if qi == q.len() {
        // earlier completion = tighter acronym = better
        let score = -(matched[q.len() - 1] as i32);
        return Some(FuzzyScore { tier: FuzzyTier::Acronym, score, matched });
    }

    // In-order subsequence (greedy), scored by boundary/consecutive bonuses
    // minus capped gap penalties and start distance.
    let mut matched = Vec::with_capacity(q.len());
    let mut qi = 0;
    for (i, &ch) in chars.iter().enumerate() {
        if qi < q.len() && ch == q[qi] {
            matched.push(i);
            qi += 1;
        }
    }
    if qi < q.len() {
        return None;
    }
    let mut score = 0i32;
    for (k, &i) in matched.iter().enumerate() {
        if word_start[i] {
            score += WORD_BOUNDARY_BONUS;
        }
        if k > 0 {
            let gap = (i - matched[k - 1] - 1) as i32;
            if gap == 0 {
                score += CONSECUTIVE_BONUS;
            } else {
                score -= gap.min(GAP_PENALTY_CAP);
            }
        }
    }
    score -= matched[0] as i32;
    Some(FuzzyScore { tier: FuzzyTier::Subsequence, score, matched })
}
```

`index/mod.rs` for now:

```rust
//! In-memory index: fuzzy scorer, search engine, and the Index façade.
pub mod fuzzy;
```

Add `pub mod index;` to `lib.rs`.

- [ ] **Step 4: Run to verify pass**

Run: `cargo test -p jd-core fuzzy`
Expected: 7 passed. Verify the pinned `matched` indices by hand if they fail: "National Aeronautics and Space Administration" — `N`=0, `A`=9, `S`=25, `A`=31 (char indices; count them, don't trust the failure blindly — but if your count agrees with the test, fix the implementation).

- [ ] **Step 5: Full gate, then commit**

```bash
cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace
git add crates/jd-core
git commit -m "feat(core): fuzzy title scorer with acronym tier"
```

---

### Task 2: `index/search.rs` — tokenizer, postings, query parser, BM25

**Files:**
- Create: `crates/jd-core/src/index/search.rs`
- Modify: `crates/jd-core/src/index/mod.rs` (add `pub mod search;`)

**Interfaces:**
- Consumes: `crate::id::NoteId`, `crate::tag::Tag`.
- Produces: `tokenize(text) -> Vec<(String, u32)>`, `Query { terms, phrases, tags, negated, prefix_last }` (fields `pub`), `parse_query(&str) -> Query`, `SearchHit { id, score, matched_terms }`, `SearchIndex::{new, add_doc, remove_doc, query(q, limit, filter) -> Vec<SearchHit>, len}`. Task 4's `Index` passes tag-filtered candidates via `filter: Option<&HashSet<NoteId>>`.

- [ ] **Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::id::NoteId;

    fn nid(n: u8) -> NoteId {
        NoteId([n; 16])
    }

    fn q(input: &str) -> Query {
        parse_query(input)
    }

    #[test]
    fn tokenizer_lowercases_and_positions() {
        assert_eq!(
            tokenize("Hello, wörld! [[Link]]"),
            vec![
                ("hello".to_owned(), 0),
                ("wörld".to_owned(), 1),
                ("link".to_owned(), 2),
            ]
        );
        assert!(tokenize("").is_empty());
    }

    #[test]
    fn query_parser_full_grammar() {
        let parsed = q("rust \"smart notes\" #method -draft egui");
        assert_eq!(parsed.terms, vec!["rust", "egui"]);
        assert_eq!(parsed.phrases, vec![vec!["smart".to_owned(), "notes".to_owned()]]);
        assert_eq!(parsed.tags.len(), 1);
        assert_eq!(parsed.tags[0].as_str(), "method");
        assert_eq!(parsed.negated, vec!["draft"]);
        assert!(parsed.prefix_last, "last bare term is prefix-matched");
    }

    #[test]
    fn query_parser_edge_cases() {
        assert!(q("").terms.is_empty());
        let only_tag = q("#rust");
        assert!(only_tag.terms.is_empty() && !only_tag.prefix_last);
        let unclosed = q("\"unclosed phrase");
        assert_eq!(unclosed.phrases, vec![vec!["unclosed".to_owned(), "phrase".to_owned()]]);
        assert_eq!(q("-").terms, Vec::<String>::new()); // bare '-' ignored
    }

    fn small_index() -> SearchIndex {
        let mut s = SearchIndex::new();
        s.add_doc(nid(1), "the quick brown fox jumps over the lazy dog");
        s.add_doc(nid(2), "quick notes about rust programming and rust macros");
        s.add_doc(nid(3), "a slow brown bear eats quick berries");
        s
    }

    #[test]
    fn and_semantics_and_ranking() {
        let s = small_index();
        let hits = s.query(&q("quick brown"), 10, None);
        let ids: Vec<NoteId> = hits.iter().map(|h| h.id).collect();
        assert_eq!(ids.len(), 2); // docs 1 and 3 have both; doc 2 lacks 'brown'
        assert!(ids.contains(&nid(1)) && ids.contains(&nid(3)));
        // matched terms reported
        assert!(hits[0].matched_terms.contains(&"quick".to_owned()));
    }

    #[test]
    fn repeated_term_scores_higher() {
        let s = small_index();
        let hits = s.query(&q("rust "), 10, None); // "rust" prefix-expands only to itself here
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].id, nid(2));
        // and tf matters vs a one-occurrence doc:
        let mut s2 = SearchIndex::new();
        s2.add_doc(nid(1), "rust once here");
        s2.add_doc(nid(2), "rust and rust again rust");
        let hits = s2.query(&q("rust "), 10, None);
        assert_eq!(hits[0].id, nid(2));
        assert!(hits[0].score > hits[1].score);
    }

    #[test]
    fn prefix_on_last_term() {
        let s = small_index();
        let hits = s.query(&q("prog"), 10, None);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].id, nid(2));
        assert!(hits[0].matched_terms.contains(&"programming".to_owned()));
    }

    #[test]
    fn phrases_verify_adjacency() {
        let s = small_index();
        assert_eq!(s.query(&q("\"brown fox\""), 10, None).len(), 1);
        assert_eq!(s.query(&q("\"fox brown\""), 10, None).len(), 0);
        assert_eq!(s.query(&q("\"quick berries\""), 10, None)[0].id, nid(3));
    }

    #[test]
    fn negation_excludes() {
        let s = small_index();
        let hits = s.query(&q("quick -fox"), 10, None);
        let ids: Vec<NoteId> = hits.iter().map(|h| h.id).collect();
        assert!(!ids.contains(&nid(1)));
        assert_eq!(ids.len(), 2);
    }

    #[test]
    fn filter_restricts_candidates() {
        let s = small_index();
        let only3: std::collections::HashSet<NoteId> = [nid(3)].into();
        let hits = s.query(&q("quick"), 10, Some(&only3));
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].id, nid(3));
    }

    #[test]
    fn remove_doc_forgets() {
        let mut s = small_index();
        s.remove_doc(nid(2));
        assert!(s.query(&q("rust "), 10, None).is_empty());
        assert_eq!(s.len(), 2);
        // re-add works
        s.add_doc(nid(2), "rust returns");
        assert_eq!(s.query(&q("rust "), 10, None).len(), 1);
    }

    #[test]
    fn limit_is_respected() {
        let s = small_index();
        assert_eq!(s.query(&q("quick"), 2, None).len(), 2);
        assert_eq!(s.query(&q("quick"), 10, None).len(), 3);
    }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p jd-core search`
Expected: compile error.

- [ ] **Step 3: Implement**

```rust
//! Inverted search index with BM25 (spec §7). Positions are token indices
//! (decision §6.11). ~500 lines was the spec's budget; keep it lean.
//! tantivy explicitly rejected (Appendix B).

use std::collections::{BTreeMap, HashMap, HashSet};

use crate::id::NoteId;
use crate::tag::Tag;

const K1: f32 = 1.2;
const B: f32 = 0.75;
/// Bound on prefix expansions of the final query term (search-as-you-type).
const MAX_PREFIX_EXPANSIONS: usize = 64;

/// Lowercased alphanumeric runs with their token index.
pub fn tokenize(text: &str) -> Vec<(String, u32)> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut idx = 0u32;
    for ch in text.chars().chain(std::iter::once(' ')) {
        if ch.is_alphanumeric() {
            cur.extend(ch.to_lowercase());
        } else if !cur.is_empty() {
            out.push((std::mem::take(&mut cur), idx));
            idx += 1;
        }
    }
    out
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Query {
    pub terms: Vec<String>,
    pub phrases: Vec<Vec<String>>,
    pub tags: Vec<Tag>,
    pub negated: Vec<String>,
    /// The LAST bare term (if any) is prefix-matched (search-as-you-type).
    pub prefix_last: bool,
}

/// The whole grammar: words (AND) · "phrases" · #tag · -word.
pub fn parse_query(input: &str) -> Query {
    let mut q = Query::default();
    let mut rest = input.trim();
    while !rest.is_empty() {
        if let Some(after) = rest.strip_prefix('"') {
            let (phrase, tail) = match after.find('"') {
                Some(i) => (&after[..i], &after[i + 1..]),
                None => (after, ""),
            };
            let words: Vec<String> = tokenize(phrase).into_iter().map(|(t, _)| t).collect();
            if !words.is_empty() {
                q.phrases.push(words);
            }
            rest = tail.trim_start();
            continue;
        }
        let word_end = rest.find(char::is_whitespace).unwrap_or(rest.len());
        let word = &rest[..word_end];
        rest = rest[word_end..].trim_start();
        if let Some(tag) = word.strip_prefix('#') {
            if let Some(t) = Tag::new(tag) {
                q.tags.push(t);
            }
        } else if let Some(neg) = word.strip_prefix('-') {
            if let Some((t, _)) = tokenize(neg).into_iter().next() {
                q.negated.push(t);
            }
        } else if let Some((t, _)) = tokenize(word).into_iter().next() {
            q.terms.push(t);
        }
    }
    q.prefix_last = !q.terms.is_empty();
    q
}

#[derive(Clone, Debug)]
pub struct SearchHit {
    pub id: NoteId,
    pub score: f32,
    /// Lowercased terms that hit, incl. prefix expansions and phrase words —
    /// input for make_snippet (decision §6.10).
    pub matched_terms: Vec<String>,
}

#[derive(Default)]
pub struct SearchIndex {
    /// term → doc → token positions. BTreeMap so prefix expansion is a range scan.
    terms: BTreeMap<String, HashMap<NoteId, Vec<u32>>>,
    /// doc → (term, tf) — needed for remove_doc and cosine similarity.
    doc_terms: HashMap<NoteId, Vec<(String, u32)>>,
    doc_len: HashMap<NoteId, u32>,
    /// Sum of doc_len values — keeps avg doc length O(1) for BM25.
    total_len: u64,
}

impl SearchIndex {
    pub fn new() -> SearchIndex {
        SearchIndex::default()
    }

    pub fn len(&self) -> usize {
        self.doc_len.len()
    }

    pub fn is_empty(&self) -> bool {
        self.doc_len.is_empty()
    }

    pub fn add_doc(&mut self, id: NoteId, text: &str) {
        self.remove_doc(id);
        let tokens = tokenize(text);
        self.total_len += tokens.len() as u64;
        self.doc_len.insert(id, tokens.len() as u32);
        let mut tf: HashMap<String, (u32, Vec<u32>)> = HashMap::new();
        for (term, pos) in tokens {
            let e = tf.entry(term).or_default();
            e.0 += 1;
            e.1.push(pos);
        }
        let mut per_doc = Vec::with_capacity(tf.len());
        for (term, (count, positions)) in tf {
            self.terms.entry(term.clone()).or_default().insert(id, positions);
            per_doc.push((term, count));
        }
        self.doc_terms.insert(id, per_doc);
    }

    pub fn remove_doc(&mut self, id: NoteId) {
        let Some(old) = self.doc_terms.remove(&id) else { return };
        for (term, _) in old {
            if let Some(posts) = self.terms.get_mut(&term) {
                posts.remove(&id);
                if posts.is_empty() {
                    self.terms.remove(&term);
                }
            }
        }
        if let Some(len) = self.doc_len.remove(&id) {
            self.total_len -= len as u64;
        }
    }

    fn idf(&self, df: usize) -> f32 {
        let n = self.doc_len.len() as f32;
        ((n - df as f32 + 0.5) / (df as f32 + 0.5) + 1.0).ln()
    }

    fn bm25(&self, positions: usize, df: usize, doc_len: u32) -> f32 {
        let avg = self.total_len as f32 / self.doc_len.len().max(1) as f32;
        let tf = positions as f32;
        self.idf(df) * tf * (K1 + 1.0) / (tf + K1 * (1.0 - B + B * doc_len as f32 / avg.max(1.0)))
    }

    /// One required group per query term: the exact term, or (for the last
    /// term when prefix_last) all terms sharing the prefix. Docs must satisfy
    /// every group AND every phrase, and none of the negated terms.
    pub fn query(
        &self,
        q: &Query,
        limit: usize,
        filter: Option<&HashSet<NoteId>>,
    ) -> Vec<SearchHit> {
        // Build groups: Vec<Vec<&str expansions>>
        let mut groups: Vec<Vec<String>> = Vec::new();
        for (i, term) in q.terms.iter().enumerate() {
            if q.prefix_last && i == q.terms.len() - 1 {
                let expansions: Vec<String> = self
                    .terms
                    .range(term.clone()..)
                    .take_while(|(t, _)| t.starts_with(term.as_str()))
                    .take(MAX_PREFIX_EXPANSIONS)
                    .map(|(t, _)| t.clone())
                    .collect();
                if expansions.is_empty() {
                    return Vec::new();
                }
                groups.push(expansions);
            } else {
                if !self.terms.contains_key(term) {
                    return Vec::new();
                }
                groups.push(vec![term.clone()]);
            }
        }
        for phrase in &q.phrases {
            for w in phrase {
                if !self.terms.contains_key(w) {
                    return Vec::new();
                }
                groups.push(vec![w.clone()]);
            }
        }
        if groups.is_empty() {
            return Vec::new(); // tag-only queries are handled by Index (Task 4)
        }

        // Candidates: intersection over groups (union within a group).
        let mut candidates: Option<HashSet<NoteId>> = None;
        for group in &groups {
            let mut docs = HashSet::new();
            for term in group {
                if let Some(posts) = self.terms.get(term) {
                    docs.extend(posts.keys().copied());
                }
            }
            candidates = Some(match candidates {
                None => docs,
                Some(prev) => prev.intersection(&docs).copied().collect(),
            });
        }
        let mut candidates = candidates.unwrap_or_default();
        if let Some(f) = filter {
            candidates.retain(|id| f.contains(id));
        }
        for neg in &q.negated {
            if let Some(posts) = self.terms.get(neg) {
                candidates.retain(|id| !posts.contains_key(id));
            }
        }
        candidates.retain(|id| q.phrases.iter().all(|p| self.phrase_matches(p, *id)));

        // Score: per group, the best-scoring expansion counts.
        let mut hits: Vec<SearchHit> = candidates
            .into_iter()
            .map(|id| {
                let doc_len = self.doc_len[&id];
                let mut score = 0.0;
                let mut matched: Vec<String> = Vec::new();
                for group in &groups {
                    let mut best: Option<(f32, &String)> = None;
                    for term in group {
                        if let Some(positions) = self.terms.get(term).and_then(|p| p.get(&id)) {
                            let s = self.bm25(positions.len(), self.terms[term].len(), doc_len);
                            if best.is_none_or(|(b, _)| s > b) {
                                best = Some((s, term));
                            }
                        }
                    }
                    if let Some((s, term)) = best {
                        score += s;
                        if !matched.contains(term) {
                            matched.push(term.clone());
                        }
                    }
                }
                SearchHit { id, score, matched_terms: matched }
            })
            .collect();
        hits.sort_by(|a, b| b.score.total_cmp(&a.score).then(a.id.cmp(&b.id)));
        hits.truncate(limit);
        hits
    }

    fn phrase_matches(&self, phrase: &[String], id: NoteId) -> bool {
        let mut anchors: Option<Vec<u32>> = None;
        for word in phrase {
            let Some(positions) = self.terms.get(word).and_then(|p| p.get(&id)) else {
                return false;
            };
            anchors = Some(match anchors {
                None => positions.clone(),
                Some(prev) => {
                    let set: HashSet<u32> = positions.iter().copied().collect();
                    let next: Vec<u32> =
                        prev.iter().filter(|&&p| set.contains(&(p + 1))).map(|&p| p + 1).collect();
                    if next.is_empty() {
                        return false;
                    }
                    next
                }
            });
        }
        anchors.is_some()
    }
}
```

Update `index/mod.rs`:

```rust
//! In-memory index: fuzzy scorer, search engine, and the Index façade.
pub mod fuzzy;
pub mod search;
```

- [ ] **Step 4: Run to verify pass**

Run: `cargo test -p jd-core search`
Expected: 11 passed. Note `repeated_term_scores_higher` uses `q("rust ")` with a trailing space — `parse_query` still sets `prefix_last: true` (it's the last bare term); the test works because "rust" is a complete term and its expansions include itself plus "returns"… no: "returns" doesn't share the "rust" prefix. If a prefix-expansion surprise DOES break a test, re-read the grammar decision (last bare term always prefix-matches) — the tests were written against that rule; fix the implementation, not the tests.

- [ ] **Step 5: Full gate, then commit**

```bash
cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace
git add crates/jd-core
git commit -m "feat(core): inverted search index with BM25 and query language"
```

---

### Task 3: Snippets + similarity

**Files:**
- Modify: `crates/jd-core/src/index/search.rs`

**Interfaces:**
- Consumes: Task 2's `SearchIndex`.
- Produces: `make_snippet(body, terms, radius) -> Snippet` (pure; app-layer, decision §6.10), `Snippet { text, highlights }`, `SearchIndex::similar(id, k) -> Vec<(NoteId, f32)>` (cosine over tf-idf).

- [ ] **Step 1: Write the failing tests** (append)

```rust
    #[test]
    fn snippet_centers_on_match_and_highlights() {
        let body = "aaaa ".repeat(30) + "the magic word appears here " + &"bbbb ".repeat(30);
        let sn = make_snippet(&body, &["magic".to_owned()], 20);
        assert!(sn.text.contains("magic"));
        assert!(sn.text.len() <= 2 * 20 + "magic".len() + 2 * 3 + 8); // window + ellipses + slack
        assert_eq!(sn.highlights.len(), 1);
        let h = &sn.highlights[0];
        assert_eq!(&sn.text[h.start..h.end], "magic");
    }

    #[test]
    fn snippet_is_case_insensitive_and_multibyte_safe() {
        let body = "prefix Ärger MAGIC suffix";
        let sn = make_snippet(body, &["magic".to_owned()], 50);
        let h = &sn.highlights[0];
        assert_eq!(&sn.text[h.start..h.end], "MAGIC");
    }

    #[test]
    fn snippet_with_no_match_takes_the_head() {
        let sn = make_snippet("just some text with nothing special", &["absent".to_owned()], 10);
        assert!(sn.text.starts_with("just"));
        assert!(sn.highlights.is_empty());
    }

    #[test]
    fn similar_prefers_shared_vocabulary() {
        let mut s = SearchIndex::new();
        s.add_doc(nid(1), "zettelkasten method for permanent notes and knowledge");
        s.add_doc(nid(2), "permanent notes are the zettelkasten heart of knowledge work");
        s.add_doc(nid(3), "gardening tips for tomato plants in july");
        let sim = s.similar(nid(1), 5);
        assert_eq!(sim[0].0, nid(2));
        assert!(sim[0].1 > 0.0);
        assert!(!sim.iter().any(|&(d, _)| d == nid(1)), "never returns itself");
        // doc 3 shares no vocabulary: either absent or scored below doc 2
        if let Some(&(_, score3)) = sim.iter().find(|&&(d, _)| d == nid(3)) {
            assert!(score3 < sim[0].1);
        }
    }

    #[test]
    fn similar_k_and_unknown_doc() {
        let mut s = SearchIndex::new();
        s.add_doc(nid(1), "alpha beta");
        s.add_doc(nid(2), "alpha gamma");
        s.add_doc(nid(3), "alpha delta");
        assert_eq!(s.similar(nid(1), 1).len(), 1);
        assert!(s.similar(nid(99), 5).is_empty());
    }
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p jd-core search`
Expected: compile errors — `make_snippet`/`similar` not defined.

- [ ] **Step 3: Implement** (append to `search.rs`)

```rust
#[derive(Clone, Debug, PartialEq)]
pub struct Snippet {
    pub text: String,
    /// Byte ranges within `text`.
    pub highlights: Vec<std::ops::Range<usize>>,
}

/// Case-fold `body` and keep a byte map back to the original, so highlight
/// ranges land on the ORIGINAL text (lowercasing can change byte lengths).
fn folded_with_map(body: &str) -> (String, Vec<usize>) {
    let mut folded = String::with_capacity(body.len());
    let mut map = Vec::with_capacity(body.len() + 1);
    for (orig_idx, ch) in body.char_indices() {
        for low in ch.to_lowercase() {
            for _ in 0..low.len_utf8() {
                map.push(orig_idx);
            }
            folded.push(low);
        }
    }
    map.push(body.len());
    (folded, map)
}

fn snap_to_boundary(s: &str, mut i: usize) -> usize {
    while i < s.len() && !s.is_char_boundary(i) {
        i += 1;
    }
    i.min(s.len())
}

/// Pure snippet builder for the app layer (decision §6.10): best window of
/// ~radius bytes each side of the first match, ellipses at cut edges,
/// highlight ranges relative to the returned text.
pub fn make_snippet(body: &str, terms: &[String], radius: usize) -> Snippet {
    let (folded, map) = folded_with_map(body);
    // All occurrences (in original-body byte ranges), first-match window.
    let mut occurrences: Vec<std::ops::Range<usize>> = Vec::new();
    for term in terms {
        let term = term.to_lowercase();
        if term.is_empty() {
            continue;
        }
        let mut at = 0;
        while let Some(found) = folded[at..].find(&term) {
            let f_start = at + found;
            let f_end = f_start + term.len();
            occurrences.push(map[f_start]..map[f_end.min(map.len() - 1)]);
            at = f_end;
        }
    }
    occurrences.sort_by_key(|r| r.start);

    let (win_start, win_end) = match occurrences.first() {
        Some(first) => {
            let start = snap_to_boundary(body, first.start.saturating_sub(radius));
            let end = snap_to_boundary(body, (first.end + radius).min(body.len()));
            (start, end)
        }
        None => (0, snap_to_boundary(body, (2 * radius).min(body.len()))),
    };

    let prefix = if win_start > 0 { "…" } else { "" };
    let suffix = if win_end < body.len() { "…" } else { "" };
    let text = format!("{prefix}{}{suffix}", &body[win_start..win_end]);
    let offset = prefix.len();
    let highlights = occurrences
        .into_iter()
        .filter(|r| r.start >= win_start && r.end <= win_end)
        .map(|r| (r.start - win_start + offset)..(r.end - win_start + offset))
        .collect();
    Snippet { text, highlights }
}

impl SearchIndex {
    /// Cosine similarity over tf-idf vectors, computed sparsely via postings.
    /// For ghost fans and post-promotion suggestions (spec §6).
    pub fn similar(&self, id: NoteId, k: usize) -> Vec<(NoteId, f32)> {
        let Some(src_terms) = self.doc_terms.get(&id) else {
            return Vec::new();
        };
        let weight = |tf: u32, df: usize| tf as f32 * self.idf(df);
        let norm_of = |terms: &[(String, u32)]| -> f32 {
            terms
                .iter()
                .map(|(t, tf)| {
                    let df = self.terms.get(t).map_or(1, HashMap::len);
                    let w = weight(*tf, df);
                    w * w
                })
                .sum::<f32>()
                .sqrt()
        };
        let src_norm = norm_of(src_terms);
        if src_norm == 0.0 {
            return Vec::new();
        }
        let mut dot: HashMap<NoteId, f32> = HashMap::new();
        for (term, tf) in src_terms {
            let Some(posts) = self.terms.get(term) else { continue };
            let df = posts.len();
            let w_src = weight(*tf, df);
            for (&other, positions) in posts {
                if other != id {
                    *dot.entry(other).or_default() += w_src * weight(positions.len() as u32, df);
                }
            }
        }
        let mut out: Vec<(NoteId, f32)> = dot
            .into_iter()
            .map(|(d, dp)| {
                let n = norm_of(&self.doc_terms[&d]);
                (d, if n == 0.0 { 0.0 } else { dp / (src_norm * n) })
            })
            .collect();
        out.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
        out.truncate(k);
        out
    }
}
```

- [ ] **Step 4: Run to verify pass**

Run: `cargo test -p jd-core search`
Expected: 16 passed (11 + 5).

- [ ] **Step 5: Full gate, then commit**

```bash
cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace
git add crates/jd-core
git commit -m "feat(core): search snippets and cosine similarity"
```

---

### Task 4: `index/mod.rs` — the Index façade

**Files:**
- Modify: `crates/jd-core/src/index/mod.rs` (the `Index` struct and impl)
- Create: `crates/jd-core/tests/index_integration.rs`

**Interfaces:**
- Consumes: everything prior; `NoteMeta`/`LinkRef` (Task WP1a), `Tag::fold_key`.
- Produces: the full §2.9 `Index` API: `new`, `upsert(meta, body)`, `remove(id)`, `get`, `resolve_title`, `backlinks`, `outlinks`, `notes_with_tag`, `all_tags`, `unlinked`, `fleeting`, `count`, `iter_meta`, `query(q, limit)`, `similar(id, k)`, plus `pub type SharedIndex = Arc<RwLock<Index>>`. WP1d builds the scan/watcher on exactly these.

**Pinned semantics:**
- Search text per note = title (if any) + `" "` + body.
- `titles`: lowercased title → most recent upserter (decision §6.12); removing a note only unmaps the title if that note holds it.
- Link resolution: `by_target` maps lowercased target → source-note set; upserting/removing a note re-resolves exactly the sources targeting its old/new titles.
- `fleeting()`: all `Status::Fleeting`, oldest `created` first (the Inbox, spec §6).
- `unlinked()`: notes with no resolved outlinks AND no backlinks.
- `all_tags()`: buckets by `fold_key`; representative `Tag` = lexicographically smallest in the bucket; counts = number of notes; sorted by count desc, then tag asc.
- `query()`: applies `q.tags` as a candidate filter (intersection of `notes_with_tag` over all query tags) before delegating to `SearchIndex::query`; a tag-only query (no terms/phrases) returns the tag members as hits (score 0.0, empty matched_terms), newest `modified` first, limited.

- [ ] **Step 1: Write the failing integration tests**

`crates/jd-core/tests/index_integration.rs`:

```rust
//! Index façade integration: link resolution across upserts, tag folding,
//! lifecycle views, end-to-end query.

use jd_core::doc::extract_links;
use jd_core::id::NoteId;
use jd_core::index::search::parse_query;
use jd_core::index::Index;
use jd_core::note::{Kind, NoteMeta, Status};
use jd_core::tag::Tag;
use jd_core::time::Timestamp;

fn nid(n: u8) -> NoteId {
    NoteId([n; 16])
}

/// Build a NoteMeta the way doc.rs::to_meta would.
fn meta(n: u8, title: Option<&str>, status: Status, tags: &[&str], body: &str) -> NoteMeta {
    NoteMeta {
        id: nid(n),
        rel_path: format!("notes/{n}.md").into(),
        title: title.map(str::to_owned),
        first_line: title.unwrap_or("scrap").to_owned(),
        status,
        kind: Kind::Note,
        source: None,
        created: Timestamp(n as i64 * 1000),
        modified: Timestamp(n as i64 * 2000),
        tags: tags.iter().filter_map(|t| Tag::new(t)).collect(),
        links_out: extract_links(body),
        word_count: 0,
    }
}

#[test]
fn links_resolve_when_target_appears_and_unresolve_on_removal() {
    let mut ix = Index::new();
    ix.upsert(meta(1, Some("Alpha"), Status::Permanent, &[], "points at [[Beta]]"), "points at [[Beta]]");
    // Beta doesn't exist yet: unresolved
    let outs = ix.outlinks(nid(1));
    assert_eq!(outs.len(), 1);
    assert_eq!(outs[0].1, None);
    assert!(ix.backlinks(nid(2)).is_empty());

    ix.upsert(meta(2, Some("Beta"), Status::Permanent, &[], "the target"), "the target");
    assert_eq!(ix.outlinks(nid(1))[0].1, Some(nid(2)));
    assert_eq!(ix.backlinks(nid(2)), vec![nid(1)]);

    ix.remove(nid(2));
    assert_eq!(ix.outlinks(nid(1))[0].1, None);
}

#[test]
fn retitling_moves_resolution() {
    let mut ix = Index::new();
    ix.upsert(meta(1, Some("Alpha"), Status::Permanent, &[], "see [[Old Name]]"), "see [[Old Name]]");
    ix.upsert(meta(2, Some("Old Name"), Status::Permanent, &[], ""), "");
    assert_eq!(ix.outlinks(nid(1))[0].1, Some(nid(2)));

    // note 2 gets retitled: the link unresolves
    ix.upsert(meta(2, Some("New Name"), Status::Permanent, &[], ""), "");
    assert_eq!(ix.outlinks(nid(1))[0].1, None);
    assert_eq!(ix.resolve_title("new name"), Some(nid(2)));
    assert_eq!(ix.resolve_title("old name"), None);
}

#[test]
fn title_resolution_is_case_insensitive_and_latest_wins() {
    let mut ix = Index::new();
    ix.upsert(meta(1, Some("Same Title"), Status::Permanent, &[], ""), "");
    ix.upsert(meta(2, Some("same title"), Status::Permanent, &[], ""), "");
    assert_eq!(ix.resolve_title("SAME TITLE"), Some(nid(2))); // decision §6.12
}

#[test]
fn tags_fold_and_count() {
    let mut ix = Index::new();
    ix.upsert(meta(1, Some("A"), Status::Permanent, &["book"], ""), "");
    ix.upsert(meta(2, Some("B"), Status::Permanent, &["books"], ""), "");
    ix.upsert(meta(3, Some("C"), Status::Permanent, &["rust"], ""), "");
    let mut with_book = ix.notes_with_tag(&Tag::new("book").unwrap());
    with_book.sort();
    assert_eq!(with_book, vec![nid(1), nid(2)]);
    let all = ix.all_tags();
    assert_eq!(all[0].1, 2); // book-bucket first (count desc)
    assert_eq!(all[0].0.as_str(), "book"); // lexicographically smallest representative
}

#[test]
fn fleeting_is_the_inbox_oldest_first() {
    let mut ix = Index::new();
    ix.upsert(meta(3, None, Status::Fleeting, &[], "newer scrap"), "newer scrap");
    ix.upsert(meta(1, None, Status::Fleeting, &[], "older scrap"), "older scrap");
    ix.upsert(meta(2, Some("Card"), Status::Permanent, &[], ""), "");
    assert_eq!(ix.fleeting(), vec![nid(1), nid(3)]);
}

#[test]
fn unlinked_view() {
    let mut ix = Index::new();
    ix.upsert(meta(1, Some("Alpha"), Status::Permanent, &[], "see [[Beta]]"), "see [[Beta]]");
    ix.upsert(meta(2, Some("Beta"), Status::Permanent, &[], ""), "");
    ix.upsert(meta(3, Some("Loner"), Status::Permanent, &[], "no links"), "no links");
    assert_eq!(ix.unlinked(), vec![nid(3)]);
}

#[test]
fn query_end_to_end_with_tags() {
    let mut ix = Index::new();
    ix.upsert(meta(1, Some("Rust notes"), Status::Permanent, &["rust"], "borrow checker"), "borrow checker");
    ix.upsert(meta(2, Some("Python notes"), Status::Permanent, &["python"], "borrow ideas"), "borrow ideas");
    let hits = ix.query(&parse_query("borrow #rust"), 10);
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].id, nid(1));

    // tag-only query returns members
    let hits = ix.query(&parse_query("#python"), 10);
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].id, nid(2));

    // title terms are searchable
    let hits = ix.query(&parse_query("python"), 10);
    assert_eq!(hits.len(), 1);
}

#[test]
fn remove_cleans_everything() {
    let mut ix = Index::new();
    ix.upsert(meta(1, Some("Alpha"), Status::Permanent, &["rust"], "text [[Beta]]"), "text [[Beta]]");
    ix.remove(nid(1));
    assert_eq!(ix.count(), 0);
    assert!(ix.get(nid(1)).is_none());
    assert_eq!(ix.resolve_title("alpha"), None);
    assert!(ix.notes_with_tag(&Tag::new("rust").unwrap()).is_empty());
    assert!(ix.query(&parse_query("text"), 10).is_empty());
}

#[test]
fn similar_delegates() {
    let mut ix = Index::new();
    ix.upsert(meta(1, Some("A"), Status::Permanent, &[], "zettelkasten permanent notes"), "zettelkasten permanent notes");
    ix.upsert(meta(2, Some("B"), Status::Permanent, &[], "permanent zettelkasten writing"), "permanent zettelkasten writing");
    ix.upsert(meta(3, Some("C"), Status::Permanent, &[], "tomato gardening"), "tomato gardening");
    let sim = ix.similar(nid(1), 2);
    assert_eq!(sim[0].0, nid(2));
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p jd-core --test index_integration`
Expected: compile error — `Index` not defined.

- [ ] **Step 3: Implement** (in `index/mod.rs`)

```rust
//! In-memory index: fuzzy scorer, search engine, and the Index façade
//! (spec §3). The vault on disk is the only persistent truth; this is
//! rebuilt from a scan and kept fresh incrementally.
pub mod fuzzy;
pub mod search;

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, RwLock};

use crate::id::NoteId;
use crate::note::{LinkRef, NoteMeta, Status};
use crate::tag::Tag;
use search::{Query, SearchHit, SearchIndex};

pub type SharedIndex = Arc<RwLock<Index>>;

#[derive(Default)]
pub struct Index {
    notes: HashMap<NoteId, NoteMeta>,
    /// lowercased title → most recent holder (decision §6.12).
    titles: HashMap<String, NoteId>,
    /// note → (link, resolved target) in body order.
    links_fwd: HashMap<NoteId, Vec<(LinkRef, Option<NoteId>)>>,
    links_rev: HashMap<NoteId, HashSet<NoteId>>,
    /// lowercased link target → sources that reference it (resolved or not).
    by_target: HashMap<String, HashSet<NoteId>>,
    /// fold_key → (representative tag, members).
    tags: HashMap<String, (Tag, HashSet<NoteId>)>,
    search: SearchIndex,
}

impl Index {
    pub fn new() -> Index {
        Index::default()
    }

    pub fn upsert(&mut self, meta: NoteMeta, body: &str) {
        let id = meta.id;
        let old_title = self.notes.get(&id).and_then(|m| m.title.clone());
        self.unwire(id);

        // Title mapping + re-resolution of sources aimed at old/new titles.
        if let Some(old) = &old_title {
            let key = old.to_lowercase();
            if self.titles.get(&key) == Some(&id) {
                self.titles.remove(&key);
                self.reresolve_target(&key);
            }
        }
        let search_text = match &meta.title {
            Some(t) => format!("{t} {body}"),
            None => body.to_owned(),
        };
        if let Some(title) = &meta.title {
            let key = title.to_lowercase();
            self.titles.insert(key.clone(), id);
            self.notes.insert(id, meta.clone());
            self.wire_links(id, &meta.links_out);
            self.wire_tags(id, &meta);
            self.reresolve_target(&key);
        } else {
            self.notes.insert(id, meta.clone());
            self.wire_links(id, &meta.links_out);
            self.wire_tags(id, &meta);
        }
        self.search.add_doc(id, &search_text);
    }

    pub fn remove(&mut self, id: NoteId) {
        self.unwire(id);
        if let Some(meta) = self.notes.remove(&id) {
            if let Some(title) = &meta.title {
                let key = title.to_lowercase();
                if self.titles.get(&key) == Some(&id) {
                    self.titles.remove(&key);
                    self.reresolve_target(&key);
                }
            }
        }
        self.search.remove_doc(id);
    }

    /// Detach id's outgoing links, tags, and reverse edges (not its title).
    fn unwire(&mut self, id: NoteId) {
        if let Some(old_links) = self.links_fwd.remove(&id) {
            for (link, resolved) in old_links {
                let key = link.target.to_lowercase();
                if let Some(set) = self.by_target.get_mut(&key) {
                    set.remove(&id);
                    if set.is_empty() {
                        self.by_target.remove(&key);
                    }
                }
                if let Some(target) = resolved {
                    if let Some(rev) = self.links_rev.get_mut(&target) {
                        rev.remove(&id);
                        if rev.is_empty() {
                            self.links_rev.remove(&target);
                        }
                    }
                }
            }
        }
        self.tags.retain(|_, (_, members)| {
            members.remove(&id);
            !members.is_empty()
        });
    }

    fn wire_links(&mut self, id: NoteId, links: &[LinkRef]) {
        let mut fwd = Vec::with_capacity(links.len());
        for link in links {
            let key = link.target.to_lowercase();
            let resolved = self.titles.get(&key).copied();
            self.by_target.entry(key).or_default().insert(id);
            if let Some(target) = resolved {
                self.links_rev.entry(target).or_default().insert(id);
            }
            fwd.push((link.clone(), resolved));
        }
        self.links_fwd.insert(id, fwd);
    }

    fn wire_tags(&mut self, id: NoteId, meta: &NoteMeta) {
        for tag in &meta.tags {
            let entry = self
                .tags
                .entry(tag.fold_key())
                .or_insert_with(|| (tag.clone(), HashSet::new()));
            if tag.as_str() < entry.0.as_str() {
                entry.0 = tag.clone();
            }
            entry.1.insert(id);
        }
    }

    /// Recompute resolution for every source that targets `key`.
    fn reresolve_target(&mut self, key: &str) {
        let Some(sources) = self.by_target.get(key).cloned() else { return };
        let holder = self.titles.get(key).copied();
        for src in sources {
            if let Some(links) = self.links_fwd.get_mut(&src) {
                for (link, resolved) in links.iter_mut() {
                    if link.target.to_lowercase() == key {
                        if let Some(old) = *resolved {
                            if let Some(rev) = self.links_rev.get_mut(&old) {
                                rev.remove(&src);
                            }
                        }
                        *resolved = holder;
                        if let Some(new) = holder {
                            self.links_rev.entry(new).or_default().insert(src);
                        }
                    }
                }
            }
        }
    }

    // ---- lookups ----

    pub fn get(&self, id: NoteId) -> Option<&NoteMeta> {
        self.notes.get(&id)
    }

    pub fn count(&self) -> usize {
        self.notes.len()
    }

    pub fn iter_meta(&self) -> impl Iterator<Item = &NoteMeta> {
        self.notes.values()
    }

    pub fn resolve_title(&self, title: &str) -> Option<NoteId> {
        self.titles.get(&title.to_lowercase()).copied()
    }

    pub fn backlinks(&self, id: NoteId) -> Vec<NoteId> {
        let mut v: Vec<NoteId> = self.links_rev.get(&id).into_iter().flatten().copied().collect();
        v.sort();
        v
    }

    pub fn outlinks(&self, id: NoteId) -> Vec<(LinkRef, Option<NoteId>)> {
        self.links_fwd.get(&id).cloned().unwrap_or_default()
    }

    pub fn notes_with_tag(&self, tag: &Tag) -> Vec<NoteId> {
        self.tags.get(&tag.fold_key()).map_or_else(Vec::new, |(_, m)| m.iter().copied().collect())
    }

    /// (representative tag, member count), count desc then tag asc.
    pub fn all_tags(&self) -> Vec<(Tag, usize)> {
        let mut v: Vec<(Tag, usize)> =
            self.tags.values().map(|(t, m)| (t.clone(), m.len())).collect();
        v.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        v
    }

    pub fn unlinked(&self) -> Vec<NoteId> {
        let mut v: Vec<NoteId> = self
            .notes
            .keys()
            .filter(|id| {
                let no_out = self
                    .links_fwd
                    .get(id)
                    .is_none_or(|l| l.iter().all(|(_, r)| r.is_none()));
                let no_in = self.links_rev.get(id).is_none_or(HashSet::is_empty);
                no_out && no_in
            })
            .copied()
            .collect();
        v.sort();
        v
    }

    /// The Inbox: every fleeting note, oldest created first (spec §6).
    pub fn fleeting(&self) -> Vec<NoteId> {
        let mut v: Vec<&NoteMeta> =
            self.notes.values().filter(|m| m.status == Status::Fleeting).collect();
        v.sort_by_key(|m| (m.created, m.id));
        v.into_iter().map(|m| m.id).collect()
    }

    // ---- search ----

    pub fn query(&self, q: &Query, limit: usize) -> Vec<SearchHit> {
        let tag_filter: Option<HashSet<NoteId>> = if q.tags.is_empty() {
            None
        } else {
            let mut sets = q.tags.iter().map(|t| {
                self.tags
                    .get(&t.fold_key())
                    .map_or_else(HashSet::new, |(_, m)| m.clone())
            });
            let first = sets.next().unwrap_or_default();
            Some(sets.fold(first, |acc, s| acc.intersection(&s).copied().collect()))
        };
        let has_text = !q.terms.is_empty() || !q.phrases.is_empty();
        if !has_text {
            let Some(members) = tag_filter else { return Vec::new() };
            let mut v: Vec<&NoteMeta> =
                members.iter().filter_map(|id| self.notes.get(id)).collect();
            v.sort_by_key(|m| std::cmp::Reverse((m.modified, m.id)));
            return v
                .into_iter()
                .take(limit)
                .map(|m| SearchHit { id: m.id, score: 0.0, matched_terms: Vec::new() })
                .collect();
        }
        self.search.query(q, limit, tag_filter.as_ref())
    }

    pub fn similar(&self, id: NoteId, k: usize) -> Vec<(NoteId, f32)> {
        self.search.similar(id, k)
    }
}
```

- [ ] **Step 4: Run to verify pass**

Run: `cargo test -p jd-core --test index_integration && cargo test -p jd-core`
Expected: 9 integration tests + all unit tests pass.

- [ ] **Step 5: Full gate, then commit**

```bash
cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace
git add crates/jd-core
git commit -m "feat(core): Index facade with link resolution and tag folding"
```

---

## Self-Review Notes

- Arch §2.9 coverage: every pinned method implemented; `SharedIndex` alias exported; `SearchHit`/`make_snippet` per amended contract (§6.10).
- Spec §13 search bullets: fuzzy ranking tables with the acronym case pinned (T1), BM25 sanity (T2), similarity sanity (T3). Palette-strata ordering is WP4 (app-side) — not here.
- Known deliberate simplifications (document, don't "fix"): greedy (not DP-optimal) subsequence matching in fuzzy — the tier system carries the ranking weight, and the bonus structure matches the arch doc's description; `similar()` recomputes candidate norms per call — fine until WP1d's perf budgets say otherwise.
- `upsert` clones `NoteMeta` once for storage — bodies are never stored (spec §3).
