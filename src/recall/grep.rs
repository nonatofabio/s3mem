//! The grep path: literal or regex pattern match across a record's fields, returning
//! line-numbered snippets like `grep -n`. This is the precise counterpart to BM25 — use it
//! when you know the exact token/identifier/regex, not a fuzzy topic.

use regex::{Regex, RegexBuilder};

use crate::error::{Error, Result};
use crate::recall::{Filter, Hit};
use crate::record::Record;

/// Options for [`grep`].
#[derive(Debug, Clone)]
pub struct GrepOptions {
    pub pattern: String,
    /// Treat `pattern` as a regex (default: literal substring).
    pub regex: bool,
    /// Case-sensitive matching (default: insensitive).
    pub case_sensitive: bool,
    pub filter: Filter,
    /// Cap on snippets returned per record, so a pathological match can't flood output.
    pub max_snippets: usize,
}

impl GrepOptions {
    /// Literal, case-insensitive search with sensible defaults.
    pub fn new(pattern: impl Into<String>) -> Self {
        GrepOptions {
            pattern: pattern.into(),
            regex: false,
            case_sensitive: false,
            filter: Filter::default(),
            max_snippets: 5,
        }
    }
}

/// Search `records` for `opts.pattern`, returning a [`Hit`] per record with at least one
/// match. Errors only if a `regex` pattern fails to compile.
pub fn grep(records: &[Record], opts: &GrepOptions) -> Result<Vec<Hit>> {
    let source = if opts.regex {
        opts.pattern.clone()
    } else {
        regex::escape(&opts.pattern)
    };
    let re = RegexBuilder::new(&source)
        .case_insensitive(!opts.case_sensitive)
        .build()
        .map_err(|e| Error::Pattern(e.to_string()))?;

    let mut hits = Vec::new();
    for record in opts.filter.apply(records) {
        let mut snippets = Vec::new();
        consider(&mut snippets, &re, opts.max_snippets, "id", &record.meta.id);
        consider(
            &mut snippets,
            &re,
            opts.max_snippets,
            "description",
            &record.meta.description,
        );
        for tag in &record.meta.tags {
            consider(&mut snippets, &re, opts.max_snippets, "tag", tag);
        }
        for (n, line) in record.body.lines().enumerate() {
            consider(
                &mut snippets,
                &re,
                opts.max_snippets,
                &format!("body:{}", n + 1),
                line,
            );
        }
        if !snippets.is_empty() {
            hits.push(Hit {
                id: record.meta.id.clone(),
                kind: record.meta.kind,
                description: record.meta.description.clone(),
                score: None,
                snippets,
            });
        }
    }
    Ok(hits)
}

/// Push `label: text` if `text` matches and we're under the per-record cap.
fn consider(snippets: &mut Vec<String>, re: &Regex, max: usize, label: &str, text: &str) {
    if snippets.len() < max && re.is_match(text) {
        snippets.push(format!("{label}: {}", text.trim()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::record::{MemoryType, RecordMeta};

    fn rec(id: &str, desc: &str, tags: &[&str], body: &str) -> Record {
        let mut m = RecordMeta::new(id, MemoryType::Semantic, desc, "2026-06-19T00:00:00Z");
        m.tags = tags.iter().map(|s| s.to_string()).collect();
        Record::new(m, body)
    }

    fn corpus() -> Vec<Record> {
        vec![
            rec(
                "rust-pref",
                "User prefers Rust",
                &["lang"],
                "The user likes Rust.",
            ),
            rec("py-data", "Python for data", &["lang"], "Pandas and numpy."),
            rec(
                "deploy",
                "Deploy steps",
                &["ops"],
                "Run terraform apply\nthen restart.",
            ),
        ]
    }

    #[test]
    fn literal_match_returns_snippet_with_line_number() {
        let hits = grep(&corpus(), &GrepOptions::new("terraform")).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].id, "deploy");
        assert!(hits[0]
            .snippets
            .iter()
            .any(|s| s.starts_with("body:1") && s.contains("terraform")));
    }

    #[test]
    fn case_insensitive_by_default_sensitive_on_request() {
        // "RUST" matches "Rust" by default ...
        assert_eq!(grep(&corpus(), &GrepOptions::new("RUST")).unwrap().len(), 1);
        // ... but not when case-sensitive.
        let opts = GrepOptions {
            case_sensitive: true,
            ..GrepOptions::new("RUST")
        };
        assert!(grep(&corpus(), &opts).unwrap().is_empty());
    }

    #[test]
    fn regex_mode_matches_alternation() {
        let opts = GrepOptions {
            regex: true,
            ..GrepOptions::new("pa(ndas|sta)")
        };
        let hits = grep(&corpus(), &opts).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].id, "py-data");
    }

    #[test]
    fn literal_mode_does_not_treat_pattern_as_regex() {
        // The literal "pa(ndas|sta)" appears in no record, so no match (no regex meaning).
        assert!(grep(&corpus(), &GrepOptions::new("pa(ndas|sta)"))
            .unwrap()
            .is_empty());
    }

    #[test]
    fn invalid_regex_errors() {
        let opts = GrepOptions {
            regex: true,
            ..GrepOptions::new("(")
        };
        assert!(matches!(grep(&corpus(), &opts), Err(Error::Pattern(_))));
    }

    #[test]
    fn filter_applies() {
        let opts = GrepOptions {
            filter: Filter {
                tags: vec!["ops".into()],
                ..Filter::default()
            },
            ..GrepOptions::new("e")
        };
        let hits = grep(&corpus(), &opts).unwrap();
        assert!(hits.iter().all(|h| h.id == "deploy"));
    }
}
