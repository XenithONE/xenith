//! Every ```xenith block in the repository's Markdown must compile.
//!
//! For a language whose entire premise is that a model learns it from the
//! documentation, an example that does not parse is not a typo — it teaches
//! the wrong syntax to every reader, human or otherwise. The README's central
//! example was wrong for a day before this test existed, which is exactly why
//! it now does.
//!
//! Blocks are treated as whole modules. Two fence tags adjust that:
//!
//! - `xenith,in-fn` — a fragment; wrapped in a function body before checking.
//! - `xenith,planned` — syntax the compiler does not implement yet. Skipped,
//!   and **permitted only under `design/`**. A design note may describe a
//!   language that does not exist; the README, the specification and the
//!   examples may not.

use std::path::{Path, PathBuf};

use xenith_syntax::parse;

fn repo_root() -> PathBuf {
    // crates/xenith-syntax -> crates -> compiler -> repository root
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(3)
        .expect("repository root")
        .to_path_buf()
}

fn markdown_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            // `target` holds vendored crate documentation, which is not ours.
            if path.file_name().is_some_and(|n| n == "target") {
                continue;
            }
            markdown_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "md") {
            out.push(path);
        }
    }
}

struct Block {
    file: PathBuf,
    /// One-based line of the opening fence, so a failure is clickable.
    line: usize,
    source: String,
    /// Tagged as not-yet-implemented syntax.
    planned: bool,
}

/// Pull fenced blocks tagged `xenith` out of Markdown.
fn xenith_blocks(path: &Path) -> Vec<Block> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };

    let mut blocks = Vec::new();
    let mut lines = text.lines().enumerate();

    while let Some((index, line)) = lines.next() {
        let trimmed = line.trim_start();
        let Some(info) = trimmed.strip_prefix("```") else {
            continue;
        };
        let tags: Vec<&str> = info.trim().split(',').map(str::trim).collect();
        if tags.first() != Some(&"xenith") {
            continue;
        }
        let in_fn = tags.contains(&"in-fn");

        let mut body = String::new();
        for (_, content) in lines.by_ref() {
            if content.trim_start().starts_with("```") {
                break;
            }
            body.push_str(content);
            body.push('\n');
        }

        let source = if in_fn {
            format!("fn __example() {{\n{body}}}\n")
        } else {
            body
        };

        blocks.push(Block {
            file: path.to_path_buf(),
            line: index + 1,
            source,
            planned: tags.contains(&"planned"),
        });
    }

    blocks
}

#[test]
fn every_documented_example_parses() {
    let root = repo_root();
    let mut files = Vec::new();
    for dir in ["", "design", "spec", "examples", "bench"] {
        let path = if dir.is_empty() {
            root.clone()
        } else {
            root.join(dir)
        };
        if dir.is_empty() {
            // Top level: only the README, not a recursive walk.
            let readme = path.join("README.md");
            if readme.exists() {
                files.push(readme);
            }
        } else {
            markdown_files(&path, &mut files);
        }
    }

    assert!(
        !files.is_empty(),
        "found no Markdown to check; the repository layout must have moved"
    );

    let mut failures = Vec::new();
    let mut checked = 0;
    let mut planned = 0;

    for file in &files {
        for block in xenith_blocks(file) {
            let relative_dir = block
                .file
                .strip_prefix(&root)
                .unwrap_or(&block.file)
                .to_path_buf();

            if block.planned {
                // A design note may describe a language that does not exist.
                // Nothing a reader is meant to copy may.
                assert!(
                    relative_dir.starts_with("design"),
                    "{}:{} is tagged `planned` outside design/; \
                     the README, the specification and the examples must only \
                     show syntax that compiles today",
                    relative_dir.display(),
                    block.line
                );
                planned += 1;
                continue;
            }

            checked += 1;
            let parsed = parse(&block.source);
            if !parsed.diagnostics.is_empty() {
                let relative = block
                    .file
                    .strip_prefix(&root)
                    .unwrap_or(&block.file)
                    .display()
                    .to_string();
                let problems: Vec<String> = parsed
                    .diagnostics
                    .iter()
                    .map(|d| format!("{}: {}", d.code.id(), d.message))
                    .collect();
                failures.push(format!(
                    "{relative}:{} — {}\n{}",
                    block.line,
                    problems.join("; "),
                    block.source.trim_end()
                ));
            }
        }
    }

    assert!(
        failures.is_empty(),
        "{} documented example(s) do not parse:\n\n{}",
        failures.len(),
        failures.join("\n\n")
    );

    // A guard against the check quietly becoming a no-op if the fence tag or
    // the directory layout changes.
    assert!(
        checked >= 3,
        "only {checked} xenith blocks found; the extractor has probably stopped matching"
    );

    // Not a failure, but worth seeing: every `planned` block is syntax the
    // documentation promises and the compiler does not yet deliver.
    println!("checked {checked} example(s); {planned} describe unimplemented syntax");
}

#[test]
fn the_example_files_themselves_parse() {
    let root = repo_root();
    let mut failures = Vec::new();

    let entries = std::fs::read_dir(root.join("examples")).expect("examples directory");
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().is_some_and(|e| e == "xn") {
            let source = std::fs::read_to_string(&path).expect("readable example");
            let parsed = parse(&source);
            if !parsed.diagnostics.is_empty() {
                failures.push(format!(
                    "{}: {}",
                    path.display(),
                    parsed
                        .diagnostics
                        .iter()
                        .map(|d| d.code.id())
                        .collect::<Vec<_>>()
                        .join(", ")
                ));
            }
        }
    }

    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
