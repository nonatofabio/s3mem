//! QA corner-case probes for the recall layer (BM25 + grep). Each test asserts the
//! *correct* behavior, so a failing test is a confirmed defect/limitation. See QA_FINDINGS.md.

use s3mem::recall::{bm25, grep, Filter, GrepOptions};
use s3mem::record::{MemoryType, Record, RecordMeta};

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
            "Rust for systems.",
        ),
        rec(
            "py-data",
            MemoryType::Semantic,
            "Python for data",
            &["lang"],
            "Pandas and numpy.",
        ),
        rec(
            "jp-note",
            MemoryType::Semantic,
            "日本語のメモ",
            &["lang"],
            "これは日本語の本文です。",
        ),
    ]
}

// --- REC-1: empty-query asymmetry between the two recall tools ---------------

/// ASSUMPTION: an empty query returns nothing (BM25 does — `empty_query_or_corpus_yields
/// _nothing`). Probe: grep with an empty pattern compiles to an empty regex that matches
/// every field of every record, so `s3mem grep "$QUERY"` with an unset var dumps the whole
/// bundle. The two recall paths must agree that "no query" means "no results".
#[test]
fn grep_empty_pattern_returns_nothing() {
    let c = corpus();
    let hits = grep(&c, &GrepOptions::new("")).unwrap();
    assert!(
        hits.is_empty(),
        "grep(\"\") returned the entire corpus ({} hits) — BM25 returns 0 for an empty query",
        hits.len()
    );
}

/// Reinforcement: a whitespace-only pattern is the same hazard (matches anything with a space).
#[test]
fn grep_whitespace_pattern_returns_nothing() {
    let c = corpus();
    let hits = grep(&c, &GrepOptions::new("   ")).unwrap();
    assert!(
        hits.is_empty(),
        "grep on whitespace matched {} records",
        hits.len()
    );
}

// --- REC-2: the snippet cap silently hides whole matches ---------------------

/// ASSUMPTION: `max_snippets` caps output volume, not which records match. Probe: with
/// `max_snippets = 0`, a record that clearly matches is dropped entirely (0 hits), so the
/// cap doubles as an accidental "hide all matches" switch.
#[test]
fn grep_zero_cap_still_reports_matching_records() {
    let c = corpus();
    let opts = GrepOptions {
        max_snippets: 0,
        ..GrepOptions::new("rust")
    };
    let hits = grep(&c, &opts).unwrap();
    assert_eq!(
        hits.len(),
        1,
        "max_snippets=0 hid a matching record instead of just capping its snippets"
    );
}

// --- REC-3: ranked recall silently misses non-space-delimited scripts --------

/// ASSUMPTION: BM25 is the default recall path; grep is the precise fallback. Probe: the
/// tokenizer splits on non-alphanumerics, but `char::is_alphanumeric` is Unicode-aware, so
/// an entire CJK run becomes ONE token. `bm25("日本")` finds nothing while `grep("日本")`
/// finds the record — the default path silently misses matches the literal path catches.
#[test]
fn bm25_recalls_cjk_substring_like_grep_does() {
    let c = corpus();
    let by_grep = grep(&c, &GrepOptions::new("日本")).unwrap();
    let by_bm25 = bm25(&c, "日本", &Filter::default(), 10);
    assert_eq!(by_grep.len(), 1, "sanity: grep should find the CJK record");
    assert_eq!(
        by_bm25.len(),
        by_grep.len(),
        "BM25 missed a CJK substring that grep matched (tokenizer treats 日本語 as one token)"
    );
}

// --- Confirmed-sound behaviors (defensive coverage; these PASS) --------------

/// k = 0 must not panic and must yield no hits.
#[test]
fn bm25_k_zero_is_safe() {
    assert!(bm25(&corpus(), "rust", &Filter::default(), 0).is_empty());
}

/// A term present in EVERY candidate must still score positively (the IDF uses the
/// `1 + (…)` smoothing, so it never goes negative and zeroes the hit out).
#[test]
fn ubiquitous_term_keeps_positive_score() {
    let hits = bm25(&corpus(), "lang", &Filter::default(), 10); // tag on all three
    assert_eq!(hits.len(), 3);
    assert!(hits.iter().all(|h| h.score.unwrap() > 0.0));
}

/// A query made only of punctuation / single chars tokenizes to nothing → no hits (matches
/// the documented empty-query behavior, and is NOT the grep-empty footgun).
#[test]
fn bm25_punctuation_only_query_is_empty() {
    assert!(bm25(&corpus(), "??? -- !", &Filter::default(), 10).is_empty());
}

/// A pathological regex must be rejected at compile time (size limit), never hang the
/// process — the grep path takes untrusted patterns from the CLI.
#[test]
fn pathological_regex_is_rejected_not_hung() {
    let opts = GrepOptions {
        regex: true,
        ..GrepOptions::new("a{1000000000}{1000000}")
    };
    assert!(grep(&corpus(), &opts).is_err());
}

/// Literal mode must not interpret regex metacharacters (pinned at the integration boundary
/// the CLI uses).
#[test]
fn literal_mode_does_not_interpret_metacharacters() {
    let hits = grep(&corpus(), &GrepOptions::new("pa(ndas|sta)")).unwrap();
    assert!(hits.is_empty(), "literal pattern was treated as a regex");
}
