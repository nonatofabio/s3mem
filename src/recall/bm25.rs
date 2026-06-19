//! Okapi BM25 over the corpus, hand-rolled (no search-engine dependency).
//!
//! Per query we compute IDF from the (prefiltered) corpus, then score each candidate. Fields
//! are weighted by repeating their tokens: a hit in the one-line `description` should outrank
//! the same word buried in a long body. Weights: `description` ×3, `id` ×2, each tag ×2,
//! `body` ×1.

use std::collections::HashMap;

use crate::recall::tokenize::{tokenize, truncate};
use crate::recall::{Filter, Hit};
use crate::record::Record;

const K1: f32 = 1.2;
const B: f32 = 0.75;

const W_DESCRIPTION: usize = 3;
const W_ID: usize = 2;
const W_TAG: usize = 2;
const W_BODY: usize = 1;

const SNIPPET_CHARS: usize = 200;

/// Rank `records` against `query` with BM25, returning the top `k` hits (score-descending,
/// score > 0). Applies `filter` first. Empty query or empty candidate set → no hits.
pub fn bm25(records: &[Record], query: &str, filter: &Filter, k: usize) -> Vec<Hit> {
    let mut query_terms = tokenize(query);
    query_terms.sort();
    query_terms.dedup();

    let candidates = filter.apply(records);
    if query_terms.is_empty() || candidates.is_empty() {
        return Vec::new();
    }

    let doc_tf: Vec<HashMap<String, u32>> = candidates
        .iter()
        .map(|r| term_freqs(&doc_tokens(r)))
        .collect();
    let doc_len: Vec<f32> = doc_tf
        .iter()
        .map(|tf| tf.values().sum::<u32>() as f32)
        .collect();
    let n = candidates.len() as f32;
    let avgdl = (doc_len.iter().sum::<f32>() / n).max(1.0);

    // Document frequency of each query term across the candidate set.
    let df: HashMap<&str, f32> = query_terms
        .iter()
        .map(|term| {
            let count = doc_tf.iter().filter(|tf| tf.contains_key(term)).count();
            (term.as_str(), count as f32)
        })
        .collect();

    let mut scored: Vec<(usize, f32)> = Vec::new();
    for (i, tf) in doc_tf.iter().enumerate() {
        let mut score = 0.0f32;
        for term in &query_terms {
            let Some(&freq) = tf.get(term) else { continue };
            let f = freq as f32;
            let n_q = df[term.as_str()];
            let idf = (1.0 + (n - n_q + 0.5) / (n_q + 0.5)).ln();
            let denom = f + K1 * (1.0 - B + B * doc_len[i] / avgdl);
            score += idf * (f * (K1 + 1.0)) / denom;
        }
        if score > 0.0 {
            scored.push((i, score));
        }
    }

    scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
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
                snippets: vec![best_snippet(r, &query_terms)],
            }
        })
        .collect()
}

/// The weighted token bag for a record (field weights applied by repetition).
fn doc_tokens(record: &Record) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut push = |text: &str, weight: usize| {
        for tok in tokenize(text) {
            for _ in 0..weight {
                tokens.push(tok.clone());
            }
        }
    };
    push(&record.meta.id, W_ID);
    push(&record.meta.description, W_DESCRIPTION);
    for tag in &record.meta.tags {
        push(tag, W_TAG);
    }
    push(&record.body, W_BODY);
    tokens
}

fn term_freqs(tokens: &[String]) -> HashMap<String, u32> {
    let mut map = HashMap::new();
    for tok in tokens {
        *map.entry(tok.clone()).or_insert(0) += 1;
    }
    map
}

/// The body line with the most query-term hits (fallback: the description), truncated.
fn best_snippet(record: &Record, query_terms: &[String]) -> String {
    let mut best: Option<(usize, &str)> = None;
    for line in record.body.lines() {
        let lower = line.to_lowercase();
        let hits = query_terms
            .iter()
            .filter(|t| lower.contains(t.as_str()))
            .count();
        if hits > 0 && best.is_none_or(|(prev, _)| hits > prev) {
            best = Some((hits, line));
        }
    }
    let text = best.map(|(_, l)| l).unwrap_or(&record.meta.description);
    truncate(text.trim(), SNIPPET_CHARS)
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
