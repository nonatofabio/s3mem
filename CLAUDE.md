# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Status: working core (Rust, early)

The crate ships the **format layer** ([`Record`]), the **local-filesystem and S3 backends**
([`LocalStore`] / [`S3Store`], both always compiled, behind the same [`Store`] trait), the
**recall layer** (BM25 + grep, with a fingerprint-gated cached index), the **link graph**
(`graph.rs`), the **`s3mem` CLI** (`cli` feature), and an agent **skill**
(`skills/s3mem-memory/`). The architecture section is the design of record — build outward
from here and keep this file current as tooling lands.

### Commands

Both backends are always compiled (S3 is no longer feature-gated), so plain `cargo test`
builds the AWS SDK. The only feature is `cli` (clap, for the binary).

```bash
cargo test                                # full suite (both backends always built in)
cargo build --release --features cli      # build the `s3mem` CLI
cargo test --test recall                  # recall integration tests over a LocalStore
cargo test round_trips_through_markdown   # a single test by name
cargo clippy --all-targets --features cli # lints (kept clean)
cargo fmt                                 # format (rustfmt)

# Live S3 round-trip (skipped unless a bucket + AWS creds are present):
S3MEM_TEST_BUCKET=my-bucket cargo test --test s3_store -- --nocapture

# Drive the CLI:
S3MEM_PATH=/tmp/mem S3MEM_NAMESPACE=agent target/debug/s3mem neighbors my-id --depth 2
```

### Source layout

```
src/
  lib.rs            crate root + re-exports + doctest of the happy path
  record.rs         OKF Record / RecordMeta / MemoryType — parse + to_markdown
  store.rs          Store trait (incl. records()) + Manifest/ManifestEntry
  graph.rs          link/unlink (mutual edges) + neighbors BFS + [[id]] wikilinks
  error.rs          Error enum (thiserror), Result alias
  util.rs           now_iso() RFC-3339 timestamp helper
  recall/
    mod.rs          Filter + Hit; re-exports bm25 / grep / Index
    score.rs        shared BM25 math (weights, rank, snippet) — one impl for both paths
    bm25.rs         uncached Okapi BM25 over a &[Record]
    grep.rs         literal/regex match with line-numbered snippets
    index.rs        cached BM25 index (recall-index.json) + load_or_build_index
    tokenize.rs     tokenizer (CJK-aware: unigrams+bigrams) + snippet truncation
  backend/
    mod.rs          backend wiring (common + local, s3 under cfg)
    common.rs       SHARED key rules: validate_id, encode_id, safe_segments (parity-critical)
    local.rs        LocalStore — Store over a directory
    s3.rs           S3Store — Store over S3 objects (feature = "s3")
  bin/s3mem.rs      the `s3mem` CLI (feature = "cli"): remember/recall/grep/get/list/forget
                    + link/unlink/links/neighbors
skills/s3mem-memory/SKILL.md   agent skill wrapping the CLI (when to use recall vs grep)
tests/local_store.rs  integration tests against a temp-dir bundle
tests/qa_probes.rs    adversarial corner-case suite (see QA_FINDINGS.md)
tests/recall_probes.rs  adversarial recall-layer probes (see QA_FINDINGS.md)
tests/recall.rs       recall over a real LocalStore corpus
tests/cache.rs        cached-index lifecycle (write/reuse/self-heal/transparency)
tests/graph.rs        link/unlink/neighbors over a real LocalStore
tests/s3_store.rs     live S3 round-trip, gated on S3MEM_TEST_BUCKET
```

Key implementation notes for future work:
- **Recall is a layer over `Store`, not part of it.** `bm25`/`grep` are pure functions over a
  `&[Record]` slice (from `Store::records()`), so they're backend-agnostic and unit-testable
  without any store. Both run a cheap frontmatter `Filter` (type/tags) first — that's where an
  S3 Select prefilter would plug in later. The BM25 math lives once in `recall/score.rs`, used
  by both the uncached `bm25()` and the cached `Index` so their rankings can't drift.
- **BM25 is hand-rolled** (k1=1.2/b=0.75), field-weighted by repeating tokens (description ×3,
  id ×2, tags ×2, body ×1). No search-engine dependency.
- **The cached index is the recall hot path.** `load_or_build_index` reads `recall-index.json`
  and uses it when `Store::fingerprint()` still matches, else rebuilds from `records()` and
  re-caches (self-healing on out-of-band edits). Building stays in the recall layer (lazy, on
  read) so **backends don't depend on recall** — they only expose generic `fingerprint`,
  `read_artifact`, `write_artifact`. `fingerprint()` must stay cheap (listing metadata only —
  size/mtime locally, ETag on S3 — never body reads) and must exclude derived artifacts (it
  scans `memories/` only), or writing the cache would change the fingerprint and loop. Bump
  `INDEX_VERSION` if the index format or scoring weights change.
- **Recall results must be backend/cache-transparent** — `Index::search` recomputes df/avgdl
  over the filtered candidates exactly like `bm25()`, so cached == uncached (a test pins this).
- The CLI prints **JSON by default** for the recall tools (agent-parseable), `--pretty` for
  humans. Backend/namespace come from `S3MEM_PATH`/`S3MEM_BUCKET`/`S3MEM_NAMESPACE` env vars.
