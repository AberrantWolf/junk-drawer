//! Inverted search index with BM25 (spec §7). Positions are token indices
//! (decision §6.11). ~500 lines was the spec's budget; keep it lean.
//! tantivy explicitly rejected (Appendix B).

use std::cmp::Reverse;
use std::collections::{BTreeMap, BinaryHeap, HashMap, HashSet};

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

/// Entry for the bounded min-heap used in `query`.
///
/// The heap is keyed so that the *lowest-scoring* hit sits at the top and can
/// be evicted when a better candidate arrives.  We want the final output
/// ordered score DESC then id ASC (matching the previous full-sort), so the
/// min-heap comparator must be the *inverse*:
///
///   heap order: score ASC (Reverse), then id DESC (Reverse) on tiebreak
///
/// That keeps the worst current hit accessible at the top.  After popping all
/// entries, reversing the vec restores score DESC / id ASC order.
#[derive(PartialEq)]
struct HeapEntry {
    /// `Reverse` so that lower scores sort higher in the max-heap → min-heap.
    score_rev: Reverse<u32>, // bits of f32, compared via total_cmp polarity
    /// `Reverse` on id so that *larger* ids sort higher in the heap (evicted
    /// first when scores tie), giving us id ASC in the final reversed output.
    id_rev: Reverse<NoteId>,
    /// Lazily-built matched terms — only cloned for hits that enter the heap.
    matched_terms: Vec<String>,
}

impl Eq for HeapEntry {}

