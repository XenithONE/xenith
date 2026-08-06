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
        /// Attach knowledge to diagnostics that carry it: the callee's
        /// signature, the receiver's method catalogue. `off` reproduces the
        /// pre-teaching output byte for byte.
        #[arg(long = "diagnostic-teaching", value_enum, default_value_t = Teaching::On)]
        diagnostic_teaching: Teaching,
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
    /// Type-check and execute a file's `fn main`.
    ///
    /// A file with diagnostics is refused — run `xenith check` first. A file
    /// with holes runs: reaching one is a precise trap naming the hole, so
    /// running a partial program tells you which hole to fill next.
    ///
    /// Exit codes: 0 = main succeeded; 1 = main returned `Err`;
    /// 2 = refused (diagnostics); 101 = a runtime trap fired.
    Run {
        path: PathBuf,
        /// As on `check`: the refusal path renders the same diagnostics.
        #[arg(long = "diagnostic-teaching", value_enum, default_value_t = Teaching::On)]
        diagnostic_teaching: Teaching,
    },
}

/// Whether diagnostics carry their teaching blocks (design/0009 §3).
#[derive(Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
enum Teaching {
    On,
    Off,
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
        Command::Check {
            paths,
            json,
            diagnostic_teaching,
        } => check(&paths, json, diagnostic_teaching),
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
        Command::Run {
            path,
            diagnostic_teaching,
        } => run(&path, diagnostic_teaching),
    }
}

fn run(path: &Path, teaching: Teaching) -> ExitCode {
    let mut failed = false;
    let Some(source) = read(path, &mut failed) else {
        return ExitCode::from(2);
    };

    // Running a program with diagnostics would be executing guesses.
    let mut analysis = xenith_driver::analyze_source(&source);
    if !analysis.diagnostics.is_empty() {
        strip_teaches(&mut analysis.diagnostics, teaching);
        let index = LineIndex::new(&source);
        for diagnostic in &analysis.diagnostics {
            print!("{}", render::diagnostic(path, &source, &index, diagnostic));
        }
        eprintln!("{}: not run — fix the diagnostics first", path.display());
        return ExitCode::from(2);
    }

    let parsed = parse(&source);
    let (table, _) = xenith_sema::def::collect(&parsed.module);
    let outcome = xenith_vm::run(&parsed.module, &table);

    use std::io::Write;
    let _ = std::io::stdout().write_all(&outcome.stdout);
    let _ = std::io::stdout().flush();

    if let Some((message, span)) = outcome.error {
        let index = LineIndex::new(&source);
        let at = index.line_col(&source, span.start);
        eprintln!(
            "{}:{}:{}: runtime error: {message}",
            path.display(),
            at.line,
            at.column
        );
    }

    ExitCode::from(u8::try_from(outcome.exit).unwrap_or(101))
}

fn check(paths: &[PathBuf], json: bool, teaching: Teaching) -> ExitCode {
    let mut findings: Vec<(PathBuf, String, Vec<Diagnostic>)> = Vec::new();
    let mut failed = false;

    for path in paths {
        let Some(source) = read(path, &mut failed) else {
            continue;
        };
        let mut analysis = xenith_driver::analyze_source(&source);
        strip_teaches(&mut analysis.diagnostics, teaching);
        findings.push((path.clone(), source, analysis.diagnostics));
    }

    let has_errors = findings
        .iter()
        .any(|(_, _, diagnostics)| diagnostics.iter().any(Diagnostic::is_error));

    if json {
        println!(
            "{}",
            render::diagnostics_json(&findings, teaching == Teaching::On)
        );
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
        let analysis = xenith_driver::analyze_source(&source);
        reports.push((
            path.clone(),
            source,
            analysis.goals,
            analysis.diagnostics.len(),
        ));
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
    let Some(offset) = xenith_driver::wire::position_to_offset(&source, &index, line, column)
    else {
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
        let rendered =
            xenith_driver::wire::probe(&path.display().to_string(), line, column, &probe);
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
        let rendered = xenith_driver::wire::producers(&found);
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

/// `--diagnostic-teaching=off` strips exactly the teaches and nothing else,
/// which is what makes the byte-identity guarantee testable.
fn strip_teaches(diagnostics: &mut [Diagnostic], teaching: Teaching) {
    if teaching == Teaching::Off {
        for diagnostic in diagnostics {
            diagnostic.teaches.clear();
        }
    }
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
