//! The cached recall index over a real LocalStore: it is written, reused while fresh,
//! self-heals when the bundle changes out of band, survives a corrupt cache, and stays
//! transparent (same results as the uncached path).

use s3mem::recall::RECALL_INDEX_FILE;
use s3mem::{bm25, load_or_build_index, Filter, LocalStore, MemoryType, Record, RecordMeta, Store};

fn temp_bundle(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("s3mem-cache-{}-{}", std::process::id(), name));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

fn rec(id: &str, desc: &str, body: &str) -> Record {
    let m = RecordMeta::new(id, MemoryType::Semantic, desc, "2026-06-19T00:00:00Z");
    Record::new(m, body)
}

#[test]
fn cache_is_written_then_reused() {
    let root = temp_bundle("written");
    let store = LocalStore::new(&root, "agent");
    store
        .put(&rec("rust-pref", "User prefers Rust", "Rust for systems."))
        .unwrap();

    // No cache before the first recall.
    assert!(store.read_artifact(RECALL_INDEX_FILE).unwrap().is_none());

    let index = load_or_build_index(&store).unwrap();
    assert_eq!(
        index.search("rust", &Filter::default(), 5)[0].id,
        "rust-pref"
    );

    // The cache artifact now exists, and a second call still returns correct results.
    assert!(store.read_artifact(RECALL_INDEX_FILE).unwrap().is_some());
    assert_eq!(load_or_build_index(&store).unwrap().len(), 1);

    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn cache_self_heals_when_bundle_changes() {
    let root = temp_bundle("heal");
    let store = LocalStore::new(&root, "agent");
    store.put(&rec("a", "first", "alpha")).unwrap();

    let fp1 = store.fingerprint().unwrap();
    let built = load_or_build_index(&store).unwrap();
    assert_eq!(built.len(), 1);

    // A new memory changes the fingerprint, so the stale cache must be rebuilt on next recall.
    store.put(&rec("b", "second", "beta terraform")).unwrap();
    assert_ne!(
        store.fingerprint().unwrap(),
        fp1,
        "fingerprint must change after a write"
    );

    let healed = load_or_build_index(&store).unwrap();
    assert_eq!(healed.len(), 2);
    assert_eq!(healed.search("terraform", &Filter::default(), 5)[0].id, "b");

    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn corrupt_cache_is_ignored_and_rebuilt() {
    let root = temp_bundle("corrupt");
    let store = LocalStore::new(&root, "agent");
    store.put(&rec("a", "first", "alpha")).unwrap();
    store
        .write_artifact(RECALL_INDEX_FILE, "{ not valid json")
        .unwrap();

    let index = load_or_build_index(&store).unwrap();
    assert_eq!(index.len(), 1);

    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn cached_results_match_uncached() {
    let root = temp_bundle("transparent");
    let store = LocalStore::new(&root, "agent");
    store
        .put(&rec(
            "rust-pref",
            "User prefers Rust",
            "Rust for systems work.",
        ))
        .unwrap();
    store
        .put(&rec("py-data", "Python for data", "Pandas and numpy."))
        .unwrap();
    store
        .put(&rec("deploy", "Deploy steps", "Run terraform apply."))
        .unwrap();

    let records = store.records().unwrap();
    let index = load_or_build_index(&store).unwrap();
    for query in ["rust systems", "python data", "deploy terraform"] {
        let cached: Vec<_> = index
            .search(query, &Filter::default(), 10)
            .into_iter()
            .map(|h| h.id)
            .collect();
        let direct: Vec<_> = bm25(&records, query, &Filter::default(), 10)
            .into_iter()
            .map(|h| h.id)
            .collect();
        assert_eq!(cached, direct, "cached vs uncached diverged for {query:?}");
    }

    std::fs::remove_dir_all(&root).ok();
}
