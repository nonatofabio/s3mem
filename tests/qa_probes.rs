//! QA corner-case probes. Each test states an ASSUMPTION the design appears to make
//! and tries to disprove it. Where a test asserts buggy behavior, the comment says so;
//! where it asserts the *correct* behavior and fails, the bug is real.

use s3mem::{LocalStore, MemoryType, Record, RecordMeta, Store};

fn temp_bundle(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("s3mem-qa-{}-{}", std::process::id(), name));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

fn meta(id: &str) -> RecordMeta {
    RecordMeta::new(id, MemoryType::Semantic, "desc", "2026-06-19T00:00:00Z")
}

// ---------------------------------------------------------------------------
// FORMAT LAYER: round-trip fidelity
// ---------------------------------------------------------------------------

/// ASSUMPTION: parse(to_markdown(r)) == r  (the round-trip invariant the README leans on).
/// Probe: a body with significant LEADING whitespace.
#[test]
fn body_leading_whitespace_round_trips() {
    let r = Record::new(meta("x"), "    indented code block\nline2");
    let md = r.to_markdown().unwrap();
    let back = Record::parse(&md).unwrap();
    assert_eq!(r.body, back.body, "leading whitespace lost on round-trip");
}

/// Probe: body that is a markdown horizontal rule / contains `---`.
#[test]
fn body_with_triple_dash_round_trips() {
    let r = Record::new(meta("x"), "above\n---\nbelow");
    let md = r.to_markdown().unwrap();
    let back = Record::parse(&md).unwrap();
    assert_eq!(r.body, back.body);
}

/// Probe: CRLF line endings in the body (Windows authoring; "ship around" claim).
#[test]
fn body_crlf_round_trips() {
    let r = Record::new(meta("x"), "line1\r\nline2");
    let md = r.to_markdown().unwrap();
    let back = Record::parse(&md).unwrap();
    assert_eq!(r.body, back.body, "CRLF not preserved");
}

/// ASSUMPTION: frontmatter is the structured source of truth and survives editing.
/// Probe: a hand-added extra frontmatter field. OKF is "git-native, hand-editable".
#[test]
fn unknown_frontmatter_field_is_preserved() {
    let md = "---\nid: x\ntype: semantic\ndescription: d\ncreated: 2026-01-01T00:00:00Z\nupdated: 2026-01-01T00:00:00Z\nimportance: high\n---\n\nbody\n";
    let rec = Record::parse(md).unwrap();
    let back = rec.to_markdown().unwrap();
    assert!(
        back.contains("importance: high"),
        "user-added frontmatter field silently dropped on rewrite"
    );
}

/// Probe: a body line that is `---` surrounded by spaces is treated as frontmatter close.
#[test]
fn padded_dashes_in_frontmatter_region() {
    // Close delimiter is `line.trim() == "---"`, so a yaml value cannot contain a `---`
    // line, and an opening `   ---` with spaces is accepted. Demonstrate the loose close.
    let md = "---\nid: x\ntype: semantic\ndescription: d\ncreated: 2026-01-01T00:00:00Z\nupdated: 2026-01-01T00:00:00Z\n  ---  \n\nbody\n";
    let rec = Record::parse(md);
    assert!(rec.is_ok(), "padded `---` should still close frontmatter");
}

// ---------------------------------------------------------------------------
// STORE LAYER: id handling & traversal
// ---------------------------------------------------------------------------

/// ASSUMPTION: validate_id makes ids traversal-safe and a clean filename stem.
/// Probe: id == "." — not empty, contains no "..", no slash. Accepted?
#[test]
fn dot_id_rejected() {
    let root = temp_bundle("dot");
    let store = LocalStore::new(&root, "ns");
    let r = Record::new(meta("."), "b");
    assert!(store.put(&r).is_err(), "id `.` should be rejected");
    std::fs::remove_dir_all(&root).ok();
}

/// Probe: id containing a newline / control chars passes validate_id.
#[test]
fn newline_id_rejected() {
    let root = temp_bundle("newline");
    let store = LocalStore::new(&root, "ns");
    let r = Record::new(meta("a\nb"), "b");
    assert!(store.put(&r).is_err(), "id with newline should be rejected");
    std::fs::remove_dir_all(&root).ok();
}