- `Store` is the seam, and it's **synchronous**. The AWS SDK is async, so `S3Store` owns a
  Tokio runtime and bridges each call with `block_on` — this keeps the trait, the local
  backend, and all callers sync. Don't asyncify the trait to accommodate S3.
- **Backend parity is load-bearing.** `validate_id`, `encode_id`, and namespace containment
  live in `backend/common.rs` and are used identically by both backends, so a bundle written
  locally is byte-for-byte the same on S3 (and vice versa) — that's the "ship it around"
  promise. Any third backend MUST reuse `common.rs`, not reinvent key rules.
- `get`/`delete` resolve a record by its **frontmatter id**: a fast path at the canonical
  `encode_id(id)` key, then a fallback scan for files/objects whose on-disk name diverges
  (renames, hand-edits). Keep `get`/`delete` consistent with what `list`/`manifest` report.
- **Both backends are always compiled** (S3 is not feature-gated; the AWS SDK is a normal
  dependency). The only feature is `cli`.
- **The graph layer (`graph.rs`) sits beside recall, over `Store`.** `neighbors`/`edges`/
  `wikilinks` are pure over `&[Record]`; `link`/`unlink` read-modify-write through `Store` and
  bump `updated` (the right place for the auto-stamp the store itself omits). Edges are the
  union of frontmatter `links` (mutual) and body `[[id]]` (directional); traversal tolerates
  dangling targets. Mutations touch only `links`, never the body prose.
- `manifest.json` + `index.md` are **derived on every write** (`LocalStore::reindex`) and
  are never authoritative. `manifest()`/`list()` go through `read_entries`, which **skips +
  warns on unparseable files** rather than failing the whole bundle — a stray non-OKF `.md`
  must never brick writes. Ids come from each record's frontmatter, not the filename.
- **Two safety boundaries, symmetric:** record ids go through `validate_id` (allowlist:
  `[A-Za-z0-9._-]`, never `.`/`..`), and the `namespace` is contained by `bundle_dir`
  (traversal components dropped) so neither can escape the bundle root. Preserve both in any
  new backend.
- **On-disk filenames are `encode_id(id)`** (uppercase letters percent-escaped to lowercase
  hex), so case-only-distinct ids (`Alpha` vs `alpha`) don't collide on case-insensitive
  filesystems — the "ship a bundle around unchanged" claim must hold across backends. S3 is
  case-sensitive, so apply the same encoding there for cross-backend parity.
- **Format round-trip is byte-faithful:** `Record::parse` is the exact inverse of
  `to_markdown` (leading whitespace, CRLF, and unknown frontmatter fields all preserved).
  Unknown frontmatter is captured in `RecordMeta.extra` (`#[serde(flatten)]`) so hand-edits
  aren't silently dropped. Don't reintroduce body trimming or `.lines()`-based body parsing.
- **The store never stamps `updated`** — callers must bump it themselves before `put`. If a
  higher-level `remember()` API lands, that's where the auto-stamp belongs.

See [`QA_FINDINGS.md`](QA_FINDINGS.md), [`tests/qa_probes.rs`](tests/qa_probes.rs), and
[`tests/recall_probes.rs`](tests/recall_probes.rs) for the corner cases these invariants defend
(all 33 format/store + 9 recall probes pass).

## Architecture (as built)

Concept, pitch, and the OKF/s3grep lineage live in the [README](README.md). This is the
implemented shape — five layers, each independent of the ones above it:

```
Skill / CLI ──  s3mem remember·recall·grep·get·list·forget·link·neighbors  (bin/s3mem.rs, skills/)
Recall + graph  bm25()/Index + grep() (recall-index.json) · link/neighbors  (recall/, graph.rs)
Store ───────  put/get/list/delete/manifest/records/fingerprint/artifact       (backend/)
Index ───────  manifest.json + index.md (write) · recall-index.json (lazy, fingerprint-gated)
Format (OKF) ─ Record: frontmatter + body; parse ⇄ to_markdown                 (record.rs)
```

The core invariant — **a memory is a file; a bundle is a prefix**
(`<root | s3://bucket>/<namespace>/`) — is what makes the portability claim real: everything
above the Store layer is backend-agnostic, so a bundle is byte-for-byte identical on disk and
on S3. Guard it — anything that can't survive a copy-to-filesystem (an embedded DB, a vendor
API, a non-portable index) belongs in an optional layer, never the core. The "Key
implementation notes" above are the specific invariants that protect this; read them before
editing a backend or the format.

## Not yet built (roadmap)

- **S3 Select prefilter.** The cached index makes recall a single fetch, but a very large
  bundle's index object itself grows; pushing the `type`/`tag` prefilter down to S3 Select
  would avoid fetching the whole index.
- **Optional vector recall.** An embedding-ranked stage layered on BM25 — keep it optional; a
  hard embedding dependency would break the portability guarantee.
- **CJK recall** is unigram+bigram (`recall/tokenize.rs`); a proper Unicode word segmenter
  would improve precision if non-Latin memory becomes common.
- **Referential cleanup.** `delete`/`forget` leaves dangling `links` in other records
  (traversal tolerates them); a prune/`gc` pass is a nice-to-have.
