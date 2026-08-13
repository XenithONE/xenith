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
    /// Print a project's public API surface: per module, the pub functions,
    /// structs, enums and consts with their effect sets, in deterministic
    /// order.
    ///
    /// The answer describes what callers may write — an API map, not wiring:
    /// it does not place `use` lines or connect modules. Scope with
    /// `--module` when the full surface would be too much to read.
    Api {
        /// The project: its root directory, or any path inside it.
        project: PathBuf,
        /// Restrict to one module and its submodules, dotted
        /// ("game.player"). An unknown module is an error.
        #[arg(long)]
        module: Option<String>,
        /// Emit the machine-readable form (carries `api_schema_version`).
        #[arg(long)]
        json: bool,
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
        Command::Api {
            project,
            module,
            json,
        } => api(&project, module.as_deref(), json),
    }
}

/// `xenith api` (design/0013 §2): the ApiSurface model rendered as text or
/// JSON, optionally scoped to one module subtree.
fn api(project_path: &Path, module: Option<&str>, json: bool) -> ExitCode {
    let project = match xenith_driver::project::project_at(project_path, None) {
        Ok(project) => project,
        Err(error) => {
            eprintln!("{error}");
            return ExitCode::FAILURE;
        }
    };
    let surface = match xenith_driver::api::surface(&project) {
        Ok(surface) => surface,
        Err(error) => {
            eprintln!("{error}");
            return ExitCode::FAILURE;
        }
    };
    let scoped = match module {
        Some(module) => match surface.scoped(module) {
            Some(scoped) => scoped,
            None => {
                let known: Vec<&str> = surface.modules.iter().map(|m| m.path.as_str()).collect();
                eprintln!(
                    "no module `{module}` in the project at `{}` — modules: {}",
                    project.root.display(),
                    known.join(", ")
                );
                return ExitCode::FAILURE;
            }
        },
        None => surface,
    };
    if json {
        let rendered = xenith_driver::api::render_json(&scoped);
        println!(
            "{}",
            serde_json::to_string_pretty(&rendered).unwrap_or_default()
        );
    } else {
        print!("{}", xenith_driver::api::render_text(&scoped));
    }
    ExitCode::SUCCESS
}

