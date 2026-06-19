# S3Mem — OKF memory over S3

Portable, vendor-neutral **memory for AI agents**. Each memory is a plain Markdown note
(YAML frontmatter + body) stored one-per-file under a local directory **or** an S3 prefix.
Recall it by relevance (BM25) or by exact pattern (grep). Because a whole memory is just
files under a prefix, you can **ship it around** — `aws s3 sync` it, tar it, or commit it to
git — with no database and no lock-in.

It combines two existing ideas:

- **[OKF](https://github.com/GoogleCloudPlatform/knowledge-catalog/tree/main/okf)** (Open
  Knowledge Format) — knowledge as Markdown + YAML frontmatter, one concept per file, with a
  generated `index.md` and `[[links]]` graph edges. Git-native, readable without an SDK.
- **[s3grep](https://github.com/dacort/s3grep)** — searching content directly across S3
  objects. S3Mem's `grep` path is the same idea, applied to memory.

> **Status:** early but functional. The format layer, both backends (local FS + S3), the
> recall layer (BM25 + grep), the `s3mem` CLI, and an agent skill all work and are tested
> (incl. a verified live S3 round-trip). A cached recall index and graph-link tooling are the
> main pieces still to come — see [Roadmap](#roadmap).

## Install

Requires a Rust toolchain.

```bash
cargo build --release --features cli
export PATH="$PWD/target/release:$PATH"
```

Both backends (local filesystem and S3) are always built in. The one feature flag is `cli`
(pulls in clap to build the `s3mem` binary) — library users who only want the API can leave
it off.

## Quickstart (CLI)

Point the CLI at a bundle via environment variables, then remember and recall:

```bash
export S3MEM_PATH=~/.s3mem            # local backend …
# export S3MEM_BUCKET=my-bucket       # … or S3 backend (needs --features s3)
export S3MEM_NAMESPACE=my-agent       # a bundle per agent/project

s3mem remember --id user-deploy-pref --type semantic \
  --description "User deploys via terraform, never the console" --tag ops \
  --body "Runs 'terraform apply' from CI."

s3mem recall "how does the user ship code" --pretty   # ranked (BM25)
s3mem grep  "terraform"                                # exact / regex
s3mem get   user-deploy-pref                           # full note
s3mem list                                             # all ids
s3mem forget user-deploy-pref
```

`recall` and `grep` print **JSON by default** (easy to parse from an agent); `--pretty` gives
human output. Both accept `--type` and `--tag` filters.

## Quickstart (library)

```rust
use s3mem::{bm25, Filter, LocalStore, MemoryType, Record, RecordMeta, Store, now_iso};

let store = LocalStore::new("/tmp/mem", "my-agent");

let mut meta = RecordMeta::new(
    "user-deploy-pref", MemoryType::Semantic,
    "User deploys via terraform, never the console", now_iso(),
);
meta.tags = vec!["ops".into()];
store.put(&Record::new(meta, "Runs `terraform apply` from CI."))?;

// Recall ranks the in-memory corpus the store hands back.
let records = store.records()?;
for hit in bm25(&records, "how does the user ship code", &Filter::default(), 5) {
    println!("{}  ({:.2})", hit.id, hit.score.unwrap());
}
```

Swap `LocalStore` for `S3Store::new("my-bucket", "my-agent")?` and nothing else changes —
both implement the same `Store` trait.

## The data model

One memory is one Markdown file: filterable fields up top, the fact in the body.

```markdown
---
id: user-deploy-pref          # stable slug; also the storage key
type: semantic                # semantic | episodic | procedural | reference
description: User deploys via terraform, never the console
tags:
- ops
created: 2026-06-19T20:53:22Z
updated: 2026-06-19T20:53:22Z
---

Runs `terraform apply` from CI. Relate to other notes with [[other-id]].
```

A **bundle** is everything under a namespace — plain files you can read, diff, and copy:

```
<root | s3://bucket>/<namespace>/
  memories/<id>.md     one OKF record per file/object
  manifest.json        derived digest of every note's frontmatter
  index.md             derived, human/agent-navigable entry point
```

`manifest.json` and `index.md` are **derived on every write** and never authoritative — the
notes are the source of truth, so a hand-dropped or hand-edited note can never corrupt the
bundle.

## Recall: two paths

| Tool                  | What it does                              | Reach for it when…                                  |
|-----------------------|-------------------------------------------|-----------------------------------------------------|
| `recall` (**BM25**)   | ranked relevance, best-first              | fuzzy, natural-language lookups: *"what do I know about X"* |
| `grep`                | literal or `--regex` match, with snippets | you know the exact token, identifier, or pattern    |

BM25 is hand-rolled (no search-engine dependency) and field-weighted — a hit in the one-line
`description` outranks the same word buried in a long body. Both paths first apply a cheap
frontmatter filter (`--type` / `--tag`) and return small hits (`id`, `description`, score,
snippet) so an agent can triage, then `get` only the notes it wants.

Recall is served from a **cached index** (`recall-index.json` in the bundle): a precomputed
BM25 index that turns recall into a single fetch instead of one read per memory. It's
validated by a cheap content fingerprint (a hash of the `memories/` listing — file
size/mtime locally, object ETag on S3), so it's rebuilt automatically when the bundle
changes, including out-of-band edits. The cache is transparent — results are identical to the
uncached path.

## Linking memories

Memories form a graph. `link` records a **mutual** edge in frontmatter; you can also write
`[[other-id]]` wiki-links in a body (directional, the OKF convention). `neighbors` walks the
graph so an agent can pull a connected cluster of context at once.

```bash
s3mem link user-deploy-pref ci-pipeline      # mutual edge
s3mem links user-deploy-pref                 # direct neighbors
s3mem neighbors user-deploy-pref --depth 2   # everything within 2 hops
s3mem unlink user-deploy-pref ci-pipeline
```

Traversal tolerates dangling links (a `[[id]]` with no record shows as `exists: false`), so
the graph never breaks when a memory is forgotten.

## For agents: the skill

[`skills/s3mem-memory/SKILL.md`](skills/s3mem-memory/SKILL.md) wraps the CLI as an agent
skill: it tells the agent how to point at a bundle and **when to use `recall` (fuzzy/ranked)
vs `grep` (exact/regex)**, plus how to remember and manage memories. Drop it into an agent's
skills directory and the two recall paths become tools it can call.

## Architecture

Five layers, each independent of the ones above it:

```
Skill / CLI ──  s3mem remember · recall · grep · get · list · forget
Recall ──────  bm25() ranked  +  grep() literal/regex  (over Store::records())
Store ───────  put/get/list/delete/manifest/records — LocalStore | S3Store
Index ───────  manifest.json + index.md (derived on write; a cache, not truth)
Format (OKF) ─ Record: frontmatter + body; parse ⇄ to_markdown (byte-faithful)
```

The core invariant — **a memory is a file; a bundle is a prefix** — is what makes "ship it
around" real: everything above the Store layer is backend-agnostic, so a bundle is
byte-for-byte identical on local disk and on S3. Anything that can't survive a
copy-to-filesystem (an embedded DB, a vendor API, a non-portable index) stays in an optional
layer, never the core.

Working in this repo? See [CLAUDE.md](CLAUDE.md) for build commands, the source map, and the
invariants worth preserving.

## Roadmap

- **S3 Select prefilter** — the cached index makes recall a single fetch, but for very large
  S3 bundles even that one object grows; pushing the `type`/`tag` prefilter down to S3 Select
  would avoid fetching the whole index.
- **Optional vector recall** — an embedding-ranked stage layered on BM25, kept optional so it
  never becomes a hard dependency that breaks portability.
- **Referential cleanup** — `forget` currently leaves dangling links (traversal tolerates
  them); a `gc`/prune pass to drop edges to deleted records is a nice-to-have.

## License

Dual-licensed under MIT or Apache-2.0 (as declared in `Cargo.toml`).
