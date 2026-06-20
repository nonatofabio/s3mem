//! End-to-end "agent wiring" test: drives the `s3mem` CLI the way an agent following
//! `skills/s3mem-memory/SKILL.md` would. Each command is a **separate process**, so nothing
//! carries between them except the on-disk bundle — i.e. this is the real cross-session
//! question: does memory written in one agent session survive into a later one?
//!
//! Run: `cargo test --features cli --test agent_workflow`
#![cfg(feature = "cli")]

use std::path::{Path, PathBuf};
use std::process::Command;

fn bundle(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("s3mem-agent-{}-{}", std::process::id(), name));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

/// Run the CLI as a fresh process against `path`; assert success and return stdout.
fn run(path: &Path, args: &[&str]) -> String {
    let out = Command::new(env!("CARGO_BIN_EXE_s3mem"))
        .env("S3MEM_PATH", path)
        .env("S3MEM_NAMESPACE", "agent")
        .args(args)
        .output()
        .expect("spawn s3mem");
    assert!(
        out.status.success(),
        "`s3mem {args:?}` failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// The first result line (`● <id> …`) of a `--pretty` recall/grep/neighbors listing.
fn top(listing: &str) -> &str {
    listing.lines().next().unwrap_or_default()
}

#[test]
fn agent_learns_in_one_session_and_uses_memory_in_another() {
    let path = bundle("crosssession");

    // ===== Session 1: the agent learns facts and links related ones =====
    run(
        &path,
        &[
            "remember",
            "--id",
            "user-deploy-pref",
            "--type",
            "semantic",
            "--description",
            "User deploys via terraform from CI, never the console",
            "--tag",
            "ops",
            "--body",
            "The user runs `terraform apply` from CI on merge to main.",
        ],
    );
    run(
        &path,
        &[
            "remember",
            "--id",
            "ci-pipeline",
            "--type",
            "procedural",
            "--description",
            "CI plans on PR, applies on merge",
            "--tag",
            "ops",
            "--body",
            "CI runs terraform plan on every PR and apply on merge to main.",
        ],
    );
    run(
        &path,
        &[
            "remember",
            "--id",
            "pref-rust",
            "--type",
            "semantic",
            "--description",
            "User prefers Rust for backend services",
            "--tag",
            "lang",
            "--body",
            "Backend services should be written in Rust.",
        ],
    );
    run(&path, &["link", "user-deploy-pref", "ci-pipeline"]);

    // ===== Session 2: a brand-new process — only the bundle persists =====

    // Ranked recall of a *paraphrased* question surfaces the deploy knowledge (not pref-rust).
    let recalled = run(
        &path,
        &[
            "recall",
            "how does the user ship code to production",
            "--pretty",
        ],
    );
    assert!(
        top(&recalled).contains("user-deploy-pref") || top(&recalled).contains("ci-pipeline"),
        "recall's top hit should be a deploy memory, got:\n{recalled}"
    );
    assert!(
        !top(&recalled).contains("pref-rust"),
        "off-topic memory ranked first:\n{recalled}"
    );

    // Exact-token grep finds both terraform memories.
    let grepped = run(&path, &["grep", "terraform", "--pretty"]);
    assert!(
        grepped.contains("user-deploy-pref") && grepped.contains("ci-pipeline"),
        "{grepped}"
    );

    // The link the agent made in session 1 is still traversable.
    let neighbors = run(
        &path,
        &["neighbors", "user-deploy-pref", "--depth", "1", "--pretty"],
    );
    assert!(
        neighbors.contains("ci-pipeline"),
        "link did not persist across sessions:\n{neighbors}"
    );

    // And it can pull the full memory to actually answer.
    let full = run(&path, &["get", "user-deploy-pref"]);
    assert!(
        full.contains("terraform apply"),
        "get returned the wrong body:\n{full}"
    );

    std::fs::remove_dir_all(&path).ok();
}

#[test]
fn agent_abstains_when_memory_is_empty() {
    // Before anything is remembered, recall must return nothing — the basis for an agent
    // correctly saying "I don't have that in memory" rather than confabulating.
    let path = bundle("abstain");
    let recalled = run(
        &path,
        &["recall", "what is the user's favorite color", "--pretty"],
    );
    assert!(
        recalled.contains("(no matches)"),
        "empty bundle should recall nothing:\n{recalled}"
    );
    std::fs::remove_dir_all(&path).ok();
}
