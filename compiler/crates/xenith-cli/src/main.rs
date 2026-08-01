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
}

fn main() -> ExitCode {
    match Cli::parse().command {
        Command::Check { paths, json } => check(&paths, json),
        Command::Fmt { paths, check } => fmt(&paths, check),
        Command::Explain { code } => explain(code.as_deref()),
        Command::Goals { paths, json } => goals(&paths, json),
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
