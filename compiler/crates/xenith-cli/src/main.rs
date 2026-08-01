//! The `xenith` command.
//!
//! Every subcommand that reports problems can emit JSON. That is not a
//! convenience: the compiler's output is a protocol consumed by tools and
//! models, and the human rendering is a view over it rather than the source of
//! truth. See `design/0002-design-review.md`.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use xenith_diag::{DiagCode, Diagnostic, LineIndex};
use xenith_sema::analyze;
use xenith_syntax::{FormatError, format, parse};

mod render;

#[derive(Parser)]
#[command(name = "xenith", version, about = "The Xenith compiler")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Parse and report diagnostics without producing output.
    Check {
        /// Files to check.
        #[arg(required = true)]
        paths: Vec<PathBuf>,
        /// Emit machine-readable diagnostics.
        #[arg(long)]
        json: bool,
    },
    /// Rewrite source into canonical form.
    ///
    /// The formatter has no options: the same meaning always produces the same
    /// bytes. It also verifies its own output and refuses to write anything it
    /// cannot prove meaning-preserving.
    Fmt {
        /// Files to format.
        #[arg(required = true)]
        paths: Vec<PathBuf>,
        /// Report which files would change and exit non-zero, without writing.
        #[arg(long)]
        check: bool,
    },
    /// Explain a diagnostic code in full, for example `XN0002`.
    Explain {
        /// The code to explain. Omit to list every code.
        code: Option<String>,
    },
    /// Report every hole: the type required there, what is in scope, and
    /// which effects are permitted.
    ///
    /// A hole (`??` or `??name`) is a legal program element, so a partial
    /// program is something to query, not something to fix. This command is
    /// how a tool — or a model — asks "what belongs here?".
    Goals {
        /// Files to inspect.
        #[arg(required = true)]
        paths: Vec<PathBuf>,
        /// Emit machine-readable goals.
        #[arg(long)]
        json: bool,
    },
    /// Ask the compiler a question about a file.
    ///
    /// A query is a hole the author did not have to write: the answer comes
    /// from the same traversal that answers `goals`, so partial programs
    /// answer like any other.
    Query {
        #[command(subcommand)]
        question: QueryCommand,
    },
}

