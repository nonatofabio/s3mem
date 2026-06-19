//! The OKF format layer: one memory == one markdown file with YAML frontmatter.

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

/// The kind of memory a record holds. Serialized lowercase in frontmatter (`type:`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MemoryType {
    /// Durable facts about the world or the user.
    Semantic,
    /// Time-stamped events ("what happened").
    Episodic,
    /// How-to / step sequences.
    Procedural,
    /// Pointers to external resources.
    Reference,
}

impl std::fmt::Display for MemoryType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            MemoryType::Semantic => "semantic",
            MemoryType::Episodic => "episodic",
            MemoryType::Procedural => "procedural",
            MemoryType::Reference => "reference",
        })
    }
}

/// The YAML frontmatter of an OKF record — the structured, filterable fields.
///
/// Field order here is the on-disk order (serde preserves struct order), chosen so a
/// hand-read file leads with identity and description.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecordMeta {
    /// Stable kebab-slug; also the object key stem (`memories/<id>.md`).
    pub id: String,
    #[serde(rename = "type")]
    pub kind: MemoryType,
    /// One-line summary, used for cheap relevance ranking during recall.
    pub description: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    /// ISO-8601 timestamp.
    pub created: String,
    /// ISO-8601 timestamp.
    pub updated: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    /// Graph edges: ids of related records (mirror as `[[id]]` in the body).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub links: Vec<String>,
}

impl RecordMeta {
    /// Construct metadata with the required fields, stamping `created` and `updated` to the
    /// same instant. Fill `tags`/`source`/`links` afterward as needed.
    pub fn new(
        id: impl Into<String>,
        kind: MemoryType,
        description: impl Into<String>,
        timestamp: impl Into<String>,
    ) -> Self {
        let ts = timestamp.into();
        RecordMeta {
            id: id.into(),
            kind,
            description: description.into(),
            tags: Vec::new(),
            created: ts.clone(),
            updated: ts,
            source: None,
            links: Vec::new(),
        }
    }
}

/// A complete OKF memory: frontmatter plus the markdown body holding the fact itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Record {
    pub meta: RecordMeta,
    pub body: String,
}

impl Record {
    pub fn new(meta: RecordMeta, body: impl Into<String>) -> Self {
        Record {
            meta,
            body: body.into(),
        }
    }

    /// Parse a `---`-delimited frontmatter document into a [`Record`].
    pub fn parse(input: &str) -> Result<Self> {
        // Tolerate a UTF-8 BOM.
        let input = input.strip_prefix('\u{feff}').unwrap_or(input);

        let mut lines = input.lines();
        if lines.next().map(str::trim) != Some("---") {
            return Err(Error::MissingFrontmatter);
        }

        let mut yaml = String::new();
        let mut closed = false;
        for line in lines.by_ref() {
            if line.trim() == "---" {
                closed = true;
                break;
            }
            yaml.push_str(line);
            yaml.push('\n');
        }
        if !closed {
            return Err(Error::MissingFrontmatter);
        }

        let meta: RecordMeta = serde_yaml::from_str(&yaml)?;
        let body = lines.collect::<Vec<_>>().join("\n").trim().to_string();
        Ok(Record { meta, body })
    }

    /// Render back to the on-disk markdown form (`---` frontmatter, blank line, body).
    pub fn to_markdown(&self) -> Result<String> {
        let yaml = serde_yaml::to_string(&self.meta)?; // ends with a newline
        Ok(format!("---\n{yaml}---\n\n{}\n", self.body.trim_end()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Record {
        let mut meta = RecordMeta::new(
            "prefers-rust",
            MemoryType::Semantic,
            "User leans toward Rust",
            "2026-06-19T00:00:00Z",
        );
        meta.tags = vec!["stack".into(), "preferences".into()];
        meta.links = vec!["stack-choice".into()];
        Record::new(
            meta,
            "User prefers Rust unless prohibitive. See [[stack-choice]].",
        )
    }

    #[test]
    fn round_trips_through_markdown() {
        let rec = sample();
        let md = rec.to_markdown().unwrap();
        let parsed = Record::parse(&md).unwrap();
        assert_eq!(rec, parsed);
    }

    #[test]
    fn renders_expected_shape() {
        let md = sample().to_markdown().unwrap();
        assert!(md.starts_with("---\n"));
        assert!(md.contains("id: prefers-rust"));
        assert!(md.contains("type: semantic"));
        assert!(md.contains("[[stack-choice]]"));
    }

    #[test]
    fn empty_collections_are_omitted() {
        let meta = RecordMeta::new("x", MemoryType::Reference, "d", "2026-01-01T00:00:00Z");
        let md = Record::new(meta, "body").to_markdown().unwrap();
        assert!(!md.contains("tags:"));
        assert!(!md.contains("links:"));
        assert!(!md.contains("source:"));
    }

    #[test]
    fn rejects_missing_frontmatter() {
        assert!(matches!(
            Record::parse("no frontmatter here"),
            Err(Error::MissingFrontmatter)
        ));
        assert!(matches!(
            Record::parse("---\nid: x\n(no close)"),
            Err(Error::MissingFrontmatter)
        ));
    }
}
