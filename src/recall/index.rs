//! The cached recall index — a precomputed BM25 index persisted as one bundle artifact
//! (`recall-index.json`), so recall is a single fetch instead of one read per memory.
//!
//! Staleness is gated by a cheap [`Store::fingerprint`] (a hash of the `memories/` listing
//! metadata — no body reads). [`load_or_build_index`] returns the cached index when its
//! fingerprint still matches, and otherwise rebuilds from `Store::records()` and self-heals,
//! which covers out-of-band edits (a hand-edited file, an `aws s3 cp`).
//!
//! Building stays in the recall layer (lazy, on read) so backends don't depend on recall;
//! the trade-off is that the first recall after a write rebuilds. The cache holds each
//! document's precomputed term frequencies *and* its body, so [`Index::search`] never
//! re-tokenizes and [`Index::grep`] can scan without touching the store again.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::error::Result;
use crate::recall::grep::{grep, GrepOptions};
use crate::recall::score::{best_snippet, rank, weighted_term_freqs};
use crate::recall::tokenize::tokenize;
use crate::recall::{Filter, Hit};
use crate::record::{MemoryType, Record, RecordMeta};
use crate::store::Store;

/// Bundle-relative name of the cache artifact (a sibling of `manifest.json` / `index.md`).
pub const RECALL_INDEX_FILE: &str = "recall-index.json";

/// Bumped whenever the on-disk index format or the scoring weights change, so an old cache is
/// treated as stale and rebuilt rather than misread.
const INDEX_VERSION: u32 = 1;

/// A precomputed BM25 index over a bundle's records.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Index {
    docs: Vec<IndexedDoc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct IndexedDoc {
    id: String,
    #[serde(rename = "type")]
    kind: MemoryType,
    description: String,
    #[serde(default)]
    tags: Vec<String>,
    created: String,
    updated: String,
    body: String,
    /// Field-weighted term frequencies (the precomputed part — search never re-tokenizes docs).
    tf: BTreeMap<String, u32>,
    /// Weighted document length (`tf.values().sum()`), cached to avoid recomputing.
    len: u32,
}

/// The serialized cache: an index plus the fingerprint of the corpus it was built from.
#[derive(Serialize, Deserialize)]
struct CachedIndex {
    version: u32,
    fingerprint: String,
    index: Index,
}

impl IndexedDoc {
    fn from_record(record: &Record) -> Self {
        let tf = weighted_term_freqs(record);
        let len = tf.values().sum();
        IndexedDoc {
            id: record.meta.id.clone(),
            kind: record.meta.kind,
            description: record.meta.description.clone(),
            tags: record.meta.tags.clone(),
            created: record.meta.created.clone(),
            updated: record.meta.updated.clone(),
            body: record.body.clone(),
            tf,
            len,
        }
    }

    /// Reconstruct a [`Record`] for the grep path. Fields recall never reads (`source`,
    /// `links`, `extra`) are left empty — they don't affect matching.
    fn to_record(&self) -> Record {
        let mut meta = RecordMeta::new(&self.id, self.kind, &self.description, &self.created);
        meta.updated = self.updated.clone();
        meta.tags = self.tags.clone();
        Record::new(meta, &self.body)
    }
}

impl Index {
    /// Build an index from a corpus (precomputes per-doc term frequencies).
    pub fn build(records: &[Record]) -> Self {
        Index {
            docs: records.iter().map(IndexedDoc::from_record).collect(),
        }
    }

    pub fn len(&self) -> usize {
        self.docs.len()
    }

    pub fn is_empty(&self) -> bool {
        self.docs.is_empty()
    }

    /// BM25 recall against the precomputed index. Identical ranking to
    /// [`bm25`](crate::recall::bm25::bm25) over the same records.
    pub fn search(&self, query: &str, filter: &Filter, k: usize) -> Vec<Hit> {
        let mut query_terms = tokenize(query);
        query_terms.sort();
        query_terms.dedup();
        if query_terms.is_empty() {
            return Vec::new();
        }

        let candidates: Vec<&IndexedDoc> = self
            .docs
            .iter()
            .filter(|d| filter.matches_meta(d.kind, &d.tags))
            .collect();
        if candidates.is_empty() {
            return Vec::new();
        }

        let tf: Vec<&BTreeMap<String, u32>> = candidates.iter().map(|d| &d.tf).collect();
        let len: Vec<u32> = candidates.iter().map(|d| d.len).collect();

        let mut scored = rank(&query_terms, &tf, &len);
        scored.truncate(k);
        scored
            .into_iter()
            .map(|(i, score)| {
                let d = candidates[i];
                Hit {
                    id: d.id.clone(),
                    kind: d.kind,
                    description: d.description.clone(),
                    score: Some(score),
                    snippets: vec![best_snippet(&d.body, &d.description, &query_terms)],
                }
            })
            .collect()
    }