fn run(path: &Path, teaching: Teaching) -> ExitCode {
    // A file inside a project runs as the project (design/0010 §2): the
    // entry is `src/main.xn`, whichever file was named. The one pipeline
    // decides which (design/0013 §1).
    let request = xenith_driver::project::ProjectRequest {
        path,
        mode: xenith_driver::project::ModeRequest::Auto,
        containment: None,
    };
    let source = match xenith_driver::project::snapshot(&request) {
        Ok(xenith_driver::project::ProjectSnapshot::Project { project, .. }) => {
            return run_project(&project, teaching);
        }
        Ok(xenith_driver::project::ProjectSnapshot::SingleFile { source, .. }) => source,
        Err(error) => {
            eprintln!("{error}");
            return ExitCode::from(2);
        }
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
    let mut roots_done: Vec<PathBuf> = Vec::new();

    for path in paths {
        // A path inside a project checks the whole project, once. The one
        // pipeline decides which mode a path gets (design/0013 §1).
        let request = xenith_driver::project::ProjectRequest {
            path,
            mode: xenith_driver::project::ModeRequest::Auto,
            containment: None,
        };
        match xenith_driver::project::snapshot(&request) {
            Ok(xenith_driver::project::ProjectSnapshot::Project { project, .. }) => {
                if roots_done.contains(&project.root) {
                    continue;
                }
                roots_done.push(project.root.clone());
                findings.extend(project_findings(&project, teaching));
            }
            Ok(xenith_driver::project::ProjectSnapshot::SingleFile { source, .. }) => {
                let mut analysis = xenith_driver::analyze_source(&source);
                strip_teaches(&mut analysis.diagnostics, teaching);
                findings.push((path.clone(), source, analysis.diagnostics));
            }
            Err(error) => {
                eprintln!("{error}");
                failed = true;
            }
        }
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

/// `xenith goals`, project-aware: a path inside a project reports the whole
/// project's holes — the requested file first, the rest in path order — with
/// every module's declarations in view, the same answer the MCP `goals` tool
/// gives (design/0013 §1). Outside a project nothing changes.
fn goals(paths: &[PathBuf], json: bool) -> ExitCode {
    let mut failed = false;
    let mut reports: Vec<render::GoalReport> = Vec::new();
    let mut roots_done: Vec<PathBuf> = Vec::new();

    for path in paths {
        match snapshot_of(path) {
            Ok(xenith_driver::project::ProjectSnapshot::Project { project, requested }) => {
                if roots_done.contains(&project.root) {
                    continue;
                }
                roots_done.push(project.root.clone());
                note_layout(&project);
                let analyzed = xenith_driver::project::analyze(&project);
                let root = project.root.display().to_string();
                for index in prioritized(&project, requested) {
                    let file = &project.files[index];
                    reports.push(render::GoalReport {
                        path: shown_path(&project, &file.rel),
                        source: file.source.clone(),
                        goals: analyzed.goals[index].clone(),
                        problems: analyzed.diagnostics[index].len(),
                        project_root: Some(root.clone()),
                    });
                }
            }
            Ok(xenith_driver::project::ProjectSnapshot::SingleFile { source, .. }) => {
                let analysis = xenith_driver::analyze_source(&source);
                reports.push(render::GoalReport {
                    path: path.clone(),
                    source,
                    goals: analysis.goals,
                    problems: analysis.diagnostics.len(),
                    project_root: None,
                });
            }
            Err(error) => {
                eprintln!("{error}");
                failed = true;
            }
        }
    }

    if json {
        println!("{}", render::goals_json(&reports));
    } else {
        let mut total = 0usize;
        for report in &reports {
            total += report.goals.len();
            let index = LineIndex::new(&report.source);
            for goal in &report.goals {
                print!(
                    "{}",
                    render::goal(&report.path, &report.source, &index, goal)
                );
            }
            if report.problems > 0 {
                eprintln!(
                    "note: {} has {} diagnostic(s); run `xenith check` to see them",
                    report.path.display(),
                    report.problems
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

/// `xenith query type-at`, project-aware: inside a project the file is
/// checked with every module's declarations in view, so a cross-module type
/// answers qualified instead of reading as unknown (design/0013 §1).
fn type_at(path: &Path, at: &str, json: bool) -> ExitCode {
    let Some((line, column)) = parse_position(at) else {
        eprintln!("--at takes line:column, one-based — for example --at 59:5");
        return ExitCode::FAILURE;
    };

    let snapshot = match snapshot_of(path) {
        Ok(snapshot) => snapshot,
        Err(error) => {
            eprintln!("{error}");
            return ExitCode::FAILURE;
        }
    };

    let (probe, root) = match &snapshot {
        xenith_driver::project::ProjectSnapshot::Project { project, requested } => {
            note_layout(project);
            let Some(file) = *requested else {
                eprintln!(
                    "`{}` is not a module source of the project at `{}`",
                    path.display(),
                    project.root.display()
                );
                return ExitCode::FAILURE;
            };
            let source = &project.files[file].source;
            let Some(offset) = offset_at(path, source, line, column) else {
                return ExitCode::FAILURE;
            };
            (
                xenith_driver::project::type_at(project, file, offset),
                Some(project.root.display().to_string()),
            )
        }
        xenith_driver::project::ProjectSnapshot::SingleFile { source, .. } => {
            let Some(offset) = offset_at(path, source, line, column) else {
                return ExitCode::FAILURE;
            };
            let parsed = parse(source);
            (xenith_sema::type_at(&parsed.module, offset), None)
        }
    };

    let Some(probe) = probe else {
        eprintln!(
            "{}:{line}:{column} is not inside an expression — try a position on a value",
            path.display()
        );
        return ExitCode::FAILURE;
    };

    if json {
        let mut rendered =
            xenith_driver::wire::probe(&path.display().to_string(), line, column, &probe);
        xenith_driver::wire::stamp_mode(&mut rendered, snapshot.analysis_mode(), root.as_deref());
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

/// `xenith query producers`, project-aware: inside a project the scope is
/// the file's own — its items, the pub items of the modules it `use`s, and
/// the prelude (design/0010 §6, design/0013 §1).
fn producers(path: &Path, type_text: &str, json: bool) -> ExitCode {
    let snapshot = match snapshot_of(path) {
        Ok(snapshot) => snapshot,
        Err(error) => {
            eprintln!("{error}");
            return ExitCode::FAILURE;
        }
    };

    let (found, root) = match &snapshot {
        xenith_driver::project::ProjectSnapshot::Project { project, requested } => {
            note_layout(project);
            let Some(file) = *requested else {
                eprintln!(
                    "`{}` is not a module source of the project at `{}`",
                    path.display(),
                    project.root.display()
                );
                return ExitCode::FAILURE;
            };
            (
                xenith_driver::project::producers(project, file, type_text),
                Some(project.root.display().to_string()),
            )
        }
        xenith_driver::project::ProjectSnapshot::SingleFile { source, .. } => {
            let parsed = parse(source);
            (xenith_sema::producers(&parsed.module, type_text), None)
        }
    };

    let found = match found {
        Ok(found) => found,
        Err(message) => {
            eprintln!("{message}");
            return ExitCode::FAILURE;
        }
    };

    if json {
        let mut rendered = xenith_driver::wire::producers(&found);
        xenith_driver::wire::stamp_mode(&mut rendered, snapshot.analysis_mode(), root.as_deref());
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

/// The one pipeline, as the unconfined CLI asks for it (design/0013 §1):
/// project when a manifest governs the path, single-file otherwise, and
/// never single-file because discovery failed.
fn snapshot_of(
    path: &Path,
) -> Result<xenith_driver::project::ProjectSnapshot, xenith_driver::project::SnapshotError> {
    xenith_driver::project::snapshot(&xenith_driver::project::ProjectRequest {
        path,
        mode: xenith_driver::project::ModeRequest::Auto,
        containment: None,
    })
}

/// A layout problem means the module map is only partly trustworthy. The
/// CLI has no `mode` flag to fall back to, so the answer is given and the
/// caveat is stated rather than withheld — never silently.
fn note_layout(project: &xenith_driver::project::Project) {
    for (rel, diagnostic) in &project.layout {
        eprintln!(
            "note: {}: {}: {} — the project's module map is incomplete; \
             run `xenith check` for the full report",
            rel,
            diagnostic.code.id(),
            diagnostic.message
        );
    }
}

/// Indices into `project.files`, the requested file first and the rest in
/// path order — the same priority the project wire responses use.
fn prioritized(project: &xenith_driver::project::Project, requested: Option<usize>) -> Vec<usize> {
    let mut order: Vec<usize> = (0..project.files.len()).collect();
    order.sort_by(|a, b| {
        let a_first = Some(*a) == requested;
        let b_first = Some(*b) == requested;
        b_first
            .cmp(&a_first)
            .then_with(|| project.files[*a].rel.cmp(&project.files[*b].rel))
    });
    order
}

/// A project file's path as the CLI spells it: `<root>/src/<rel>`, with the
/// host's separators — the same spelling `check` reports.
fn shown_path(project: &xenith_driver::project::Project, rel: &str) -> PathBuf {
    let mut shown = project.root.join("src");
    for part in rel.split('/') {
        shown.push(part);
    }
    shown
}

/// One-based line:column to a byte offset, reporting the out-of-file case.
fn offset_at(path: &Path, source: &str, line: u32, column: u32) -> Option<u32> {
    let index = LineIndex::new(source);
    let offset = xenith_driver::wire::position_to_offset(source, &index, line, column);
    if offset.is_none() {
        eprintln!("{}:{line}:{column} is outside the file", path.display());
    }
    offset
}

/// One report entry per project file, layout problems included — a layout
/// diagnostic renders like any other, just without a source excerpt.
fn project_findings(
    project: &xenith_driver::project::Project,
    teaching: Teaching,
) -> Vec<(PathBuf, String, Vec<Diagnostic>)> {
    let mut per_file = xenith_driver::project::analyze(project).diagnostics;
    let mut findings = Vec::new();
    for (rel, diagnostic) in &project.layout {
        findings.push((
            project.root.join(rel),
            String::new(),
            vec![diagnostic.clone()],
        ));
    }
    for (file, mut diagnostics) in project.files.iter().zip(per_file.drain(..)) {
        strip_teaches(&mut diagnostics, teaching);
        let mut shown = project.root.join("src");
        for part in file.rel.split('/') {
            shown.push(part);
        }
        findings.push((shown, file.source.clone(), diagnostics));
    }
    findings
}

/// Check, then execute a whole project. Refusal mirrors the single-file
/// rule: any diagnostic anywhere means nothing runs.
fn run_project(project: &xenith_driver::project::Project, teaching: Teaching) -> ExitCode {
    let findings = project_findings(project, teaching);
    let mut any = false;
    for (path, source, diagnostics) in &findings {
        let index = LineIndex::new(source);
        for diagnostic in diagnostics {
            any = true;
            print!("{}", render::diagnostic(path, source, &index, diagnostic));
        }
    }
    if any {
        eprintln!(
            "{}: not run — fix the diagnostics first",
            project.root.display()
        );
        return ExitCode::from(2);
    }

    let table = xenith_driver::project::analyze(project).table;
    let modules: Vec<(String, &xenith_syntax::ast::Module)> = project
        .files
        .iter()
        .map(|file| (file.module.clone(), &file.parsed.module))
        .collect();
    let outcome = xenith_vm::run_project(&modules, &table);

    use std::io::Write;
    let _ = std::io::stdout().write_all(&outcome.stdout);
    let _ = std::io::stdout().flush();

    if let Some((message, _)) = outcome.error {
        eprintln!("{}: runtime error: {message}", project.root.display());
    }
    ExitCode::from(u8::try_from(outcome.exit).unwrap_or(101))
}

/// `--diagnostic-teaching=off` strips exactly the teaching — the teach
/// blocks and the module-call teach note — and nothing else, which is what
/// makes the byte-identity guarantee testable.
fn strip_teaches(diagnostics: &mut [Diagnostic], teaching: Teaching) {
    if teaching == Teaching::Off {
        for diagnostic in diagnostics {
            diagnostic.strip_teaching();
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
