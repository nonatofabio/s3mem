use s3mem::{LocalStore, MemoryType, Record, RecordMeta, Store};

/// A throwaway bundle dir under the OS temp dir, cleared up front so each run is clean.
fn temp_bundle(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("s3mem-it-{}-{}", std::process::id(), name));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

fn record(id: &str, desc: &str) -> Record {
    let meta = RecordMeta::new(id, MemoryType::Semantic, desc, "2026-06-19T00:00:00Z");
    Record::new(meta, format!("Body of {id}."))
}

#[test]
fn put_get_round_trips() {
    let root = temp_bundle("round-trip");
    let store = LocalStore::new(&root, "agent-1");

    let rec = record("alpha", "the first memory");
    store.put(&rec).unwrap();

    let got = store.get("alpha").unwrap();
    assert_eq!(got, rec);

    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn list_and_manifest_and_index_track_writes() {
    let root = temp_bundle("manifest");
    let store = LocalStore::new(&root, "agent-1");

    store.put(&record("beta", "second")).unwrap();
    store.put(&record("alpha", "first")).unwrap();

    assert_eq!(store.list().unwrap(), vec!["alpha", "beta"]);

    let manifest = store.manifest().unwrap();
    assert_eq!(manifest.entries.len(), 2);
    assert_eq!(manifest.entries[0].key, "memories/alpha.md");

    // Derived artifacts exist on disk and reflect the records.
    let index = std::fs::read_to_string(store.bundle_dir().join("index.md")).unwrap();
    assert!(index.contains("[`alpha`](memories/alpha.md)"));
    assert!(std::fs::metadata(store.bundle_dir().join("manifest.json")).is_ok());

    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn delete_removes_and_reindexes() {
    let root = temp_bundle("delete");
    let store = LocalStore::new(&root, "agent-1");

    store.put(&record("gamma", "third")).unwrap();
    store.delete("gamma").unwrap();

    assert!(store.list().unwrap().is_empty());
    assert!(store.get("gamma").is_err());
    assert!(store.manifest().unwrap().entries.is_empty());

    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn rejects_unsafe_ids_and_missing_records() {
    let root = temp_bundle("safety");
    let store = LocalStore::new(&root, "agent-1");

    assert!(store.get("../escape").is_err());
    assert!(store.get("missing").is_err());

    std::fs::remove_dir_all(&root).ok();
}