#[derive(Subcommand)]
enum QueryCommand {
    /// The type of the expression at a position, and what surrounds it.
    TypeAt {
        path: PathBuf,
        /// Position as line:column, one-based, counting characters.
        #[arg(long)]
        at: String,
        /// Emit the answer as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Everything in the module that can produce a given type.
    ///
    /// This is the anti-hallucination query: instead of guessing a function
    /// name, ask which ones return `Result<Player, ScoreError>`.
    Producers {
        path: PathBuf,
        /// The type, spelled as in source: "Result<Player, ScoreError>".
        #[arg(value_name = "TYPE")]
        type_text: String,
        /// Emit the answer as JSON.
        #[arg(long)]
        json: bool,
    },
}

fn main() -> ExitCode {
    match Cli::parse().command {
        Command::Check { paths, json } => check(&paths, json),
        Command::Fmt { paths, check } => fmt(&paths, check),
        Command::Explain { code } => explain(code.as_deref()),
        Command::Goals { paths, json } => goals(&paths, json),
        Command::Query { question } => match question {
            QueryCommand::TypeAt { path, at, json } => type_at(&path, &at, json),
            QueryCommand::Producers {
                path,
                type_text,
                json,
            } => producers(&path, &type_text, json),
        },
    }
}

fn check(paths: &[PathBuf], json: bool) -> ExitCode {
    let mut findings: Vec<(PathBuf, String, Vec<Diagnostic>)> = Vec::new();
    let mut failed = false;

    for path in paths {
        let Some(source) = read(path, &mut failed) else {
            continue;
        };
        let parsed = parse(&source);
        // The checker runs even over a tree with parse errors in it —
        // recovery nodes are ordinary nodes, and a model mid-edit still
        // deserves type information about the parts that did parse.
        let analysis = analyze(&parsed.module);
        let mut diagnostics = parsed.diagnostics;
        diagnostics.extend(analysis.diagnostics);
        diagnostics.sort_by_key(|d| d.span.start);
        findings.push((path.clone(), source, diagnostics));
    }

    let has_errors = findings
        .iter()
        .any(|(_, _, diagnostics)| diagnostics.iter().any(Diagnostic::is_error));

    if json {
        println!("{}", render::diagnostics_json(&findings));
    } else {
        for (path, source, diagnostics) in &findings {
            let index = LineIndex::new(source);
            for diagnostic in diagnostics {
                print!("{}", render::diagnostic(path, source, &index, diagnostic));
            }
        }
        let count: usize = findings.iter().map(|(_, _, d)| d.len()).sum();
        if count == 0 {
            eprintln!("no problems found");
        }
    }

    if failed || has_errors {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

fn fmt(paths: &[PathBuf], check_only: bool) -> ExitCode {
    let mut failed = false;
    let mut would_change: Vec<&Path> = Vec::new();

    for path in paths {
        let Some(source) = read(path, &mut failed) else {
            continue;
        };

        match format(&source) {
            Ok(formatted) if formatted == source => {}
            Ok(formatted) => {
                if check_only {
                    would_change.push(path);
                } else if let Err(error) = std::fs::write(path, &formatted) {
                    eprintln!("{}: {error}", path.display());
                    failed = true;
                }
            }
            Err(FormatError::Unparsable(diagnostics)) => {
                // Formatting source the compiler cannot read would be guessing,
                // so report the parse problems instead and leave the file be.
                let index = LineIndex::new(&source);
                for diagnostic in &diagnostics {
                    print!("{}", render::diagnostic(path, &source, &index, diagnostic));
                }
                eprintln!("{}: not formatted, source does not parse", path.display());
                failed = true;
            }
            Err(error) => {
                // The remaining variants mean the formatter could not prove its
                // own output safe. Refusing is the whole point.
                eprintln!("{}: {error}", path.display());
                eprintln!("  this is a bug in xenith; the file was left unchanged");
                failed = true;
            }
        }
    }

    for path in &would_change {
        println!("{}", path.display());
    }

    if failed || (check_only && !would_change.is_empty()) {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

fn explain(code: Option<&str>) -> ExitCode {
    let Some(code) = code else {
        for code in DiagCode::ALL {
            println!("{}  {}", code.id(), first_line(code.explain()));
        }
        return ExitCode::SUCCESS;
    };

    // Accept `xn0002` as readily as `XN0002`; a model reading an error message
    // should not have to think about case.
    let normalised = code.to_ascii_uppercase();
    match DiagCode::from_id(&normalised) {
        Some(found) => {
            println!("{}\n", found.id());
            println!("{}", found.explain());
            ExitCode::SUCCESS
        }
        None => {
            eprintln!("unknown diagnostic code `{code}`");
            eprintln!("run `xenith explain` with no argument to list every code");
            ExitCode::FAILURE
        }
    }
}

fn goals(paths: &[PathBuf], json: bool) -> ExitCode {
    let mut failed = false;
    let mut reports = Vec::new();

    for path in paths {
        let Some(source) = read(path, &mut failed) else {
            continue;
        };
        let parsed = parse(&source);
        let analysis = analyze(&parsed.module);
        let problem_count = parsed.diagnostics.len() + analysis.diagnostics.len();
        reports.push((path.clone(), source, analysis.goals, problem_count));
    }

    if json {
        println!("{}", render::goals_json(&reports));
    } else {
        let mut total = 0usize;
        for (path, source, goals, problems) in &reports {
            total += goals.len();
            let index = LineIndex::new(source);
            for goal in goals {
                print!("{}", render::goal(path, source, &index, goal));
            }
            if *problems > 0 {
                eprintln!(
                    "note: {} has {problems} diagnostic(s); run `xenith check` to see them",
                    path.display()
                );
            }
        }
        if total == 0 {
            eprintln!("no holes found");
        }
    }

    if failed {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

fn type_at(path: &Path, at: &str, json: bool) -> ExitCode {
    let mut failed = false;
    let Some(source) = read(path, &mut failed) else {
        return ExitCode::FAILURE;
    };

    let Some((line, column)) = parse_position(at) else {
        eprintln!("--at takes line:column, one-based — for example --at 59:5");
        return ExitCode::FAILURE;
    };
    let index = LineIndex::new(&source);
    let Some(offset) = offset_of(&source, &index, line, column) else {
        eprintln!("{}:{line}:{column} is outside the file", path.display());
        return ExitCode::FAILURE;
    };

    let parsed = parse(&source);
    let Some(probe) = xenith_sema::type_at(&parsed.module, offset) else {
        eprintln!(
            "{}:{line}:{column} is not inside an expression — try a position on a value",
            path.display()
        );
        return ExitCode::FAILURE;
    };

    if json {
        let rendered = serde_json::json!({
            "file": path.display().to_string(),
            "line": line,
            "column": column,
            "type": probe.ty,
            "enclosing_function": probe.enclosing_function,
            "in_scope": probe
                .in_scope
                .iter()
                .map(|(name, ty)| serde_json::json!({ "name": name, "type": ty }))
                .collect::<Vec<_>>(),
            "allowed_effects": probe.allowed_effects,
        });
        println!(
            "{}",
            serde_json::to_string_pretty(&rendered).unwrap_or_default()
        );
    } else {
        println!("{}:{line}:{column} — {}", path.display(), probe.ty);
        println!("  in {}", probe.enclosing_function);
        if probe.in_scope.is_empty() {
            println!("  in scope: (nothing)");
        } else {
            let listed: Vec<String> = probe
                .in_scope
                .iter()
                .map(|(name, ty)| format!("{name}: {ty}"))
                .collect();
            println!("  in scope: {}", listed.join(", "));
        }
        if probe.allowed_effects.is_empty() {
            println!("  effects:  none permitted");
        } else {
            println!("  effects:  {}", probe.allowed_effects.join(", "));
        }
    }
    ExitCode::SUCCESS
}

fn producers(path: &Path, type_text: &str, json: bool) -> ExitCode {
    let mut failed = false;
    let Some(source) = read(path, &mut failed) else {
        return ExitCode::FAILURE;
    };
    let parsed = parse(&source);

    let found = match xenith_sema::producers(&parsed.module, type_text) {
        Ok(found) => found,
        Err(message) => {
            eprintln!("{message}");
            return ExitCode::FAILURE;
        }
    };

    if json {
        let rendered: Vec<serde_json::Value> = found
            .iter()
            .map(|p| {
                serde_json::json!({
                    "kind": p.kind,
                    "symbol": p.symbol,
                    "signature": p.signature,
                    "effects": p.effects,
                })
            })
            .collect();
        println!(
            "{}",
            serde_json::to_string_pretty(&rendered).unwrap_or_default()
        );
    } else if found.is_empty() {
        println!("nothing in {} produces {type_text}", path.display());
    } else {
        println!("producers of {type_text}:");
        for producer in &found {
            println!("  {:<9} {}", producer.kind, producer.signature);
        }
    }
    ExitCode::SUCCESS
}

/// `"59:5"` → `(59, 5)`.
fn parse_position(text: &str) -> Option<(u32, u32)> {
    let (line, column) = text.split_once(':')?;
    let line = line.trim().parse().ok()?;
    let column = column.trim().parse().ok()?;
    if line == 0 || column == 0 {
        return None;
    }
    Some((line, column))
}

/// One-based line and character column to a byte offset.
fn offset_of(source: &str, index: &LineIndex, line: u32, column: u32) -> Option<u32> {
    let start = index.line_start(line)?;
    let text = index.line_text(source, line)?;
    let mut seen = 0u32;
    for (byte_offset, _) in text.char_indices() {
        seen += 1;
        if seen == column {
            return Some(start + byte_offset as u32);
        }
    }
    // One past the last character addresses the end of the line.
    if column == seen + 1 {
        Some(start + text.len() as u32)
    } else {
        None
    }
}

fn read(path: &Path, failed: &mut bool) -> Option<String> {
    match std::fs::read_to_string(path) {
        Ok(source) => Some(source),
        Err(error) => {
            eprintln!("{}: {error}", path.display());
            *failed = true;
            None
        }
    }
}

fn first_line(text: &str) -> &str {
    text.lines().next().unwrap_or("")
}
