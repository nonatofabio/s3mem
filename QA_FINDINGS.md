# S3Mem — QA corner-case findings

Adversarial QA pass against the early scaffold (format layer + `LocalStore`). Goal: disprove
the assumptions the README/architecture make about the format, the store, and the "ship a
bundle around unchanged" portability claim.

**Method.** Baseline suite (9 tests) was green. I added 13 corner-case probes in
[`tests/qa_probes.rs`](tests/qa_probes.rs). Each probe asserts the *correct* behavior, so a
**failing test is a confirmed defect**. Result: **9 failed, 4 passed.** No production code was
changed — this is a QA pass, not a fix.

Reproduce:

```bash
cargo test --test qa_probes        # 9 of these FAIL on purpose — each failure is a defect
```

---

## Confirmed defects

### HIGH-1 — A single stray file bricks every write
- **Test:** `one_bad_file_does_not_poison_the_bundle` (FAIL)
- **Where:** `backend/local.rs::manifest()` → called by `reindex()` on every `put`/`delete`.
- **What:** `manifest()` does `list()` then `get()` on *every* `.md` file and propagates the
  first parse error. Drop one non-OKF markdown file into `memories/` (e.g. a human `README.md`
  — exactly the "it's just files" workflow the README sells) and **every subsequent `put`,
  `delete`, and `manifest()` fails**.
- **Contradicts:** architecture claim that the manifest "is always reconstructable by listing +
  reading the `memories/` objects."
- **Fix direction:** skip + warn on unparseable files instead of failing the whole bundle.

### HIGH-2 — Namespace bypasses traversal protection
- **Test:** `namespace_traversal_writes_outside_root` (FAIL)
- **Where:** `LocalStore::new` / `bundle_dir()` — `root.join(namespace)` with no validation.
- **What:** `validate_id` carefully blocks `..` / `/` in record ids, but `namespace` is
  completely unguarded. `LocalStore::new(root, "../ESCAPED")` writes records **outside** the
  bundle root. The traversal defense is asymmetric: id guarded, namespace wide open.
- **Fix direction:** validate `namespace` with the same (or stricter) rules as `id`.

---

## Medium

### MED-1 — "Ship around" portability invariant breaks on case-insensitive filesystems
- **Test:** `case_distinct_ids_do_not_collide` (FAIL)
- **What:** ids `Alpha` and `alpha` are distinct format keys, but on macOS APFS / Windows NTFS
  they map to one file — the second `put` **silently overwrites** the first. A bundle authored
  on case-sensitive Linux **loses records** when synced to a Mac.
- **Contradicts:** "a bundle round-trips between cloud and disk unchanged." S3 *is*
  case-sensitive, so the same bundle will behave differently across backends. (Unicode NFC/NFD
  normalization on macOS is the same class of bug — untested here, worth adding.)
- **Fix direction:** decide a case/normalization policy at the format layer before S3 lands.

### MED-2 — Hand-editing silently loses data
- **Test:** `unknown_frontmatter_field_is_preserved` (FAIL)
- **What:** `RecordMeta` has no `deny_unknown_fields` and no catch-all. Any frontmatter field a
  human adds (`importance:`, `author:`, …) is **silently dropped** the next time an agent
  `put`s that record.
- **Contradicts:** README pitch of OKF as "git-native, hand-editable." The format isn't
  lossless against hand edits.
- **Fix direction:** `#[serde(flatten)] extra: BTreeMap<String, Value>` to preserve unknowns.

### MED-3 — `index.md` is corruptible via `description`
- **Test:** `description_with_newline_keeps_index_wellformed` (FAIL)
- **What:** `description` isn't constrained to one line. A newline (or `)` / `]`) round-trips
  fine as YAML data but `to_index_md` splices it raw into the link row:
  ```
  - [`x`](memories/x.md) — real desc
  INJECTED LINE _(semantic)_
  ```
- **Fix direction:** sanitize/escape `description` (and `id`) when rendering `index.md`.

### MED-4 — Id validation gaps (denylist instead of allowlist)
- **Tests:** `dot_id_rejected` (FAIL), `newline_id_rejected` (FAIL)
- **What:** `validate_id` is a denylist (`/`, `\`, `..`, separator). It accepts `id = "."`
  (→ file literally named `..md`) and `id = "a\nb"` (control chars / newline → garbage filename
  and a corrupt-looking manifest key).
- **Fix direction:** switch to an allowlist matching the "stable-kebab-slug" contract.

---

## Low — body round-trip is not byte-faithful

- **`body_leading_whitespace_round_trips` (FAIL):** `to_markdown` does `trim_end()` but `parse`
  does `trim()`, so **leading** whitespace (indented code blocks) is lost on round-trip.
- **`body_crlf_round_trips` (FAIL):** `.lines()` strips `\r`, so CRLF bodies (Windows authoring)
  silently normalize to LF. Bodies aren't preserved verbatim.

---

## Probes that confirmed behavior is sound (4 passed)

- `body_with_triple_dash_round_trips` — a `---` horizontal rule inside the body survives.
- `padded_dashes_in_frontmatter_region` — whitespace-padded `---` still closes frontmatter
  (intentional, loose close).
- `id_with_md_extension_keeps_identity` — dotted ids (`notes.md`) keep list/get/key identity.
- `overwrite_does_not_touch_updated` — the store never auto-stamps `updated`. Passes, but this
  is an undocumented contract: callers must bump `updated` themselves. Worth documenting.

---

## Recommended priority order

1. **HIGH-1** — make `manifest()` resilient (skip + warn). Breaks the central invariant.
2. **HIGH-2** — validate `namespace` (traversal / data integrity).
3. **MED-1** — decide case/Unicode collision policy *before* S3 lands (cross-backend divergence).
4. **MED-2/3/4** — allowlist `validate_id`; sanitize `description` in `to_index_md`; flatten +
   preserve unknown frontmatter.
5. **LOW** — document or fix body whitespace/CRLF fidelity (or explicitly declare bodies are
   normalized).