    /// Grep against the cached bodies (no store access). Same results as
    /// [`grep`](crate::recall::grep::grep) over the same records.
    pub fn grep(&self, opts: &GrepOptions) -> Result<Vec<Hit>> {
        let records: Vec<Record> = self.docs.iter().map(IndexedDoc::to_record).collect();
        grep(&records, opts)
    }
}

/// Return the bundle's recall index, using the cached artifact when its fingerprint still
/// matches the corpus and rebuilding (and re-caching) otherwise.
///
/// Caching the rebuilt index is best-effort: on a read-only or failing backend, recall still
/// returns correct results, just without persisting the cache.
pub fn load_or_build_index(store: &dyn Store) -> Result<Index> {
    let fingerprint = store.fingerprint()?;

    if let Some(raw) = store.read_artifact(RECALL_INDEX_FILE)? {
        if let Ok(cached) = serde_json::from_str::<CachedIndex>(&raw) {
            if cached.version == INDEX_VERSION && cached.fingerprint == fingerprint {
                return Ok(cached.index);
            }
        }
        // Wrong version / stale / corrupt → fall through and rebuild.
    }

    let index = Index::build(&store.records()?);
    let cached = CachedIndex {
        version: INDEX_VERSION,
        fingerprint,
        index: index.clone(),
    };
    match serde_json::to_string(&cached) {
        Ok(json) => {
            if let Err(e) = store.write_artifact(RECALL_INDEX_FILE, &json) {
                eprintln!("s3mem: could not persist recall index ({e}); continuing uncached");
            }
        }
        Err(e) => eprintln!("s3mem: could not serialize recall index ({e}); continuing uncached"),
    }
    Ok(index)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::recall::bm25::bm25;
    use crate::recall::GrepOptions;

    fn rec(id: &str, kind: MemoryType, desc: &str, tags: &[&str], body: &str) -> Record {
        let mut m = RecordMeta::new(id, kind, desc, "2026-06-19T00:00:00Z");
        m.tags = tags.iter().map(|s| s.to_string()).collect();
        Record::new(m, body)
    }

    fn corpus() -> Vec<Record> {
        vec![
            rec(
                "rust-pref",
                MemoryType::Semantic,
                "User prefers Rust",
                &["lang"],
                "The user likes Rust for systems work.",
            ),
            rec(
                "py-data",
                MemoryType::Semantic,
                "Python for data",
                &["lang"],
                "Pandas and numpy for data analysis.",
            ),
            rec(
                "deploy",
                MemoryType::Procedural,
                "Deploy steps",
                &["ops"],
                "Run terraform apply then restart the service.",
            ),
        ]
    }

    /// The cache must be transparent: identical results to the uncached path.
    #[test]
    fn index_search_matches_uncached_bm25() {
        let c = corpus();
        let index = Index::build(&c);
        for query in ["rust systems", "python data", "deploy terraform", "service"] {
            let cached = index.search(query, &Filter::default(), 10);
            let direct = bm25(&c, query, &Filter::default(), 10);
            let cached_ids: Vec<_> = cached.iter().map(|h| &h.id).collect();
            let direct_ids: Vec<_> = direct.iter().map(|h| &h.id).collect();
            assert_eq!(
                cached_ids, direct_ids,
                "ranking diverged for query {query:?}"
            );
        }
    }

    #[test]
    fn index_grep_matches_uncached_grep() {
        let c = corpus();
        let index = Index::build(&c);
        let cached = index.grep(&GrepOptions::new("terraform")).unwrap();
        let direct = grep(&c, &GrepOptions::new("terraform")).unwrap();
        assert_eq!(cached.len(), direct.len());
        assert_eq!(cached[0].id, direct[0].id);
    }

    #[test]
    fn index_respects_filter() {
        let index = Index::build(&corpus());
        let filter = Filter {
            kinds: vec![MemoryType::Procedural],
            ..Filter::default()
        };
        let hits = index.search("deploy terraform", &filter, 10);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].id, "deploy");
    }

    #[test]
    fn serializes_round_trip() {
        let index = Index::build(&corpus());
        let json = serde_json::to_string(&index).unwrap();
        let back: Index = serde_json::from_str(&json).unwrap();
        assert_eq!(back.len(), 3);
        assert_eq!(
            back.search("rust", &Filter::default(), 1)[0].id,
            "rust-pref"
        );
    }
}
