# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Status: early scaffold (Rust)

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
corner cases these invariants defend (all 13 probes now pass).

## The idea (from README)

**S3Mem = OKF memory over S3.** A portable, vendor-neutral memory store for agents, built
by combining two existing ideas:

- **OKF** ([Open Knowledge Format](https://github.com/GoogleCloudPlatform/knowledge-catalog/tree/main/okf)) —
  knowledge as plain **markdown + YAML frontmatter**, one concept per file, `index.md` for
  navigation, markdown `[[links]]` as graph edges. Git-native, no SDK required to read.
- **s3grep** ([dacort/s3grep](https://github.com/dacort/s3grep)) — fast **parallel, concurrent
  search across S3 objects** (handles `.gz`, configurable concurrency).

The pitch: an agent's memory is **just OKF files living under an S3 prefix**. Because each
memory is a plain object, a whole memory can be **shipped around** — copied to another
bucket, tarballed, or synced to a filesystem/git repo — with no database and no lock-in.

## Architecture

Five layers, each independently replaceable. The core invariant: **a memory is a file; a
memory bundle is a prefix.** Nothing above the Store layer requires S3 specifically — swap
in a local filesystem and everything still works (that's the portability claim).

```
┌─ Agent interface ─┐  remember() · recall() · forget() · link() · export()/ship()
├─ Search / recall ─┤  frontmatter filter (S3 Select) → parallel content grep → rank → top-k
├─ Index ───────────┤  auto-generated index.md + manifest.json (frontmatter digest)
├─ Store ───────────┤  S3 object layout (or local FS) — pluggable backend
└─ Format (OKF) ────┘  markdown + YAML frontmatter, one memory per file
```

### 1. Format layer (OKF record)
One memory = one markdown file. Frontmatter carries the structured, filterable fields; the
body carries the fact and its relationships. Suggested schema:

```markdown
---
id: <stable-kebab-slug>          # also the object key stem
type: semantic | episodic | procedural | reference
description: <one-line summary>   # used for cheap relevance ranking
tags: [<topic>, ...]
created: <ISO-8601>
updated: <ISO-8601>
source: <where it came from>
links: [<other-id>, ...]          # graph edges, mirrored as [[id]] in body
---

<the memory>. Relate to others with [[other-id]].
```

Keep frontmatter fields flat and stable — the Search layer filters on them via S3 Select,
so adding/renaming required fields is a format-migration concern.

### 2. Store layer (pluggable backend)
Object layout — a **bundle** is everything under `<namespace>/`:

```
s3://<bucket>/<namespace>/
  memories/<id>.md        # one OKF record per object
  index.md                # human/agent-navigable, auto-generated
  manifest.json           # machine index: id → {description, type, tags, updated, key}
```

`<namespace>` scopes a bundle (per-agent, per-project, per-user). The backend is an
interface (`get`/`put`/`list`/`delete`/`select`) with at least an **S3** and a **local
filesystem** implementation so bundles round-trip between cloud and disk unchanged.

### 3. Index layer
Avoid scanning every object on every recall. Maintain two derived artifacts, rebuilt on
write (or lazily):
- `manifest.json` — a compact digest of every record's frontmatter. Cheap to fetch whole;
  lets recall pre-filter candidates before touching object bodies.
- `index.md` — the OKF navigation entrypoint, regenerated from the manifest.

The manifest is a **cache, never the source of truth** — it must be reconstructable by
listing + reading the `memories/` objects.

### 4. Search / recall layer
Two-stage retrieval — this is where s3grep's idea lives:
1. **Filter** by structured fields (type/tags/recency) using **S3 Select** over
   `manifest.json`, or a manifest read on small bundles → candidate key set.
2. **Content search** the candidate object bodies with s3grep-style **parallel concurrent
   scanning** (regex/keyword). Rank by description match + recency + tag overlap, return
   top-k **full OKF documents** for the agent to load into context.

Start with lexical search (grep). Vector/embedding recall is a later, optional ranking
stage layered on top — don't let it become a hard dependency, it breaks portability.

### 5. Agent interface layer
The surface agents actually call. Each verb maps to layers below:
- `remember(fact, meta)` → write OKF record (Format) → PUT (Store) → update index (Index)
- `recall(query, filters, k)` → two-stage retrieval (Search) → top-k records
- `forget(id)` → delete object + reindex
- `link(a, b)` → add edge to both records' frontmatter + body
- `export(namespace)` / `ship(src, dst)` → copy/sync a prefix to another backend or tarball

### "Shipped around" — the whole point
Because a bundle is a self-contained prefix of plain files: `ship` is `aws s3 sync`,
`s3 cp --recursive`, a `.tar.gz`, or a git push. No export format to design, no DB to dump.
A shipped bundle is readable by humans, by Obsidian/MkDocs, and by any other agent — that
portability is the product, so guard it: any feature that can't survive a copy-to-filesystem
(proprietary index, embedded DB, vendor API) belongs in an optional layer, never the core.

## When building this out

- **Language is unchosen.** s3grep is Rust; if recall throughput matters, a Rust core fits.
  For fastest agent-framework integration, a Python or Go CLI/library is the pragmatic pick.
  Decide based on whether this is primarily a *library agents import* or a *standalone tool*.
- **Backend interface first.** Build the local-filesystem backend before S3 — it makes the
  whole thing testable without AWS and proves the portability invariant by construction.
- **Don't skip the manifest.** Scanning every object per recall is the obvious trap; the
  index layer is what keeps recall cheap as bundles grow.
