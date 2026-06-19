//! Link/unlink over a real LocalStore: edges are mutual, idempotent, persisted, and bump
//! `updated`; traversal sees frontmatter links and body wiki-links and tolerates dangling ones.

use s3mem::{link, neighbors, unlink, LocalStore, MemoryType, Record, RecordMeta, Store};

fn temp_bundle(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("s3mem-graph-{}-{}", std::process::id(), name));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

fn rec(id: &str, body: &str) -> Record {
    let m = RecordMeta::new(id, MemoryType::Semantic, "d", "2020-01-01T00:00:00Z");
    Record::new(m, body)
}

fn seeded(name: &str) -> (std::path::PathBuf, LocalStore) {
    let root = temp_bundle(name);
    let store = LocalStore::new(&root, "agent");
    for id in ["a", "b", "c"] {
        store.put(&rec(id, "")).unwrap();
    }
    (root, store)
}

#[test]
fn link_is_mutual_and_idempotent() {
    let (root, store) = seeded("mutual");
    link(&store, "a", "b").unwrap();
    link(&store, "a", "b").unwrap(); // idempotent

    assert_eq!(store.get("a").unwrap().meta.links, vec!["b"]);
    assert_eq!(store.get("b").unwrap().meta.links, vec!["a"]);

    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn link_bumps_updated_then_unlink_removes_both_ways() {
    let (root, store) = seeded("updated");
    link(&store, "a", "b").unwrap();
    assert_ne!(
        store.get("a").unwrap().meta.updated,
        "2020-01-01T00:00:00Z",
        "link should bump `updated`"
    );

    unlink(&store, "a", "b").unwrap();
    assert!(store.get("a").unwrap().meta.links.is_empty());
    assert!(store.get("b").unwrap().meta.links.is_empty());

    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn link_rejects_missing_and_self() {
    let (root, store) = seeded("invalid");
    assert!(
        link(&store, "a", "nope").is_err(),
        "linking a missing record must fail"
    );
    assert!(link(&store, "a", "a").is_err(), "self-link must fail");
    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn neighbors_traverses_links_and_body_wikilinks() {
    let root = temp_bundle("traverse");
    let store = LocalStore::new(&root, "agent");
    store.put(&rec("a", "")).unwrap();
    store
        .put(&rec("b", "related to [[c]] and [[gone]]"))
        .unwrap();
    store.put(&rec("c", "")).unwrap();
    link(&store, "a", "b").unwrap();

    let records = store.records().unwrap();
    let reached = neighbors(&records, "a", 2);
    let ids: Vec<_> = reached
        .iter()
        .map(|n| (n.id.as_str(), n.depth, n.exists))
        .collect();

    assert!(ids.contains(&("b", 1, true)));
    assert!(
        ids.contains(&("c", 2, true)),
        "body [[c]] should be an edge"
    );
    assert!(
        ids.contains(&("gone", 2, false)),
        "dangling [[gone]] should appear as missing"
    );

    std::fs::remove_dir_all(&root).ok();
}