/// Probe: id with an embedded ".md" or other extension confuses list()/get() identity.
#[test]
fn id_with_md_extension_keeps_identity() {
    let root = temp_bundle("ext");
    let store = LocalStore::new(&root, "ns");
    let r = Record::new(meta("notes.md"), "b");
    store.put(&r).unwrap();
    let ids = store.list().unwrap();
    assert_eq!(ids, vec!["notes.md"], "list() id mismatch for dotted id");
    // and the manifest key must point at the file we actually wrote
    let m = store.manifest().unwrap();
    let key = &m.entries[0].key;
    assert!(
        store.bundle_dir().join(key).exists(),
        "manifest key {key} does not resolve to a real file"
    );
    std::fs::remove_dir_all(&root).ok();
}

/// ASSUMPTION: a bundle round-trips between filesystems unchanged ("ship around").
/// Probe: case-only-distinct ids — collide on a case-insensitive FS (macOS default).
#[test]
fn case_distinct_ids_do_not_collide() {
    let root = temp_bundle("case");
    let store = LocalStore::new(&root, "ns");
    store.put(&Record::new(meta("Alpha"), "upper")).unwrap();
    store.put(&Record::new(meta("alpha"), "lower")).unwrap();
    let ids = store.list().unwrap();
    assert_eq!(ids.len(), 2, "case-only-distinct ids collapsed to one file");
    std::fs::remove_dir_all(&root).ok();
}

/// ASSUMPTION: traversal protection guards the bundle. The namespace is NOT validated
/// at all — `../` escapes the intended root. Check the FILESYSTEM, not a lexical prefix.
#[test]
fn namespace_traversal_writes_outside_root() {
    let root = temp_bundle("ns-escape");
    std::fs::create_dir_all(&root).unwrap();
    let store = LocalStore::new(&root, "../ESCAPED-QA");
    store.put(&Record::new(meta("x"), "b")).unwrap();
    let escaped = root.parent().unwrap().join("ESCAPED-QA");
    let leaked = escaped.join("memories").join("x.md").exists();
    let _ = std::fs::remove_dir_all(&escaped);
    std::fs::remove_dir_all(&root).ok();
    assert!(
        !leaked,
        "record written OUTSIDE the bundle root via namespace `../`"
    );
}

// ---------------------------------------------------------------------------
// INDEX LAYER: robustness of the derived manifest
// ---------------------------------------------------------------------------

/// ASSUMPTION: the manifest "is always reconstructable by listing + reading records".
/// Probe: one corrupt/foreign .md file in memories/. Does it poison the WHOLE bundle?
#[test]
fn one_bad_file_does_not_poison_the_bundle() {
    let root = temp_bundle("poison");
    let store = LocalStore::new(&root, "ns");
    store.put(&Record::new(meta("good"), "b")).unwrap();

    // A human drops a stray markdown note into the bundle (OKF is "just files").
    let stray = store.bundle_dir().join("memories").join("README.md");
    std::fs::write(&stray, "# just a note, no frontmatter\n").unwrap();

    // Can we still write / index / recall the good memory?
    let put = store.put(&Record::new(meta("good2"), "b2"));
    let manifest = store.manifest();
    std::fs::remove_dir_all(&root).ok();
    assert!(
        put.is_ok() && manifest.is_ok(),
        "a single non-record .md file breaks all writes and manifest generation"
    );
}

/// ASSUMPTION: index.md is well-formed markdown. Probe: a description containing a
/// newline — not enforced to be one line — splices a raw line into the index.
#[test]
fn description_with_newline_keeps_index_wellformed() {
    let root = temp_bundle("desc");
    let store = LocalStore::new(&root, "ns");
    let m = RecordMeta::new(
        "x",
        MemoryType::Semantic,
        "real desc\nINJECTED LINE",
        "2026-01-01T00:00:00Z",
    );
    store.put(&Record::new(m, "b")).unwrap();
    let index = std::fs::read_to_string(store.bundle_dir().join("index.md")).unwrap();
    // The injected text must not appear on its own physical line outside a list item.
    let corrupted = index
        .lines()
        .any(|l| l.contains("INJECTED LINE") && !l.trim_start().starts_with("- ["));
    std::fs::remove_dir_all(&root).ok();
    assert!(
        !corrupted,
        "newline in description spliced a raw line into index.md"
    );
}

