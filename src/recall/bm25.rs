//! Okapi BM25 over a `&[Record]` corpus, hand-rolled (no search-engine dependency).
//!
//! This is the uncached path: it tokenizes the candidates on the fly and scores them. The
//! cached [`Index`](crate::recall::index::Index) does the same scoring against precomputed
//! term frequencies — both share [`crate::recall::score`], so they rank identically.

use std::collections::BTreeMap;

use crate::recall::score::{best_snippet, rank, weighted_term_freqs};
use crate::recall::tokenize::tokenize;
use crate::recall::{Filter, Hit};
use crate::record::Record;

/// Rank `records` against `query` with BM25, returning the top `k` hits (score-descending,
/// score > 0). Applies `filter` first. Empty query or empty candidate set → no hits.
pub fn bm25(records: &[Record], query: &str, filter: &Filter, k: usize) -> Vec<Hit> {
    let mut query_terms = tokenize(query);
    query_terms.sort();
    query_terms.dedup();
    if query_terms.is_empty() {
        return Vec::new();
    }

    let candidates = filter.apply(records);
    if candidates.is_empty() {
        return Vec::new();
    }

    let tf_owned: Vec<BTreeMap<String, u32>> =
        candidates.iter().map(|r| weighted_term_freqs(r)).collect();
    let tf: Vec<&BTreeMap<String, u32>> = tf_owned.iter().collect();
    let len: Vec<u32> = tf_owned.iter().map(|t| t.values().sum()).collect();

    let mut scored = rank(&query_terms, &tf, &len);
    scored.truncate(k);
    scored
        .into_iter()
        .map(|(i, score)| {
            let r = candidates[i];
            Hit {
                id: r.meta.id.clone(),
                kind: r.meta.kind,
                description: r.meta.description.clone(),
                score: Some(score),
                snippets: vec![best_snippet(&r.body, &r.meta.description, &query_terms)],
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::record::{MemoryType, RecordMeta};

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

    #[test]
    fn ranks_the_on_topic_record_first() {
        let c = corpus();
        let hits = bm25(&c, "rust systems", &Filter::default(), 10);
        assert_eq!(hits[0].id, "rust-pref");
        assert!(hits[0].score.unwrap() > 0.0);
    }

    #[test]
    fn description_match_outranks_body_only() {
        let c = corpus();
        let hits = bm25(&c, "python", &Filter::default(), 10);
        assert_eq!(hits[0].id, "py-data");
    }

    #[test]
    fn filter_narrows_before_scoring() {
        let c = corpus();
        let filter = Filter {
            kinds: vec![MemoryType::Procedural],
            ..Filter::default()
        };
        let hits = bm25(&c, "deploy terraform", &filter, 10);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].id, "deploy");
    }

    #[test]
    fn empty_query_or_corpus_yields_nothing() {
        let c = corpus();
        assert!(bm25(&c, "", &Filter::default(), 10).is_empty());
        assert!(bm25(&[], "rust", &Filter::default(), 10).is_empty());
    }

    #[test]
    fn snippet_shows_the_matching_line() {
        let c = corpus();
        let hits = bm25(&c, "terraform", &Filter::default(), 1);
        assert!(hits[0].snippets[0].contains("terraform"));
    }
}
