//! Recall over a real LocalStore corpus: remember a few memories, then rank and grep them
//! through `Store::records()` — the exact path the CLI uses.

use s3mem::{bm25, grep, Filter, GrepOptions, LocalStore, MemoryType, Record, RecordMeta, Store};

fn temp_bundle(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("s3mem-recall-{}-{}", std::process::id(), name));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

fn rec(id: &str, kind: MemoryType, desc: &str, tags: &[&str], body: &str) -> Record {
    let mut m = RecordMeta::new(id, kind, desc, "2026-06-19T00:00:00Z");
    m.tags = tags.iter().map(|s| s.to_string()).collect();
    Record::new(m, body)
}

fn seeded_store(name: &str) -> (std::path::PathBuf, LocalStore) {
    let root = temp_bundle(name);
    let store = LocalStore::new(&root, "agent");
    store
        .put(&rec(
            "rust-pref",
            MemoryType::Semantic,
            "User prefers Rust",
            &["lang"],
            "Likes Rust for systems work.",
        ))
        .unwrap();
    store
        .put(&rec(
            "py-data",
            MemoryType::Semantic,
            "Python for data",
            &["lang"],
            "Pandas and numpy for analysis.",
        ))
        .unwrap();
    store
        .put(&rec(
            "deploy",
            MemoryType::Procedural,
            "Deploy steps",
            &["ops"],
            "Run terraform apply then restart.",
        ))
        .unwrap();
    (root, store)
}

#[test]
fn bm25_ranks_corpus_loaded_from_store() {
    let (root, store) = seeded_store("bm25");
    let records = store.records().unwrap();
    assert_eq!(records.len(), 3);

    let hits = bm25(&records, "rust systems", &Filter::default(), 10);
    std::fs::remove_dir_all(&root).ok();
    assert_eq!(hits[0].id, "rust-pref");
}

#[test]
fn grep_finds_literal_across_store() {
    let (root, store) = seeded_store("grep");
    let records = store.records().unwrap();

    let hits = grep(&records, &GrepOptions::new("terraform")).unwrap();
    std::fs::remove_dir_all(&root).ok();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].id, "deploy");
    assert!(hits[0].snippets.iter().any(|s| s.contains("terraform")));
}

#[test]
fn filter_by_type_through_store() {
    let (root, store) = seeded_store("filter");
    let records = store.records().unwrap();

    let filter = Filter {
        kinds: vec![MemoryType::Procedural],
        tags: vec![],
    };
    let hits = bm25(&records, "deploy restart", &filter, 10);
    std::fs::remove_dir_all(&root).ok();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].id, "deploy");
}