impl PartialOrd for HeapEntry {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for HeapEntry {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.score_rev
            .cmp(&other.score_rev)
            .then(self.id_rev.cmp(&other.id_rev))
    }
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
    /// Cached tf-idf norms per doc; invalidated wholesale on any mutation
    /// (idf shifts globally when df/N change, so per-doc invalidation is wrong).
    norms: HashMap<NoteId, f32>,
    /// True when `norms` is out of date and must be rebuilt before use.
    norms_dirty: bool,
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
        self.norms_dirty = true;
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
            self.terms
                .entry(term.clone())
                .or_default()
                .insert(id, positions);
            per_doc.push((term, count));
        }
        self.doc_terms.insert(id, per_doc);
    }

    pub fn remove_doc(&mut self, id: NoteId) {
        self.norms_dirty = true;
        let Some(old) = self.doc_terms.remove(&id) else {
            return;
        };
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

    /// Rebuild the per-doc tf-idf norm cache if dirty.  Call this once after a
    /// batch of mutations to amortize the O(total_terms) rebuild cost.
    pub(crate) fn ensure_norms(&mut self) {
        if !self.norms_dirty {
            return;
        }
        self.norms.clear();
        for (id, terms) in &self.doc_terms {
            let norm: f32 = terms
                .iter()
                .map(|(t, tf)| {
                    let df = self.terms.get(t).map_or(1, HashMap::len);
                    let w = *tf as f32 * self.idf(df);
                    w * w
                })
                .sum::<f32>()
                .sqrt();
            self.norms.insert(*id, norm);
        }
        self.norms_dirty = false;
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
    /// Phrase words score as individual term groups (plus the adjacency requirement) — deliberate v1 behavior.
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
        //
        // Strategy: pick the smallest group (by total posting count) as the
        // seed — collect its doc ids into a Vec.  For every other group,
        // retain only ids that appear in at least one of that group's posting
        // maps, probing directly rather than materialising a per-group HashSet.
        // This avoids O(N) HashSet allocations for dense queries where every
        // group covers most of the corpus.

        // Compute total posting count per group to find the smallest.
        let group_sizes: Vec<usize> = groups
            .iter()
            .map(|g| {
                g.iter()
                    .map(|t| self.terms.get(t).map_or(0, HashMap::len))
                    .sum()
            })
            .collect();
        let seed_idx = group_sizes
            .iter()
            .enumerate()
            .min_by_key(|&(_, &sz)| sz)
            .map(|(i, _)| i)
            .unwrap_or(0);

        // Build seed Vec from the smallest group (dedup via HashSet when the
        // group has multiple expansions, plain extend when it has one).
        let seed_group = &groups[seed_idx];
        let mut candidates: Vec<NoteId> = if seed_group.len() == 1 {
            self.terms
                .get(&seed_group[0])
                .map(|p| p.keys().copied().collect())
                .unwrap_or_default()
        } else {
            let mut seen: HashSet<NoteId> = HashSet::new();
            for term in seed_group {
                if let Some(posts) = self.terms.get(term) {
                    seen.extend(posts.keys().copied());
                }
            }
            seen.into_iter().collect()
        };

        // Intersect with every other group by probing their postings maps.
        for (i, group) in groups.iter().enumerate() {
            if i == seed_idx {
                continue;
            }
            candidates.retain(|id| {
                group
                    .iter()
                    .any(|t| self.terms.get(t).is_some_and(|p| p.contains_key(id)))
            });
        }

        // Apply filter, negation, and phrase constraints.
        if let Some(f) = filter {
            candidates.retain(|id| f.contains(id));
        }
        for neg in &q.negated {
            if let Some(posts) = self.terms.get(neg) {
                candidates.retain(|id| !posts.contains_key(id));
            }
        }
        candidates.retain(|id| q.phrases.iter().all(|p| self.phrase_matches(p, *id)));

        // Pre-resolve each group's term → (&postings_map, df) once, outside the
        // per-candidate loop.  This avoids one BTreeMap lookup per term per candidate.
        type ResolvedTerm<'a> = (&'a String, &'a HashMap<NoteId, Vec<u32>>, usize);
        let resolved_groups: Vec<Vec<ResolvedTerm<'_>>> = groups
            .iter()
            .map(|group| {
                group
                    .iter()
                    .filter_map(|term| self.terms.get(term).map(|posts| (term, posts, posts.len())))
                    .collect()
            })
            .collect();

        // Score candidates and keep the top-`limit` via a bounded min-heap.
        //
        // We score cheaply first (no String clones) and only materialise
        // `matched_terms` for hits that actually enter the heap, eliminating
        // ~17k×2 String clones per dense query.
        //
        // Heap polarity: score ASC (Reverse) / id DESC (Reverse) so the worst
        // current hit sits at the top and is evicted first.  Popping all
        // entries and reversing the vec restores the spec order: score DESC
        // then id ASC.
        let mut heap: BinaryHeap<HeapEntry> = BinaryHeap::with_capacity(limit + 1);

        for id in candidates {
            let doc_len = self.doc_len[&id];

            // Cheap scoring pass — no String allocations.
            let mut score = 0.0f32;
            let mut best_terms: Vec<(&String, f32)> = Vec::new();
            for resolved in &resolved_groups {
                let mut best: Option<(f32, &String)> = None;
                for &(term, posts, df) in resolved {
                    if let Some(positions) = posts.get(&id) {
                        let s = self.bm25(positions.len(), df, doc_len);
                        if best.is_none_or(|(b, _)| s > b) {
                            best = Some((s, term));
                        }
                    }
                }
                if let Some((s, term)) = best {
                    score += s;
                    best_terms.push((term, s));
                }
            }

            // Evict if the heap is full and this score does not beat the worst
            // entry.  Compare using the same total_cmp polarity as the heap key.
            let score_bits = Reverse(score.to_bits()); // NaN-safe: to_bits preserves total_cmp order for non-NaN f32
            let id_rev = Reverse(id);
            if heap.len() >= limit
                && let Some(worst) = heap.peek()
            {
                // worse-or-equal: score_bits > worst (min-heap so worst is at top
                // with the LOWEST score, i.e. the HIGHEST Reverse(bits))
                let new_entry_worse = score_bits > worst.score_rev
                    || (score_bits == worst.score_rev && id_rev < worst.id_rev);
                if new_entry_worse {
                    continue;
                }
                heap.pop();
            }

            // This hit qualifies — now pay for the String clones.
            let mut matched: Vec<String> = Vec::new();
            for (term, _) in &best_terms {
                if !matched.contains(*term) {
                    matched.push((*term).clone());
                }
            }

            heap.push(HeapEntry {
                score_rev: score_bits,
                id_rev,
                matched_terms: matched,
            });
        }

        // Drain heap into a vec, then sort to get the spec order: score DESC,
        // id ASC.  The heap guarantees at most `limit` entries, so this sort
        // is O(limit · log limit) — negligible.
        let mut hits: Vec<SearchHit> = heap
            .into_iter()
            .map(|e| SearchHit {
                id: e.id_rev.0,
                score: f32::from_bits(e.score_rev.0),
                matched_terms: e.matched_terms,
            })
            .collect();
        hits.sort_by(|a, b| b.score.total_cmp(&a.score).then(a.id.cmp(&b.id)));
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
                    // Both `prev` and `positions` are ascending-sorted (tokens
                    // are appended in document order by `add_doc`).  Walk them
                    // with two pointers — no allocations.
                    let mut next: Vec<u32> = Vec::new();
                    let mut j = 0usize;
                    for &anchor in &prev {
                        let target = anchor + 1;
                        // Advance j until positions[j] >= target.
                        while j < positions.len() && positions[j] < target {
                            j += 1;
                        }
                        if j < positions.len() && positions[j] == target {
                            next.push(target);
                        }
                    }
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
    ///
    /// When the norm cache is clean (after `ensure_norms`), candidate doc norms
    /// are read from `self.norms` in O(1) rather than recomputed.  When dirty
    /// (after a mutation before `ensure_norms` is called), falls back to the
    /// original per-call computation — correct but slower.
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
        let src_norm = if self.norms_dirty {
            norm_of(src_terms)
        } else {
            self.norms
                .get(&id)
                .copied()
                .unwrap_or_else(|| norm_of(src_terms))
        };
        if src_norm == 0.0 {
            return Vec::new();
        }
        let mut dot: HashMap<NoteId, f32> = HashMap::new();
        for (term, tf) in src_terms {
            let Some(posts) = self.terms.get(term) else {
                continue;
            };
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
                let n = if self.norms_dirty {
                    norm_of(&self.doc_terms[&d])
                } else {
                    self.norms
                        .get(&d)
                        .copied()
                        .unwrap_or_else(|| norm_of(&self.doc_terms[&d]))
                };
                (d, if n == 0.0 { 0.0 } else { dp / (src_norm * n) })
            })
            .collect();
        out.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
        out.truncate(k);
        out
    }
}

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
        assert_eq!(
            parsed.phrases,
            vec![vec!["smart".to_owned(), "notes".to_owned()]]
        );
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
        assert_eq!(
            unclosed.phrases,
            vec![vec!["unclosed".to_owned(), "phrase".to_owned()]]
        );
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
    fn phrase_adjacency_two_pointer_edges() {
        let mut s = SearchIndex::new();
        // "a b a b a" — 'a' at 0,2,4; 'b' at 1,3
        s.add_doc(nid(1), "a b a b a");
        assert_eq!(s.query(&q("\"a b\""), 10, None).len(), 1);
        assert_eq!(s.query(&q("\"b a\""), 10, None).len(), 1);
        assert_eq!(s.query(&q("\"a a\""), 10, None).len(), 0);
        assert_eq!(s.query(&q("\"a b a\""), 10, None).len(), 1);
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
        let sn = make_snippet(
            "just some text with nothing special",
            &["absent".to_owned()],
            10,
        );
        assert!(sn.text.starts_with("just"));
        assert!(sn.highlights.is_empty());
    }

    #[test]
    fn similar_prefers_shared_vocabulary() {
        let mut s = SearchIndex::new();
        s.add_doc(
            nid(1),
            "zettelkasten method for permanent notes and knowledge",
        );
        s.add_doc(
            nid(2),
            "permanent notes are the zettelkasten heart of knowledge work",
        );
        s.add_doc(nid(3), "gardening tips for tomato plants in july");
        let sim = s.similar(nid(1), 5);
        assert_eq!(sim[0].0, nid(2));
        assert!(sim[0].1 > 0.0);
        assert!(
            !sim.iter().any(|&(d, _)| d == nid(1)),
            "never returns itself"
        );
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

    #[test]
    fn similar_results_identical_with_and_without_cache() {
        let mut s = SearchIndex::new();
        s.add_doc(nid(1), "zettelkasten method for permanent notes");
        s.add_doc(nid(2), "permanent notes are the zettelkasten heart");
        s.add_doc(nid(3), "gardening tips for tomato plants");
        let dirty = s.similar(nid(1), 5); // cache dirty → per-call path
        s.ensure_norms();
        let cached = s.similar(nid(1), 5); // cache clean
        assert_eq!(dirty.len(), cached.len());
        for (a, b) in dirty.iter().zip(&cached) {
            assert_eq!(a.0, b.0);
            assert!((a.1 - b.1).abs() < 1e-6, "{} vs {}", a.1, b.1);
        }
    }

    #[test]
    fn cache_invalidates_on_mutation() {
        let mut s = SearchIndex::new();
        s.add_doc(nid(1), "alpha beta");
        s.add_doc(nid(2), "alpha gamma");
        s.ensure_norms();
        s.add_doc(nid(3), "alpha beta delta"); // changes df(alpha,beta) → all norms stale
        let sim = s.similar(nid(1), 5); // must not use stale cache
        assert!(sim.iter().any(|&(d, _)| d == nid(3)));
        s.ensure_norms();
        let sim2 = s.similar(nid(1), 5);
        for (a, b) in sim.iter().zip(&sim2) {
            assert!((a.1 - b.1).abs() < 1e-6);
        }
    }
}
