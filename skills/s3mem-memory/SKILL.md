---
name: s3mem-memory
description: Use to store and recall durable agent memory (OKF markdown notes over a filesystem or S3). Recall with BM25 ranking for fuzzy/topic lookups, or grep for exact tokens and regex. Also remember/get/list/forget memories.
---

# s3mem memory

A portable memory store: each memory is an OKF markdown note (YAML frontmatter + body) under
a filesystem directory or an S3 prefix. You interact with it through the `s3mem` CLI, which
prints JSON by default so you can parse results.

## Setup (once per session)

The CLI reads the backend and namespace from the environment — set these before using it:

```bash
# Local filesystem backend:
export S3MEM_PATH=/path/to/memory-root
# …or S3 backend (binary must be built with --features cli,s3):
export S3MEM_BUCKET=my-bucket
export S3MEM_PREFIX=optional/key/prefix      # optional

export S3MEM_NAMESPACE=this-agent            # a bundle per agent/project (default: "default")
```

Build the binary if it isn't already on PATH:
`cargo build --release --features cli,s3` → `target/release/s3mem`.

## The two recall tools — when to use which

**`s3mem recall "<query>"` — ranked relevance (BM25).** Your default. Use for fuzzy,
natural-language questions where you want the *most relevant* memories, even without exact
word matches. Results are score-ordered.

```bash
s3mem recall "what does the user prefer for deployments" --k 5
s3mem recall "rust async runtime" --type semantic --tag lang
```

**`s3mem grep "<pattern>"` — exact / regex match (precise).** Use when you know the literal
string: an identifier, error code, file name, config key. Default is literal & case-insensitive;
add `--regex` for patterns, `-s` for case-sensitive.

```bash
s3mem grep "DEPLOY_KEY"
s3mem grep --regex "v[0-9]+\.[0-9]+\.[0-9]+"
s3mem grep "terraform" --type procedural
```

Both accept `--type <semantic|episodic|procedural|reference>` (repeatable) and `--tag <tag>`
(repeatable, AND) to prefilter, and `--pretty` for human output instead of JSON.

**Recall returns small hits** (`id`, `type`, `description`, `score`, `snippets`) — triage on the
snippet, then read the full memory only for the ones you want:

```bash
s3mem get <id>     # prints the full OKF markdown
```

## Writing and managing memories

```bash
# Remember (body via --body or piped on stdin):
s3mem remember --id user-deploy-pref --type semantic \
  --description "User deploys via terraform, never the console" --tag ops --tag deploy \
  --body "The user runs `terraform apply` from CI; never click-ops in the AWS console."

echo "Long body text…" | s3mem remember --id some-id --type episodic --description "…"

s3mem list                 # all memory ids
s3mem forget <id>          # delete a memory
```

`--id` must be a stable slug (`[A-Za-z0-9._-]`, no `.`/`..`). Re-`remember`ing the same id
overwrites it; bump your own `updated` understanding — the store does not auto-stamp it.

## When to reach for this skill

- **Before answering from assumptions about the user/project** — `recall` first; the memory may
  already hold the answer.
- **After learning a durable fact** (a preference, a decision, a recurring gotcha) — `remember`
  it so future sessions benefit.
- Prefer `recall` for "what do I know about X"; switch to `grep` when you need an exact string.

## Notes

- Each `recall`/`grep` reloads the whole bundle, so it's always current — no stale index. This
  is fast for local bundles and fine for hundreds–thousands of S3 notes; very large S3 bundles
  will be slower (one GET per object).
- A bundle is just files/objects: copy the directory or `aws s3 sync` the prefix to ship a whole
  memory to another machine or agent.