/// Probe: put does not refresh `updated`. Overwriting an existing id keeps stale stamp
/// unless the caller remembers to bump it. Is that the intended contract? Document it.
#[test]
fn overwrite_does_not_touch_updated() {
    let root = temp_bundle("updated");
    let store = LocalStore::new(&root, "ns");
    let mut m = meta("x");
    m.updated = "2020-01-01T00:00:00Z".into();
    store.put(&Record::new(m, "v1")).unwrap();
    store
        .put(&Record::new(
            {
                let mut m2 = meta("x");
                m2.updated = "2020-01-01T00:00:00Z".into();
                m2
            },
            "v2",
        ))
        .unwrap();
    let got = store.get("x").unwrap();
    std::fs::remove_dir_all(&root).ok();
    // This passes — it just pins the behavior: the store never auto-stamps `updated`.
    assert_eq!(got.meta.updated, "2020-01-01T00:00:00Z");
}

// ===========================================================================
// EXPANDED SCENARIOS (round 2) — probing the surface the fix introduced:
// encode_id, RecordMeta.extra, namespace normalization, byte-faithful parse,
// plus areas not covered before (store-level round-trips, divergence,
// concurrency, resource limits).
// ===========================================================================

// --- New seam: extra (#[serde(flatten)]) collisions ------------------------

/// ASSUMPTION: `parse(to_markdown(r))` is total — any constructible Record round-trips
/// (the "lossless / byte-faithful" promise). Probe: an `extra` key that SHADOWS a known
/// field. serde flatten emits a duplicate YAML key, and the output is then unparseable.
#[test]
fn extra_shadowing_known_field_round_trips() {
    let mut m = meta("real-id");
    m.extra
        .insert("id".into(), serde_yaml::Value::String("ghost".into()));
    let r = Record::new(m, "b");
    let md = r.to_markdown().unwrap();
    let back = Record::parse(&md);
    assert!(
        back.is_ok(),
        "to_markdown emitted a duplicate `id:` key that parse rejects: {back:?}"
    );
}

/// Store-level lossless check for NON-shadowing extra: a hand-added field survives a
/// full put -> get cycle (not just the in-memory format round-trip).
#[test]
fn store_preserves_extra_through_put_get() {
    let root = temp_bundle("extra-store");
    let store = LocalStore::new(&root, "ns");
    let mut m = meta("x");
    m.extra.insert(
        "importance".into(),
        serde_yaml::Value::String("high".into()),
    );
    store.put(&Record::new(m, "b")).unwrap();
    let got = store.get("x").unwrap();
    std::fs::remove_dir_all(&root).ok();
    assert_eq!(
        got.meta.extra.get("importance"),
        Some(&serde_yaml::Value::String("high".into())),
        "extra frontmatter lost through the store"
    );
}

// --- New seam: filename (encode_id) vs frontmatter id ----------------------

/// ASSUMPTION: every record that `list()`/`manifest()` reports can be `get()` and
/// `delete()`. Probe: a file whose frontmatter id differs from its on-disk stem (a
/// rename, hand-edit, or any backend that keys files differently). list() trusts the
/// frontmatter; get()/delete() recompute the filename via encode_id — they disagree, so
/// the record is visible but unreachable.
#[test]
fn divergent_record_is_reachable_via_get() {
    let root = temp_bundle("divergence");
    let store = LocalStore::new(&root, "ns");
    let f = store.bundle_dir().join("memories");
    std::fs::create_dir_all(&f).unwrap();
    let body = "---\nid: frontmatter-id\ntype: semantic\ndescription: d\ncreated: 2026-01-01T00:00:00Z\nupdated: 2026-01-01T00:00:00Z\n---\n\nb\n";
    std::fs::write(f.join("on-disk-name.md"), body).unwrap();

    let listed = store.list().unwrap();
    let gettable = store.get("frontmatter-id").is_ok();
    std::fs::remove_dir_all(&root).ok();
    assert!(
        listed.contains(&"frontmatter-id".to_string()) && gettable,
        "record is listed as `frontmatter-id` but get() can't reach it (listed={listed:?})"
    );
}

// --- encode_id injectivity / robustness ------------------------------------

