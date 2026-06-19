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

## Recommended priority order (remaining)

1. **MED-6** — unify the two id-resolution paths so `list()` never reports an unreachable id.
   This is the one that will bite the S3 backend.
2. **MED-5** — guard `extra` against reserved-key shadowing so `to_markdown` can't emit
   unparseable YAML.
3. Document the derived-file (`manifest.json`/`index.md`) staleness window under concurrent
   writers, and the "store never stamps `updated`" contract (pinned by
   `overwrite_does_not_touch_updated`).
