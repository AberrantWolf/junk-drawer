//! Index façade integration: link resolution across upserts, tag folding,
//! lifecycle views, end-to-end query.

use jd_core::doc::extract_links;
use jd_core::id::NoteId;
use jd_core::index::Index;
use jd_core::index::search::parse_query;
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
    ix.upsert(
        meta(
            1,
            Some("Alpha"),
            Status::Permanent,
            &[],
            "points at [[Beta]]",
        ),
        "points at [[Beta]]",
    );
    // Beta doesn't exist yet: unresolved
    let outs = ix.outlinks(nid(1));
    assert_eq!(outs.len(), 1);
    assert_eq!(outs[0].1, None);
    assert!(ix.backlinks(nid(2)).is_empty());

    ix.upsert(
        meta(2, Some("Beta"), Status::Permanent, &[], "the target"),
        "the target",
    );
    assert_eq!(ix.outlinks(nid(1))[0].1, Some(nid(2)));
    assert_eq!(ix.backlinks(nid(2)), vec![nid(1)]);

    ix.remove(nid(2));
    assert_eq!(ix.outlinks(nid(1))[0].1, None);
}

#[test]
fn retitling_moves_resolution() {
    let mut ix = Index::new();
    ix.upsert(
        meta(1, Some("Alpha"), Status::Permanent, &[], "see [[Old Name]]"),
        "see [[Old Name]]",
    );
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
    ix.upsert(
        meta(3, None, Status::Fleeting, &[], "newer scrap"),
        "newer scrap",
    );
    ix.upsert(
        meta(1, None, Status::Fleeting, &[], "older scrap"),
        "older scrap",
    );
    ix.upsert(meta(2, Some("Card"), Status::Permanent, &[], ""), "");
    assert_eq!(ix.fleeting(), vec![nid(1), nid(3)]);
}

#[test]
fn unlinked_view() {
    let mut ix = Index::new();
    ix.upsert(
        meta(1, Some("Alpha"), Status::Permanent, &[], "see [[Beta]]"),
        "see [[Beta]]",
    );
    ix.upsert(meta(2, Some("Beta"), Status::Permanent, &[], ""), "");
    ix.upsert(
        meta(3, Some("Loner"), Status::Permanent, &[], "no links"),
        "no links",
    );
    assert_eq!(ix.unlinked(), vec![nid(3)]);
}

#[test]
fn query_end_to_end_with_tags() {
    let mut ix = Index::new();
    ix.upsert(
        meta(
            1,
            Some("Rust notes"),
            Status::Permanent,
            &["rust"],
            "borrow checker",
        ),
        "borrow checker",
    );
    ix.upsert(
        meta(
            2,
            Some("Python notes"),
            Status::Permanent,
            &["python"],
            "borrow ideas",
        ),
        "borrow ideas",
    );
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
    ix.upsert(
        meta(
            1,
            Some("Alpha"),
            Status::Permanent,
            &["rust"],
            "text [[Beta]]",
        ),
        "text [[Beta]]",
    );
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
    ix.upsert(
        meta(
            1,
            Some("A"),
            Status::Permanent,
            &[],
            "zettelkasten permanent notes",
        ),
        "zettelkasten permanent notes",
    );
    ix.upsert(
        meta(
            2,
            Some("B"),
            Status::Permanent,
            &[],
            "permanent zettelkasten writing",
        ),
        "permanent zettelkasten writing",
    );
    ix.upsert(
        meta(3, Some("C"), Status::Permanent, &[], "tomato gardening"),
        "tomato gardening",
    );
    let sim = ix.similar(nid(1), 2);
    assert_eq!(sim[0].0, nid(2));
}
