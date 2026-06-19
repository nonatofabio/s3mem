//! Recall layer: ranked and literal search over the in-memory corpus of [`Record`]s.
//!
//! Two paths, both backend-agnostic (they take a `&[Record]` slice that a backend produced
//! via [`Store::records`](crate::store::Store::records)):
//!
//! - [`bm25`] — Okapi BM25 ranked relevance. The default for fuzzy, natural-language recall.
//! - [`grep`] — literal/regex pattern match with line-numbered snippets (the s3grep path).
//!   Use it for exact tokens, identifiers, and code-like strings.
//!
//! Both run a cheap frontmatter [`Filter`] (by `type`/`tags`) before scoring/scanning — the
//! prefilter stage from the architecture, where S3 Select would eventually plug in.

pub mod bm25;
pub mod grep;
pub mod tokenize;

use serde::Serialize;

use crate::record::{MemoryType, Record};

pub use bm25::bm25;
pub use grep::{grep, GrepOptions};

/// A frontmatter prefilter applied before BM25 scoring or grep scanning.
///
/// Empty fields mean "no constraint". `tags` is an AND: a record must carry *all* listed tags
/// (case-insensitive) to pass.
#[derive(Debug, Clone, Default)]
pub struct Filter {
    pub kinds: Vec<MemoryType>,
    pub tags: Vec<String>,
}

impl Filter {
    pub fn matches(&self, record: &Record) -> bool {
        let kind_ok = self.kinds.is_empty() || self.kinds.contains(&record.meta.kind);
        let tags_ok = self.tags.iter().all(|want| {
            record
                .meta
                .tags
                .iter()
                .any(|have| have.eq_ignore_ascii_case(want))
        });
        kind_ok && tags_ok
    }

    /// Narrow `records` to those passing the filter.
    pub fn apply<'a>(&self, records: &'a [Record]) -> Vec<&'a Record> {
        records.iter().filter(|r| self.matches(r)).collect()
    }
}

/// A single recall result. Intentionally small — the agent triages on `snippets`, then fetches
/// full content with `get <id>` only for the records it wants.
#[derive(Debug, Clone, Serialize)]
pub struct Hit {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: MemoryType,
    pub description: String,
    /// BM25 relevance score; `None` for grep (which is a match/no-match path).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub score: Option<f32>,
    /// Why it matched: the best body excerpt (BM25) or the matching lines (grep).
    pub snippets: Vec<String>,
}
