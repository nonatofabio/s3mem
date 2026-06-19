//! The BM25 scoring core, shared by the uncached [`bm25`](crate::recall::bm25::bm25) function
//! and the cached [`Index`](crate::recall::index::Index). Keeping the math in one place is what
//! lets the two paths return identical rankings.

use std::collections::BTreeMap;

use crate::recall::tokenize::{tokenize, truncate};
use crate::record::Record;

const K1: f32 = 1.2;
const B: f32 = 0.75;

// Field weights, applied by counting a field's tokens this many times.
const W_DESCRIPTION: u32 = 3;
const W_ID: u32 = 2;
const W_TAG: u32 = 2;
const W_BODY: u32 = 1;

const SNIPPET_CHARS: usize = 200;

/// The field-weighted term-frequency map for a record. This is what an `Index` stores per
/// document so search never has to re-tokenize the corpus.
pub(crate) fn weighted_term_freqs(record: &Record) -> BTreeMap<String, u32> {
    let mut tf = BTreeMap::new();
    let mut add = |text: &str, weight: u32| {
        for tok in tokenize(text) {
            *tf.entry(tok).or_insert(0) += weight;
        }
    };
    add(&record.meta.id, W_ID);
    add(&record.meta.description, W_DESCRIPTION);
    for tag in &record.meta.tags {
        add(tag, W_TAG);
    }
    add(&record.body, W_BODY);
    tf
}

/// Score documents (given their term-frequency maps and weighted lengths) against the query,
/// returning `(doc index, score)` for every doc with score > 0, sorted descending.
///
/// IDF and avgdl are computed over exactly the docs passed in — so callers get corpus-relative
/// scores for whatever candidate set (post-filter) they hand over.
pub(crate) fn rank(
    query_terms: &[String],
    doc_tf: &[&BTreeMap<String, u32>],
    doc_len: &[u32],
) -> Vec<(usize, f32)> {
    let n = doc_tf.len() as f32;
    if n == 0.0 || query_terms.is_empty() {
        return Vec::new();
    }
    let avgdl = (doc_len.iter().map(|&l| l as f32).sum::<f32>() / n).max(1.0);

    let df: BTreeMap<&str, f32> = query_terms
        .iter()
        .map(|term| {
            let count = doc_tf.iter().filter(|tf| tf.contains_key(term)).count();
            (term.as_str(), count as f32)
        })
        .collect();

    let mut scored = Vec::new();
    for (i, tf) in doc_tf.iter().enumerate() {
        let mut score = 0.0f32;
        for term in query_terms {
            let Some(&freq) = tf.get(term) else { continue };
            let f = freq as f32;
            let n_q = df[term.as_str()];
            let idf = (1.0 + (n - n_q + 0.5) / (n_q + 0.5)).ln();
            let denom = f + K1 * (1.0 - B + B * doc_len[i] as f32 / avgdl);
            score += idf * (f * (K1 + 1.0)) / denom;
        }
        if score > 0.0 {
            scored.push((i, score));
        }
    }
    scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    scored
}

/// The body line with the most query-term hits (fallback: the description), truncated.
pub(crate) fn best_snippet(body: &str, description: &str, query_terms: &[String]) -> String {
    let mut best: Option<(usize, &str)> = None;
    for line in body.lines() {
        let lower = line.to_lowercase();
        let hits = query_terms
            .iter()
            .filter(|t| lower.contains(t.as_str()))
            .count();
        if hits > 0 && best.is_none_or(|(prev, _)| hits > prev) {
            best = Some((hits, line));
        }
    }
    let text = best.map(|(_, l)| l).unwrap_or(description);
    truncate(text.trim(), SNIPPET_CHARS)
}
