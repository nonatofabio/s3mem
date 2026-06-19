//! The OKF format layer: one memory == one markdown file with YAML frontmatter.

use std::collections::BTreeMap;

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

impl std::str::FromStr for MemoryType {
    type Err = String;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "semantic" => Ok(MemoryType::Semantic),
            "episodic" => Ok(MemoryType::Episodic),
            "procedural" => Ok(MemoryType::Procedural),
            "reference" => Ok(MemoryType::Reference),
            other => Err(format!(
                "unknown memory type `{other}` (expected semantic|episodic|procedural|reference)"
            )),
        }
    }
}

/// The YAML frontmatter of an OKF record — the structured, filterable fields.
///
/// Field order here is the on-disk order (serde preserves struct order), chosen so a
/// hand-read file leads with identity and description.
///
/// Not `Eq`: `extra` holds arbitrary YAML, which may contain floats, so only `PartialEq`
/// is available.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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
    /// Any other frontmatter keys a human or tool added, preserved verbatim so hand-edits
    /// are lossless (OKF's "git-native, hand-editable" promise). Serialized inline.
    #[serde(flatten)]
    pub extra: BTreeMap<String, serde_yaml::Value>,
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
            extra: BTreeMap::new(),
        }
    }
}

/// The frontmatter keys backed by named [`RecordMeta`] fields (`kind` serializes as `type`).
/// `extra` must never duplicate these. Keep in sync with `RecordMeta`'s fields.
const RESERVED_FRONTMATTER_KEYS: [&str; 8] = [
    "id",
    "type",
    "description",
    "tags",
    "created",
    "updated",
    "source",
    "links",
];

/// A complete OKF memory: frontmatter plus the markdown body holding the fact itself.
#[derive(Debug, Clone, PartialEq)]
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
    ///
    /// The body is recovered byte-for-byte (leading whitespace and CRLF preserved) so that
    /// `parse(to_markdown(r)) == r`: we scan the frontmatter line-by-line (allowing a
    /// whitespace-padded closing `---`) while tracking the byte offset, then slice the body
    /// out of the raw input rather than re-joining lines.
    pub fn parse(input: &str) -> Result<Self> {
        // Tolerate a UTF-8 BOM.
        let input = input.strip_prefix('\u{feff}').unwrap_or(input);

        // `split_inclusive` keeps each line's trailing `\n`, so byte lengths sum exactly to
        // the offset where the body begins.
        let mut segments = input.split_inclusive('\n');
        let mut consumed = 0usize;

        let first = segments.next().ok_or(Error::MissingFrontmatter)?;
        consumed += first.len();
        if first.trim() != "---" {
            return Err(Error::MissingFrontmatter);
        }

        let mut yaml = String::new();
        let mut closed = false;
        for seg in segments {
            consumed += seg.len();
            if seg.trim() == "---" {
                closed = true;
                break;
            }
            yaml.push_str(seg);
        }
        if !closed {
            return Err(Error::MissingFrontmatter);
        }

        let meta: RecordMeta = serde_yaml::from_str(&yaml)?;

        // Everything after the closing delimiter is the body, minus the single blank-line
        // separator and single trailing newline that `to_markdown` adds.
        // The separator and trailing newline may be `\n` (our output) or `\r\n` (a document
        // authored by a Windows editor / git autocrlf), so strip either — without disturbing
        // any CRLFs *inside* the body that we wrote ourselves.
        let rest = &input[consumed..];
        let rest = rest
            .strip_prefix("\r\n")
            .or_else(|| rest.strip_prefix('\n'))
            .unwrap_or(rest);
        let rest = rest
            .strip_suffix("\r\n")
            .or_else(|| rest.strip_suffix('\n'))
            .unwrap_or(rest);

        Ok(Record {
            meta,
            body: rest.to_string(),
        })
    }

    /// Render back to the on-disk markdown form (`---` frontmatter, blank line, body).
    /// The body is emitted verbatim — [`parse`](Self::parse) is its exact inverse.
    ///
    /// Any `extra` key that shadows a named frontmatter field is dropped first: otherwise
    /// `#[serde(flatten)]` would emit the key twice (e.g. two `id:` lines) and the result
    /// wouldn't parse. The named field is authoritative.
    pub fn to_markdown(&self) -> Result<String> {
        let yaml = if self
            .meta
            .extra
            .keys()
            .any(|k| RESERVED_FRONTMATTER_KEYS.contains(&k.as_str()))
        {
            let mut meta = self.meta.clone();
            meta.extra
                .retain(|k, _| !RESERVED_FRONTMATTER_KEYS.contains(&k.as_str()));
            serde_yaml::to_string(&meta)?
        } else {
            serde_yaml::to_string(&self.meta)? // ends with a newline
        };
        Ok(format!(
            "---\n{}---\n\n{}\n",
            harden_yaml_1_1(&yaml),
            self.body
        ))
    }
}

/// serde_yaml emits YAML 1.2, where `no`/`yes`/`on`/`off`/`null`/`42` are plain strings — but
/// a YAML 1.1 reader (PyYAML, many Obsidian/MkDocs plugins, "any other agent" the README
/// promises portability to) coerces those bare tokens to booleans/null/numbers. We quote any
/// such bare scalar so a string stays a string everywhere. Over-quoting is harmless: the value
/// is unchanged, only its rendering.
fn harden_yaml_1_1(yaml: &str) -> String {
    let mut out: String = yaml.lines().map(harden_line).collect::<Vec<_>>().join("\n");
    out.push('\n');
    out
}

fn harden_line(line: &str) -> String {
    let indent_len = line.len() - line.trim_start().len();
    let (indent, rest) = line.split_at(indent_len);

    // Block-sequence item: `- value`
    if let Some(value) = rest.strip_prefix("- ") {
        if needs_quoting(value) {
            return format!("{indent}- '{value}'");
        }
        return line.to_string();
    }
    // Mapping entry: `key: value` (a bare value can't contain ": ", so first match is safe)
    if let Some(pos) = rest.find(": ") {
        let (key, value) = (&rest[..pos], &rest[pos + 2..]);
        if needs_quoting(value) {
            return format!("{indent}{key}: '{value}'");
        }
    }
    line.to_string()
}

/// True if `value` is a bare scalar a YAML 1.1 resolver would coerce away from a string.
fn needs_quoting(value: &str) -> bool {
    // Already quoted / a flow or block indicator / anchor / alias → serde_yaml handled it.
    let Some(first) = value.bytes().next() else {
        return false;
    };
    if matches!(
        first,
        b'\'' | b'"' | b'[' | b'{' | b'|' | b'>' | b'&' | b'*' | b'!'
    ) {
        return false;
    }
    let lower = value.to_ascii_lowercase();
    let boolish = matches!(
        lower.as_str(),
        "y" | "n" | "yes" | "no" | "true" | "false" | "on" | "off"
    );
    let nullish = matches!(lower.as_str(), "null" | "none" | "~");
    let numberish = value.parse::<i64>().is_ok() || value.parse::<f64>().is_ok();
    boolish || nullish || numberish
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