/// The `%` escape char must not be a legal id, or encode_id stops being injective
/// (encode("Alpha") == "%41lpha" could collide with a literal id "%41lpha").
#[test]
fn percent_id_is_rejected() {
    let root = temp_bundle("percent");
    let store = LocalStore::new(&root, "ns");
    let err = store.put(&Record::new(meta("%41lpha"), "b")).is_err();
    std::fs::remove_dir_all(&root).ok();
    assert!(
        err,
        "id containing `%` must be rejected to keep encode_id injective"
    );
}

/// Strengthen the case-collision probe: not just two files, but both CONTENTS retrievable.
#[test]
fn case_distinct_ids_keep_distinct_content() {
    let root = temp_bundle("case-content");
    let store = LocalStore::new(&root, "ns");
    store
        .put(&Record::new(meta("Alpha"), "UPPER body"))
        .unwrap();
    store
        .put(&Record::new(meta("alpha"), "lower body"))
        .unwrap();
    let upper = store.get("Alpha").unwrap().body;
    let lower = store.get("alpha").unwrap().body;
    std::fs::remove_dir_all(&root).ok();
    assert_eq!(upper, "UPPER body");
    assert_eq!(lower, "lower body");
}

// --- Body byte-fidelity edge cases (the new split_inclusive parser) --------

fn body_round_trips(body: &str) -> bool {
    let r = Record::new(meta("x"), body);
    Record::parse(&r.to_markdown().unwrap()).unwrap().body == body
}

#[test]
fn empty_body_round_trips() {
    assert!(body_round_trips(""));
}

#[test]
fn leading_blank_line_body_round_trips() {
    assert!(body_round_trips("\nstarts after a blank line"));
}

#[test]
fn trailing_newlines_body_round_trips() {
    // A single trailing \n is the format separator; extra ones are content.
    assert!(body_round_trips("text\n\n"));
}

#[test]
fn body_round_trips_through_store() {
    let root = temp_bundle("body-store");
    let store = LocalStore::new(&root, "ns");
    let body = "  indented\r\nCRLF line\n\ttab\n";
    store.put(&Record::new(meta("x"), body)).unwrap();
    let got = store.get("x").unwrap().body;
    std::fs::remove_dir_all(&root).ok();
    assert_eq!(got, body, "store round-trip mangled an exotic body");
}

// --- Namespace normalization behavior --------------------------------------

/// Empty / dot-only namespaces collapse into the root. Pin the behavior and confirm it
/// stays contained (no escape, no panic).
#[test]
fn empty_namespace_stays_in_root() {
    let root = temp_bundle("ns-empty");
    let store = LocalStore::new(&root, "");
    assert_eq!(store.bundle_dir(), root);
    let nested = LocalStore::new(&root, "../../a/b");
    assert!(
        nested.bundle_dir().starts_with(&root),
        "namespace escaped root"
    );
    std::fs::remove_dir_all(&root).ok();
}

// --- Concurrency: many writers, no lost or corrupt records -----------------

/// ASSUMPTION: concurrent writers don't lose records. Each put writes its own file then
/// rebuilds the index; the API rebuilds from records (not the cached manifest.json), so
/// every committed record must be visible afterward.
#[test]
fn concurrent_puts_are_all_visible() {
    let root = temp_bundle("concurrency");
    let store = LocalStore::new(&root, "ns");
    std::thread::scope(|s| {
        for i in 0..32 {
            let store = &store;
            s.spawn(move || {
                let id = format!("id-{i:03}");
                store
                    .put(&Record::new(meta(&id), format!("body {i}")))
                    .unwrap();
            });
        }
    });
    let ids = store.list().unwrap();
    std::fs::remove_dir_all(&root).ok();
    assert_eq!(ids.len(), 32, "concurrent puts lost records: {ids:?}");
}

// --- Resource limits: pathological id ---------------------------------------

/// An over-long id must fail gracefully (an Err), never panic, and never silently
/// truncate to collide with a different id.
#[test]
fn overlong_id_fails_gracefully() {
    let root = temp_bundle("longid");
    let store = LocalStore::new(&root, "ns");
    let id = "a".repeat(5000);
    let res = std::panic::catch_unwind(|| store.put(&Record::new(meta(&id), "b")));
    std::fs::remove_dir_all(&root).ok();
    match res {
        Ok(Ok(())) => { /* FS accepted it — acceptable */ }
        Ok(Err(_)) => { /* graceful error — acceptable */ }
        Err(_) => panic!("over-long id PANICKED instead of erroring"),
    }
}
