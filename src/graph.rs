//! Link graph over the memory bundle.
//!
//! A record's outbound edges are the union of two sources:
//! - its frontmatter `links` — **mutual** edges managed by [`link`] / [`unlink`];
//! - `[[id]]` wiki-links written in its body — **directional**, authored in prose (OKF's
//!   "mirror as `[[id]]`" convention). `[[id|alias]]` is supported; the target is the part
//!   before `|`.
//!
//! Mutations go through the [`Store`] (read-modify-write, bumping `updated`); traversal is a
//! pure function over a `&[Record]` corpus and tolerates dangling targets.

use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};
use std::sync::OnceLock;

use regex::Regex;
use serde::Serialize;

use crate::error::{Error, Result};
use crate::record::Record;
use crate::store::Store;
use crate::util::now_iso;

/// Extract `[[id]]` wiki-link targets from a body (`[[id|alias]]` → `id`).
pub fn wikilinks(body: &str) -> Vec<String> {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| Regex::new(r"\[\[([^\[\]]+)\]\]").expect("valid wikilink regex"));
    re.captures_iter(body)
        .filter_map(|c| {
            let raw = c.get(1)?.as_str();
            let target = raw.split('|').next().unwrap_or(raw).trim();
            (!target.is_empty()).then(|| target.to_string())
        })
        .collect()
}

/// All outbound edges of a record: frontmatter `links` ∪ body `[[id]]`, minus self-loops.
pub fn edges(record: &Record) -> BTreeSet<String> {
    let mut set: BTreeSet<String> = record.meta.links.iter().cloned().collect();
    set.extend(wikilinks(&record.body));
    set.remove(&record.meta.id);
    set
}

/// A node reached while traversing the graph.
#[derive(Debug, Clone, Serialize)]
pub struct Neighbor {
    pub id: String,
    /// Hops from the start node (1 = directly linked).
    pub depth: usize,
    /// Whether a record with this id exists in the bundle (`false` = dangling link).
    pub exists: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// Breadth-first traversal from `start` over edges, up to `depth` hops. Returns reached nodes
/// (excluding `start`), nearest-first; dangling targets appear with `exists: false`.
pub fn neighbors(records: &[Record], start: &str, depth: usize) -> Vec<Neighbor> {
    let by_id: HashMap<&str, &Record> = records.iter().map(|r| (r.meta.id.as_str(), r)).collect();

    let mut seen: HashSet<String> = HashSet::from([start.to_string()]);
    let mut frontier: VecDeque<(String, usize)> = VecDeque::from([(start.to_string(), 0)]);
    let mut out = Vec::new();

    while let Some((id, d)) = frontier.pop_front() {
        if d >= depth {
            continue;
        }
        let Some(record) = by_id.get(id.as_str()) else {
            continue; // a dangling node has no outbound edges
        };
        for target in edges(record) {
            if seen.insert(target.clone()) {
                let found = by_id.get(target.as_str());
                out.push(Neighbor {
                    id: target.clone(),
                    depth: d + 1,
                    exists: found.is_some(),
                    description: found.map(|r| r.meta.description.clone()),
                });
                frontier.push_back((target, d + 1));
            }
        }
    }
    out
}

/// Create a **mutual** link between two existing records (idempotent). Bumps `updated` on any
/// record actually changed. Errors if either id is missing or the two ids are equal.
pub fn link(store: &dyn Store, a: &str, b: &str) -> Result<()> {
    if a == b {
        return Err(Error::Graph(format!("cannot link `{a}` to itself")));
    }
    mutate_pair(store, a, b, add_link)
}

/// Remove the mutual link between two records (idempotent). Body `[[id]]` wiki-links are not
/// touched — only the frontmatter `links` field.
pub fn unlink(store: &dyn Store, a: &str, b: &str) -> Result<()> {
    if a == b {
        return Ok(());
    }
    mutate_pair(store, a, b, remove_link)
}

/// Apply `op` to both records' `links` (each toward the other), persisting only those changed.
fn mutate_pair(
    store: &dyn Store,
    a: &str,
    b: &str,
    op: fn(&mut Record, &str) -> bool,
) -> Result<()> {
    let mut ra = store.get(a)?;
    let mut rb = store.get(b)?;
    if op(&mut ra, b) {
        ra.meta.updated = now_iso();
        store.put(&ra)?;
    }
    if op(&mut rb, a) {
        rb.meta.updated = now_iso();
        store.put(&rb)?;
    }
    Ok(())
}

/// Add `target` to a record's `links` (kept sorted + deduped). Returns whether it changed.
fn add_link(record: &mut Record, target: &str) -> bool {
    if record.meta.links.iter().any(|l| l == target) {
        return false;
    }
    record.meta.links.push(target.to_string());
    record.meta.links.sort();
    true
}

/// Remove `target` from a record's `links`. Returns whether it changed.
fn remove_link(record: &mut Record, target: &str) -> bool {
    let before = record.meta.links.len();
    record.meta.links.retain(|l| l != target);
    record.meta.links.len() != before
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::record::{MemoryType, RecordMeta};

    fn rec(id: &str, links: &[&str], body: &str) -> Record {
        let mut m = RecordMeta::new(id, MemoryType::Semantic, "d", "2026-06-19T00:00:00Z");
        m.links = links.iter().map(|s| s.to_string()).collect();
        Record::new(m, body)
    }

    #[test]
    fn wikilinks_extracts_targets_and_aliases() {
        assert_eq!(
            wikilinks("see [[alpha]] and [[beta|the beta note]] plus [[]]"),
            vec!["alpha", "beta"]
        );
    }

    #[test]
    fn edges_union_frontmatter_and_body_minus_self() {
        let r = rec("self", &["a"], "links [[b]] and [[self]]");
        let e = edges(&r);
        assert!(e.contains("a") && e.contains("b"));
        assert!(!e.contains("self"), "self-loop must be dropped");
    }

    #[test]
    fn neighbors_bfs_by_depth_and_marks_dangling() {
        let corpus = vec![
            rec("a", &["b"], ""),
            rec("b", &["c"], "see [[gone]]"),
            rec("c", &[], ""),
        ];
        let d1 = neighbors(&corpus, "a", 1);
        assert_eq!(d1.iter().map(|n| n.id.as_str()).collect::<Vec<_>>(), ["b"]);

        let d2 = neighbors(&corpus, "a", 2);
        let ids: Vec<_> = d2
            .iter()
            .map(|n| (n.id.as_str(), n.depth, n.exists))
            .collect();
        assert!(ids.contains(&("b", 1, true)));
        assert!(ids.contains(&("c", 2, true)));
        assert!(
            ids.contains(&("gone", 2, false)),
            "dangling target must appear, exists=false"
        );
    }

    #[test]
    fn add_and_remove_link_are_idempotent() {
        let mut r = rec("x", &[], "");
        assert!(add_link(&mut r, "y"));
        assert!(!add_link(&mut r, "y"));
        assert_eq!(r.meta.links, vec!["y"]);
        assert!(remove_link(&mut r, "y"));
        assert!(!remove_link(&mut r, "y"));
        assert!(r.meta.links.is_empty());
    }
}
