//! The status the front page claims, asserted against the code that must
//! back it.
//!
//! Three external reviews in a row read a README that said concurrency did
//! not exist while the differential harness two files over was proving that
//! it did. The CI workflow already knew the failure mode — "documentation
//! rots" — and guarded the example *outputs*, but nothing guarded the
//! *claims*. This suite is that guard: each assertion ties a sentence in
//! README.md to a probe of the thing the sentence is about, so a capability
//! cannot ship or disappear without the front page moving in the same
//! commit.
//!
//! When a status claim changes, extend this file in the same change. An
//! assertion that greps for prose is brittle on purpose: the brittleness is
//! the reminder.

use std::path::PathBuf;
use std::process::{Command, Stdio};

#[path = "support/task_corpus.rs"]
mod task_corpus;

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("..")
}

/// README text reduced to something prose reflows cannot break: lowercase,
/// markdown markers stripped, all whitespace runs collapsed to one space.
fn normalized_readme() -> String {
    let raw = std::fs::read_to_string(workspace_root().join("README.md"))
        .expect("README.md at the workspace root");
    let mut text = String::with_capacity(raw.len());
    let mut last_was_space = false;
    for ch in raw.chars() {
        let ch = match ch {
            '*' | '`' | '>' => continue,
            c if c.is_whitespace() => ' ',
            c => c.to_ascii_lowercase(),
        };
        if ch == ' ' {
            if last_was_space {
                continue;
            }
            last_was_space = true;
        } else {
            last_was_space = false;
        }
        text.push(ch);
    }
    text
}

/// The claim "structured tasks run in real parallel" is only allowed on the
/// front page because this probe holds: a program that spawns children runs
/// green under the *default* executor — the parallel one.
#[test]
fn parallel_claim_is_backed_by_the_default_executor() {
    assert!(
        task_corpus::PROGRAMS
            .iter()
            .any(|p| p.source.contains("spawn")),
        "the task corpus no longer exercises spawn — the probe below proves nothing"
    );

    let dir = std::env::temp_dir()
        .join("xenith-readme-truth")
        .join(std::process::id().to_string());
    std::fs::create_dir_all(&dir).expect("scratch directory");
    std::fs::write(dir.join("main.xn"), task_corpus::source("fan_out"))
        .expect("write probe program");

    let status = Command::new(env!("CARGO_BIN_EXE_xenith"))
        .current_dir(&dir)
        .args(["run", "main.xn"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .expect("the compiler binary runs");
    assert!(
        status.success(),
        "fan_out no longer runs green under the default executor; \
         if parallel execution was removed, the README claim must go in the same commit"
    );

    assert!(
        normalized_readme().contains("structured tasks run in real parallel"),
        "tasks run in parallel (probe above) but the README status block no longer says so"
    );
}

/// The exact sentence three external reviews caught, pinned so it cannot
/// come back: the status block may not deny concurrency while the corpus
/// above executes it.
#[test]
fn status_block_does_not_deny_shipped_concurrency() {
    let readme = normalized_readme();
    assert!(
        !readme.contains("what does not: concurrency"),
        "the pre-0015 status sentence is back in README.md"
    );
    let denial = readme
        .find("what does not exist:")
        .map(|start| &readme[start..(start + 200).min(readme.len())]);
    if let Some(denial) = denial {
        assert!(
            !denial.contains("concurrency"),
            "the does-not-exist list denies concurrency, which ships and is tested"
        );
    }
}

/// The WebAssembly claim and the crate that makes it true move together.
#[test]
fn wasm_claim_matches_the_wasm_crate() {
    let crate_exists = workspace_root()
        .join("compiler")
        .join("tools")
        .join("xenith-wasm")
        .join("Cargo.toml")
        .is_file();
    let readme_claims = normalized_readme().contains("webassembly");
    assert_eq!(
        crate_exists, readme_claims,
        "xenith-wasm crate present: {crate_exists}, README mentions WebAssembly: \
         {readme_claims} — these must change in the same commit"
    );
}
