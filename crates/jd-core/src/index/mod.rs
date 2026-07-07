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

    /// Atomic remove-old + upsert-new under a single `&mut self`.
    /// Used when a file at the same path is replaced with a new identity
    /// (e.g. the watcher's path-reuse case, or `RenameTitle`).
    pub fn replace_at_path(&mut self, old_id: NoteId, meta: NoteMeta, body: &str) {
        self.remove(old_id);
        self.upsert(meta, body);
    }

    pub fn remove(&mut self, id: NoteId) {
        self.unwire(id);
        if let Some(meta) = self.notes.remove(&id)
            && let Some(title) = &meta.title
        {
            let key = title.to_lowercase();
            if self.titles.get(&key) == Some(&id) {
                self.titles.remove(&key);
                self.reresolve_target(&key);
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
                if let Some(target) = resolved
                    && let Some(rev) = self.links_rev.get_mut(&target)
                {
                    rev.remove(&id);
                    if rev.is_empty() {
                        self.links_rev.remove(&target);
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
        let Some(sources) = self.by_target.get(key).cloned() else {
            return;
        };
        let holder = self.titles.get(key).copied();
        for src in sources {
            if let Some(links) = self.links_fwd.get_mut(&src) {
                for (link, resolved) in links.iter_mut() {
                    if link.target.to_lowercase() == key {
                        if let Some(old) = *resolved
                            && let Some(rev) = self.links_rev.get_mut(&old)
                        {
                            rev.remove(&src);
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
        let mut v: Vec<NoteId> = self
            .links_rev
            .get(&id)
            .into_iter()
            .flatten()
            .copied()
            .collect();
        v.sort();
        v
    }

    pub fn outlinks(&self, id: NoteId) -> Vec<(LinkRef, Option<NoteId>)> {
        self.links_fwd.get(&id).cloned().unwrap_or_default()
    }

    pub fn notes_with_tag(&self, tag: &Tag) -> Vec<NoteId> {
        self.tags
            .get(&tag.fold_key())
            .map_or_else(Vec::new, |(_, m)| m.iter().copied().collect())
    }

    /// (representative tag, member count), count desc then tag asc.
    pub fn all_tags(&self) -> Vec<(Tag, usize)> {
        let mut v: Vec<(Tag, usize)> = self
            .tags
            .values()
            .map(|(t, m)| (t.clone(), m.len()))
            .collect();
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
        let mut v: Vec<&NoteMeta> = self
            .notes
            .values()
            .filter(|m| m.status == Status::Fleeting)
            .collect();
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
            let Some(members) = tag_filter else {
                return Vec::new();
            };
            let mut v: Vec<&NoteMeta> =
                members.iter().filter_map(|id| self.notes.get(id)).collect();
            v.sort_by_key(|m| std::cmp::Reverse((m.modified, m.id)));
            return v
                .into_iter()
                .take(limit)
                .map(|m| SearchHit {
                    id: m.id,
                    score: 0.0,
                    matched_terms: Vec::new(),
                })
                .collect();
        }
        self.search.query(q, limit, tag_filter.as_ref())
    }

    pub fn similar(&self, id: NoteId, k: usize) -> Vec<(NoteId, f32)> {
        self.search.similar(id, k)
    }

    /// Rebuild the tf-idf norm cache that accelerates `similar`.  Intended to
    /// run once after bulk indexing (initial scan, rescan) — NOT per upsert:
    /// the rebuild is O(total terms) and would blow the incremental reindex
    /// budget.  While the cache is dirty, `similar` falls back to the
    /// (correct, slower) per-call computation.
    pub fn refresh_similarity_cache(&mut self) {
        self.search.ensure_norms();
    }
}
