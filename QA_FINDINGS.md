# S3Mem — QA corner-case findings

Adversarial QA against the early scaffold (format layer + `LocalStore`). Goal: disprove the
assumptions the README/architecture make about the format, the store, and the "ship a bundle
around unchanged" portability claim.

**Method.** Probes live in [`tests/qa_probes.rs`](tests/qa_probes.rs). Each asserts the
*correct* behavior, so a **failing test is a confirmed defect**.

```bash
cargo test --test qa_probes
```

---

## Round 1 — all 9 defects FIXED ✅

The first pass found 9 defects (2 High, 4 Medium, 2 Low). All are now fixed and the original
13 probes pass. Verified against the current tree:

| # | Defect | Fix landed | Probe (now passing) |
|---|--------|-----------|---------------------|
| HIGH-1 | One stray `.md` bricked every write | `read_entries` skips + warns on unparseable files | `one_bad_file_does_not_poison_the_bundle` |
| HIGH-2 | `namespace` bypassed traversal guard | `bundle_dir` drops `.`/`..`/empty components | `namespace_traversal_writes_outside_root` |
| MED-1 | Case-only-distinct ids collided on case-insensitive FS | `encode_id` percent-escapes uppercase → injective lowercase stems | `case_distinct_ids_do_not_collide` |
| MED-2 | Hand-added frontmatter silently dropped | `RecordMeta.extra` via `#[serde(flatten)]` | `unknown_frontmatter_field_is_preserved` |
| MED-3 | Newline in `description` spliced raw line into `index.md` | `one_line()` collapses control chars | `description_with_newline_keeps_index_wellformed` |
| MED-4 | `validate_id` denylist accepted `.` and newlines | Allowlist `[A-Za-z0-9._-]`, reject `.`/`..` | `dot_id_rejected`, `newline_id_rejected` |
| LOW-1 | Leading whitespace lost in body round-trip | `parse` slices body from raw bytes (no trim) | `body_leading_whitespace_round_trips` |
| LOW-2 | CRLF bodies normalized to LF | same byte-faithful parser | `body_crlf_round_trips` |

---

## Round 2 — expanded scenarios

Probed the **new surface the fix introduced** (`encode_id`, `extra`, namespace normalization,
the rewritten byte-faithful parser) plus areas not covered before (store-level round-trips,
concurrency, resource limits). **23 pass, 2 new defects.**

### NEW MED-5 — `extra` shadowing a known field produces unparseable output
- **Test:** `extra_shadowing_known_field_round_trips` (FAIL)
- **What:** `#[serde(flatten)] extra` is written *after* the named struct fields. If `extra`
  contains a key that collides with a known field (`id`, `type`, `description`, …), the YAML
  gets a **duplicate key**:
  ```yaml
  id: real-id
  ...
  id: ghost          # <- from extra
  ```
  `Record::parse` then fails with `duplicate field 'id'`. So `parse(to_markdown(r))` is **not
  total** — there exist constructible `Record`s whose own serialization can't be read back.
  The round-trip fix closed the *unknown-field* gap but opened a *shadowing* gap.
- **Severity:** Medium. Reachable from the public API (`RecordMeta.extra` is `pub`); a tool
  that copies frontmatter into `extra`, or a future `link()`/migration that manipulates it,
  can write a record that can never be read back.
- **Fix direction:** on parse, deny extra keys that match reserved names (serde already does at
  read time — the problem is the *write*); or strip reserved keys from `extra` in
  `to_markdown`; or validate `extra` in `put`.

### NEW MED-6 — Filename / frontmatter-id divergence makes a record visible but unreachable
- **Test:** `divergent_record_is_reachable_via_get` (FAIL)
- **What:** `list()` and `manifest()` derive the id from each record's **frontmatter**, while
  `get()`/`delete()` derive the **filename** from the id via `encode_id`. They agree only when
  the on-disk stem equals `encode_id(frontmatter_id)`. A file `memories/on-disk-name.md` whose
  frontmatter says `id: frontmatter-id` is:
  - **listed** as `frontmatter-id`,
  - but `get("frontmatter-id")` → `NotFound`, and `delete("frontmatter-id")` → `NotFound`.

  The record is **stuck**: shown in the index, un-fetchable, un-deletable. Triggers on any
  rename, hand-edit, or `aws s3 cp` to a different key — i.e. exactly the "ship a bundle
  around" workflow. The manifest `key` is correct (real filename), so a *key-addressed* fetch
  would work, but the `Store` trait only offers id-addressed `get`/`delete`.
