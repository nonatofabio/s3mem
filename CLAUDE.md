# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Status: working core (Rust, early)

The crate ships the **format layer** ([`Record`]), the **local-filesystem backend**
([`LocalStore`]), the **S3 backend** ([`S3Store`], behind the `s3` feature) — both behind the
same [`Store`] trait — the **recall layer** (BM25 + grep), the **`s3mem` CLI** (`cli` feature),
and an agent **skill** (`skills/s3mem-memory/`). A persisted/cached recall index is the main
piece not yet built. The architecture section is the design of record — build outward from here
and keep this file current as tooling lands.

### Commands

```bash
cargo test               # core suite (unit + integration + doctests), no AWS deps
cargo test --features s3                  # also builds S3 backend + its key tests
cargo build --features cli                # build the `s3mem` CLI (local backend only)
cargo build --release --features cli,s3   # build the CLI with S3 support (for the skill)
cargo test --test recall                  # recall integration tests over a LocalStore
cargo test round_trips_through_markdown   # a single test by name
cargo clippy --all-targets --features cli,s3   # lints (kept clean across feature sets)
cargo fmt                                 # format (rustfmt)

# Live S3 round-trip (skipped unless a bucket + AWS creds are present):
S3MEM_TEST_BUCKET=my-bucket cargo test --features s3 --test s3_store -- --nocapture

# Drive the CLI:
S3MEM_PATH=/tmp/mem S3MEM_NAMESPACE=agent target/debug/s3mem recall "rust deploy" --pretty
```

### Source layout

```
src/
  lib.rs            crate root + re-exports + doctest of the happy path
  record.rs         OKF Record / RecordMeta / MemoryType — parse + to_markdown
  store.rs          Store trait (incl. records()) + Manifest/ManifestEntry
  error.rs          Error enum (thiserror), Result alias
  util.rs           now_iso() RFC-3339 timestamp helper
  recall/
    mod.rs          Filter + Hit; re-exports bm25 / grep
    bm25.rs         hand-rolled Okapi BM25 (field-weighted), ranked recall
    grep.rs         literal/regex match with line-numbered snippets
    tokenize.rs     shared tokenizer + snippet truncation
  backend/
    mod.rs          backend wiring (common + local, s3 under cfg)
    common.rs       SHARED key rules: validate_id, encode_id, safe_segments (parity-critical)
    local.rs        LocalStore — Store over a directory
    s3.rs           S3Store — Store over S3 objects (feature = "s3")
  bin/s3mem.rs      the `s3mem` CLI (feature = "cli"): remember/recall/grep/get/list/forget
skills/s3mem-memory/SKILL.md   agent skill wrapping the CLI (when to use recall vs grep)
tests/local_store.rs  integration tests against a temp-dir bundle
tests/qa_probes.rs    adversarial corner-case suite (see QA_FINDINGS.md)
tests/recall.rs       recall over a real LocalStore corpus
tests/s3_store.rs     live S3 round-trip, gated on S3MEM_TEST_BUCKET (feature = "s3")
```

Key implementation notes for future work:
- **Recall is a layer over `Store`, not part of it.** `bm25`/`grep` are pure functions over a
  `&[Record]` slice (from `Store::records()`), so they're backend-agnostic and unit-testable
  without any store. Both run a cheap frontmatter `Filter` (type/tags) first — that's where an
  S3 Select prefilter would plug in later.
- **BM25 is hand-rolled** (`recall/bm25.rs`, k1=1.2/b=0.75), field-weighted by repeating tokens
  (description ×3, id ×2, tags ×2, body ×1). No search-engine dependency; corpus is loaded and
  scored in memory per call. Fine to thousands of notes; a cached/persisted index is the next
  step for very large S3 bundles (every `recall` currently does one GET per object).
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
- The S3 backend is **off by default** to keep the core crate AWS-free; gate any future
  AWS-only code behind `#[cfg(feature = "s3")]`.
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

See [`QA_FINDINGS.md`](QA_FINDINGS.md) and [`tests/qa_probes.rs`](tests/qa_probes.rs) for the
corner cases these invariants defend (all 25 probes pass).

## Architecture (as built)

Concept, pitch, and the OKF/s3grep lineage live in the [README](README.md). This is the
implemented shape — five layers, each independent of the ones above it:

```
Skill / CLI ──  s3mem remember · recall · grep · get · list · forget   (bin/s3mem.rs, skills/)
Recall ──────  bm25() ranked  +  grep() literal/regex, over Store::records()   (recall/)
Store ───────  put/get/list/delete/manifest/records — LocalStore | S3Store     (backend/)
Index ───────  manifest.json + index.md, derived on every write (cache, not truth)
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

- **Cached recall index.** Recall loads the whole bundle per call (one GET per object on S3) —
  always fresh and correct, but a persisted BM25 index (rebuilt on manifest staleness) and/or
  an S3 Select frontmatter prefilter is the next step for large S3 bundles.
- **Graph edges.** The `links` field exists in the format but nothing walks or ships the graph
  yet (no `link`/`export` tooling).
- **Optional vector recall.** An embedding-ranked stage layered on BM25 — keep it optional; a
  hard embedding dependency would break the portability guarantee.
