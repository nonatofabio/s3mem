# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Status: early scaffold (Rust)

The crate ships the **format layer** ([`Record`]) and the **local-filesystem backend**
([`LocalStore`]) from the architecture below. The S3 backend, search/recall, and an agent
CLI are not built yet. The architecture section is the design of record — build outward
from here and keep this file current as tooling lands.

### Commands

```bash
cargo test               # unit + integration + doctests
cargo test --test local_store           # just the local-FS integration tests
cargo test round_trips_through_markdown # a single test by name
cargo clippy --all-targets              # lints (kept clean)
cargo fmt                               # format (rustfmt)
```

### Source layout

```
src/
  lib.rs            crate root + re-exports + doctest of the happy path
  record.rs         OKF Record / RecordMeta / MemoryType — parse + to_markdown
  store.rs          Store trait + Manifest/ManifestEntry (derived index)
  error.rs          Error enum (thiserror), Result alias
  util.rs           now_iso() RFC-3339 timestamp helper
  backend/
    mod.rs          backend module (reference backend now; S3 later)
    local.rs        LocalStore — Store over a directory
tests/local_store.rs  integration tests against a temp-dir bundle
```

Key implementation notes for future work:
- `Store` is the seam. The S3 backend is a new sibling under `backend/` implementing the
  same trait — callers don't change. Keep S3-only concerns out of `store.rs`.
- `manifest.json` + `index.md` are **derived on every write** (`LocalStore::reindex`) and
  are never authoritative — `manifest()` rebuilds them by reading the records.
- Record ids are filenames, so `validate_id` rejects path traversal — preserve that in any
  new backend.

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