- **Severity:** Medium. Self-inflicted only via external file moves today, but it's a latent
  inconsistency between the two id-resolution paths that the S3 backend will inherit.
- **Fix direction:** make `list()`/`manifest()` report the id that `get()` can actually
  resolve (derive id from filename, or verify `encode_id(frontmatter_id) == stem` and warn on
  mismatch), so the two paths can never disagree.

### New scenarios that PASSED (defenses confirmed)

- **Store-level losslessness:** `store_preserves_extra_through_put_get`,
  `body_round_trips_through_store` (CRLF + leading-whitespace + tabs survive `put`→`get`).
- **encode_id injectivity:** `percent_id_is_rejected` (the `%` escape char can't be a legal
  id), `case_distinct_ids_keep_distinct_content` (both `Alpha` and `alpha` retrievable with
  their own bodies).
- **Body parser edge cases:** `empty_body_round_trips`, `leading_blank_line_body_round_trips`,
  `trailing_newlines_body_round_trips`.
- **Namespace normalization:** `empty_namespace_stays_in_root` (empty / dot-only / `../..`
  namespaces collapse into the root, no escape, no panic).
- **Concurrency:** `concurrent_puts_are_all_visible` — 32 threads writing distinct ids; all
  records visible afterward (the API rebuilds from record files, not the cached manifest).
  *Note:* the persisted `manifest.json`/`index.md` can momentarily lag under concurrent
  writers since each `put` rewrites them whole and the last writer wins — harmless because
  they're derived and never read back by the code, but worth a line in the docs.
- **Resource limits:** `overlong_id_fails_gracefully` — a 5000-char id errors or is accepted
  by the FS, but never panics and never silently truncates into a collision.

---

---

## Round 3 — cross-tool portability, CRLF authoring, untested fields

Probed the portability of the emitted YAML/markdown to *other* tools (the core product
claim), CRLF-authored files, and the frontmatter fields not yet exercised (`source`, `links`,
every `MemoryType`) plus Unicode. **29 pass, 3 new defects** (1 of them reinforcing MED-6).

