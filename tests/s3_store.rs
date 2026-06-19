//! Live S3 round-trip. Skipped unless `S3MEM_TEST_BUCKET` is set (and AWS credentials are
//! available), so the suite stays green offline. Run against a real/LocalStack bucket with:
//!
//! ```bash
//! S3MEM_TEST_BUCKET=my-bucket cargo test --features s3 --test s3_store -- --nocapture
//! ```
#![cfg(feature = "s3")]

use s3mem::{MemoryType, Record, RecordMeta, S3Store, Store};

fn record(id: &str, desc: &str) -> Record {
    let meta = RecordMeta::new(id, MemoryType::Semantic, desc, "2026-06-19T00:00:00Z");
    Record::new(meta, format!("Body of {id}."))
}

#[test]
fn s3_round_trip_when_configured() {
    let Ok(bucket) = std::env::var("S3MEM_TEST_BUCKET") else {
        eprintln!("S3MEM_TEST_BUCKET unset — skipping live S3 round-trip");
        return;
    };
    // Unique namespace per process so concurrent/CI runs don't clobber each other.
    let namespace = format!("s3mem-it/{}", std::process::id());
    let store = S3Store::new(bucket, namespace).expect("build S3Store");

    store.put(&record("alpha", "first")).unwrap();
    store.put(&record("beta", "second")).unwrap();

    assert_eq!(store.get("alpha").unwrap(), record("alpha", "first"));
    assert_eq!(store.list().unwrap(), vec!["alpha", "beta"]);

    let manifest = store.manifest().unwrap();
    assert_eq!(manifest.entries.len(), 2);
    assert_eq!(manifest.entries[0].key, "memories/alpha.md");

    store.delete("alpha").unwrap();
    assert_eq!(store.list().unwrap(), vec!["beta"]);
    assert!(store.get("alpha").is_err());

    store.delete("beta").unwrap();
}