### NEW MED-7 — YAML 1.1 boolean coercion ("the Norway problem") breaks cross-tool portability
- **Tests:** `string_values_survive_yaml_1_1_readers` (FAIL), `ambiguous_tags_survive_yaml_1_1_readers` (FAIL)
- **What:** `serde_yaml` round-trips itself fine, but it emits the bare tokens
  `no` / `yes` / `on` / `off` / `y` / `n` **unquoted**, in both scalars and sequences:
  ```yaml
  description: no
  tags:
  - no
  - yes
  ```
  serde_yaml uses the YAML 1.2 core schema (those aren't booleans), so s3mem reads them back
  correctly — but **YAML 1.1 readers (PyYAML, Ruby, many Obsidian/MkDocs plugins, "any other
  agent") coerce them to booleans**. A description of `no` becomes `False`; a tag list `[no]`
  becomes `[False]`. This directly undercuts the README's headline claim that a bundle is
  "readable by humans, by Obsidian/MkDocs, and by any other agent."
- **Severity:** Medium — silent, content-dependent data corruption *at the portability boundary
  that is the product*. (Note `true`/`false`/`null`/`~`/numbers are already quoted correctly;
  only the 1.1-specific bool words leak.)
- **Fix direction:** force quoting of string scalars/seq items that match the YAML 1.1
  bool/null token set (or quote all string values) when rendering frontmatter.

### NEW LOW-3 — CRLF-authored document leaks stray carriage returns into the body
- **Test:** `crlf_document_body_has_no_stray_carriage_returns` (FAIL)
- **What:** the byte-faithful parser strips a single leading/trailing `\n` around the body but
  not the `\r` of a CRLF pair. A whole-file CRLF document (Windows editor, or git
  `core.autocrlf=true` on checkout) parses to `body = "\r\nthe body\r"` — a leading `\r\n` and
  trailing `\r` leak in. (Round 1's CRLF probe only covered CRLF *inside* an in-memory body;
  this is the on-disk-authored case.)
- **Severity:** Low — needs an externally CRLF-encoded file, but git autocrlf makes that a
  routine Windows-checkout scenario for a "git-native" format.
- **Fix direction:** treat the `\r\n` separator as a unit when slicing the body, or normalize
  line endings on read.

### MED-6 reinforced — a traversal-shaped id can reach the index but not `get()`
- **Test:** `listed_ids_are_all_resolvable` (FAIL)
- **What:** a file `memories/safe-name.md` whose frontmatter is `id: ../escape` is **listed**,
  written into `index.md` and `manifest.json` as `../escape`, yet `get("../escape")` is
  rejected by `validate_id` (`InvalidId`). Same root cause as MED-6 (read path trusts
  frontmatter, resolve path doesn't), with the extra wrinkle that a path-shaped string is
  surfaced into the derived artifacts unvalidated.

### Round-3 scenarios that PASSED (defenses confirmed)

- `source_and_links_round_trip_through_store` — `source`/`links` survive `put`→`get`.
- `every_memory_type_round_trips` + `unknown_memory_type_is_rejected` — all four `type:`
  variants round-trip; a bogus `type:` is rejected at parse.
- `unicode_body_and_description_round_trip` — emoji / CJK / RTL survive the body, and a
  Unicode description renders into `index.md`.

---

## Recommended priority order (remaining, all rounds)

1. **MED-6** (+ round-3 reinforcement) — unify the two id-resolution paths so `list()` /
   `manifest()` never report an id that `get()` can't resolve. Bites the S3 backend.
2. **MED-7** — quote YAML-1.1-ambiguous string values so the files survive non-Rust readers.
   This is a portability-claim defect, the core pitch.
3. **MED-5** — guard `extra` against reserved-key shadowing so `to_markdown` can't emit
   unparseable YAML.
4. **LOW-3** — handle CRLF separators in `parse` (or normalize on read).
5. Document the derived-file (`manifest.json`/`index.md`) staleness window under concurrent
   writers, and the "store never stamps `updated`" contract (pinned by
   `overwrite_does_not_touch_updated`).

---

---

## Round 4 — re-test after MED-5/6 fixes + QA of the new layers (recall, S3, CLI)

The repo advanced three commits (`Fix all 9 QA defects`, `Add S3 backend`, `Add recall layer
+ CLI`). Re-ran everything and QA'd the new surface.

### Previously-open format/store findings — status

| Finding | Status | Evidence |
|---|---|---|
| MED-5 (`extra` shadowing → unparseable) | **FIXED** | `extra_shadowing_known_field_round_trips` passes |
| MED-6 (divergent record unreachable) | **FIXED** for valid ids | `divergent_record_is_reachable_via_get` passes — `get`/`delete` now scan-fallback by frontmatter id in both backends |
| MED-6 residual | **OPEN** | `listed_ids_are_all_resolvable` FAILS — a record whose frontmatter id is *itself invalid* (`../escape`) is listed/indexed but `validate_id` rejects it before the fallback runs |
| MED-7 (YAML 1.1 coercion) | **OPEN** | `string_values…` + `ambiguous_tags…` FAIL |
| LOW-3 (CRLF document) | **OPEN** | `crlf_document_body_has_no_stray_carriage_returns` FAILS |

### NEW recall-layer findings (`tests/recall_probes.rs`)

**REC-1 (MED) — `grep ""` returns the entire bundle.** An empty pattern → `regex::escape("")`
→ empty regex → `is_match` true for every field, so every record is a hit. BM25 returns
nothing for an empty query, so the two recall tools disagree on "no query". Reachable from the
CLI: `s3mem grep "$QUERY"` with an unset/blank `$QUERY` dumps the whole memory. *(A
whitespace-only pattern is matched literally and is NOT affected — `grep_whitespace_pattern…`
passes, which pins the hazard to the truly-empty pattern.)*
- **Test:** `grep_empty_pattern_returns_nothing` (FAIL)
- **Fix direction:** treat an empty (post-trim) pattern as "no results" / an error, matching BM25.

**REC-2 (LOW) — `max_snippets = 0` silently drops matching records.** The cap is checked in
`consider` before any snippet is pushed, so a record that matches yields an empty snippet list
and is then filtered out by `if !snippets.is_empty()`. The volume cap doubles as an accidental
"hide all matches" switch (returns 0 hits for a real match).
- **Test:** `grep_zero_cap_still_reports_matching_records` (FAIL)
- **Fix direction:** always emit the hit; cap only the snippet vector (or guarantee ≥1 snippet).

**REC-3 (LOW/MED) — BM25 can't recall non-space-delimited scripts; grep can.** `tokenize`
splits on `!char::is_alphanumeric`, but that predicate is Unicode-aware, so a whole CJK run
(`日本語`) becomes a single token. `bm25("日本")` → 0 hits while `grep("日本")` → 1. The
*default, advertised* recall path silently misses substrings the literal path finds — a
correctness gap for any non-Latin-script memory.
- **Test:** `bm25_recalls_cjk_substring_like_grep_does` (FAIL)
- **Fix direction:** document the limitation, or add CJK n-gram / Unicode-segmentation
  tokenization.

### S3 backend — static review (no live AWS available)

Reviewed `backend/s3.rs` against the parity invariant. **Solid:** it reuses `backend/common.rs`
(`validate_id`/`encode_id`/`safe_segments`) so id rules, case-collision encoding, and namespace
containment match the local backend by construction; `read_entries` skips unparseable objects
(poison-resistant); `list_objects_v2` pagination is followed; `get`/`delete` carry the same
divergence scan-fallback as local. Its pure key-construction unit tests
(`namespace_traversal_is_contained`, `object_key_encodes_case_like_local`) pass and pin parity.
- **Gap (process, not a defect):** no offline backend test — the live test
  (`tests/s3_store.rs`) is skipped unless `S3MEM_TEST_BUCKET` is set, so CI never exercises the
  S3 code path. **Recommend a LocalStack/MinIO test** via `S3Store::from_config` (the code
  already supports a custom endpoint) so the backend is covered without real AWS. All the
  format/store findings here (MED-7, LOW-3, MED-6 residual) apply equally to S3, since it shares
  the format layer.

### Build / lint health

`cargo check --features cli` and `--features s3` both compile clean; `cargo clippy
--all-targets --features cli,s3` is clean. The crate's own suites (lib 17, `local_store` 4, s3
key tests 2, doctests) are green.

---

## Probe suite status

- `tests/qa_probes.rs` — **33 probes, 29 pass, 4 fail** (format/store). Reds:
  `string_values_survive_yaml_1_1_readers`, `ambiguous_tags_survive_yaml_1_1_readers` (MED-7),
  `crlf_document_body_has_no_stray_carriage_returns` (LOW-3), `listed_ids_are_all_resolvable`
  (MED-6 residual).
- `tests/recall_probes.rs` — **9 probes, 6 pass, 3 fail** (recall). Reds:
  `grep_empty_pattern_returns_nothing` (REC-1), `grep_zero_cap_still_reports_matching_records`
  (REC-2), `bm25_recalls_cjk_substring_like_grep_does` (REC-3).

All 7 reds are open findings and double as living repros. The crate's own test suites and
clippy stay green across all features.

## Open findings, priority order

1. **REC-1** — empty grep dumps the whole bundle (agent-facing CLI footgun).
2. **MED-7** — YAML 1.1 bool coercion breaks cross-tool portability (the core pitch).
3. **MED-6 residual** — invalid frontmatter ids are listed but unreachable.
4. **REC-3** — BM25 misses non-Latin scripts the grep path finds.
5. **REC-2** — `max_snippets=0` hides matches; **LOW-3** — CRLF-authored files leak `\r`.
6. **Process** — add an offline (LocalStack/MinIO) S3 backend test so the S3 path is covered in CI.

---

## Round 5 — resolution (alongside the cached recall index)

All seven open correctness findings are **FIXED**; their probes now pass (`qa_probes` 33/33,
`recall_probes` 9/9). Changes:

| Finding | Fix |
|---|---|
| **REC-1** | `grep` returns nothing for an empty/whitespace-only pattern (matches BM25's empty-query behavior). |
| **MED-7** | `to_markdown` post-processes serde_yaml output (`harden_yaml_1_1`), single-quoting any bare scalar a YAML-1.1 reader would coerce (bool/null/number tokens) — in mapping values *and* block-sequence items. Over-quoting only; values unchanged. |
| **MED-6 residual** | `read_entries` (both backends) now skips records whose frontmatter id fails `validate_id`, so `list`/`manifest`/`index.md`/recall never surface an id `get`/`delete` can't resolve. |
| **REC-3** | `tokenize` is CJK-aware: a CJK run emits per-char unigrams + overlapping bigrams, so `bm25("日本")` recalls `日本語` like grep does. |
| **REC-2** | `grep` emits a hit whenever a record matches; `max_snippets` caps only the snippet vector (`0` → hit with no snippets, never a hidden match). |
| **LOW-3** | `parse` strips a `\r\n` *or* `\n` separator/trailer, so a fully-CRLF document yields a clean body while CRLFs we wrote *inside* a body are still preserved. |

**Process item (offline S3 test):** still open — the S3 cache path was instead verified live
against a real bucket (recall persists `recall-index.json`; an added memory changes the
ETag-based fingerprint and recall self-heals). A LocalStack/MinIO test via `S3Store::from_config`
remains the CI-friendly follow-up.
