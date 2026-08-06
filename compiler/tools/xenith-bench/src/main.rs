//! The benchmark harness — the project's objective function, executable.
//!
//! Two subcommands with very different costs:
//!
//! - `verify` runs every task's **reference solution** through check and run
//!   and compares stdout. No model is involved; it belongs in CI. A task
//!   whose reference fails is a broken task, and measuring models against
//!   broken tasks produces numbers that lie.
//! - `run` drives a subscription CLI (codex / grok / agy / opencode) through
//!   the tasks under one condition, with repair rounds. It is deliberately
//!   not run in CI: the CLIs live on the maintainer's machine, and results
//!   are committed to `bench/ai/results/` with the date and versions.
//!
//! Conditions (see bench/ai/README.md for the deviation note):
//! `bare` — no documentation at all, the lower control.
//! `full-pack` — the field guide in context.
//! `hole-guided` — the field guide plus the hole workflow: each round feeds
//! back `xenith goals` alongside diagnostics.
//! `docs` / `query` / `docs-query` / `blind` — the 2×2 separation arms of
//! design/0007 §5: "std API table in the guide" crossed with "goals/producers
//! in the feedback". Everything else — rounds cap, prompt skeleton,
//! diagnostics, tasks — is identical across the four, so a gap between cells
//! is attributable to the two factors and nothing else.
//! `v3-plain` / `v3-teach` / `v3-docs` / `v3-docs-teach` — the 0009 §4
//! teaching arms: the docs factor crossed with "diagnostics carry their
//! `teaches` section" (the off arms pass `--diagnostic-teaching=off` to every
//! compiler call). Teaching exists only in post-failure compiler output, so
//! round-1 prompts are byte-identical across that factor, and goals-on-holes
//! stays disabled in all four arms — teaching is the only feedback
//! difference.

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Instant;

use clap::{Parser, Subcommand, ValueEnum};
use serde::Deserialize;

#[derive(Parser)]
#[command(name = "xenith-bench", about = "AI benchmark harness for Xenith")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Check every task's reference solution: parse, type-check, run, compare
    /// stdout. Fails loudly if any task is broken. Safe for CI.
    Verify,
    /// Drive one model through the tasks under one condition. Local only.
    Run {
        #[arg(long)]
        model: Model,
        #[arg(long)]
        condition: Condition,
        /// Limit to specific task names (default: all).
        #[arg(long)]
        task: Vec<String>,
        /// Maximum attempts per task (1 initial + repairs).
        #[arg(long, default_value_t = 4)]
        rounds: u32,
        /// Seconds to wait for one model reply before giving up on the round.
        #[arg(long, default_value_t = 300)]
        timeout: u64,
    },
    /// Regenerate results/summary.md from the canonical result files.
    /// Numbers typed into prose drift; generated numbers cannot.
    Summarize,
}

/// One column of the matrix. Mostly one CLI = one model, with two deliberate
/// exceptions: `cursor` is Cursor's Auto router, which picks among several
/// underlying models per call — a mixture, useful as a diversity probe, and
/// labelled as the router it is. The `opencode-*` variants reach different
/// model *families* (DeepSeek, Nemotron) through one CLI.
#[derive(Clone, Copy, ValueEnum)]
enum Model {
    Codex,
    Grok,
    Agy,
    Opencode,
    OpencodeDeepseek,
    OpencodeNemotron,
    Cursor,
}

impl Model {
    const ALL: [Model; 7] = [
        Model::Codex,
        Model::Grok,
        Model::Agy,
        Model::Opencode,
        Model::OpencodeDeepseek,
        Model::OpencodeNemotron,
        Model::Cursor,
    ];

    fn name(self) -> &'static str {
        match self {
            Model::Codex => "codex",
            Model::Grok => "grok",
            Model::Agy => "agy",
            Model::Opencode => "opencode",
            Model::OpencodeDeepseek => "opencode-deepseek",
            Model::OpencodeNemotron => "opencode-nemotron",
            Model::Cursor => "cursor",
        }
    }
}

/// The first three are the original matrix and the next four are the 0007 §5
/// separation arms; all seven stay frozen for reproducibility. The `v3-*`
/// quartet is the 0009 §4 teaching experiment. Every CLI name doubles as the
/// `{model}-{condition}.json` result-file suffix.
#[derive(Clone, Copy, ValueEnum, PartialEq)]
enum Condition {
    Bare,
    FullPack,
    HoleGuided,
    Docs,
    Query,
    DocsQuery,
    Blind,
    // The names are result-file suffixes, so they are pinned rather than
    // trusted to the derive's kebab-casing across the `V3` digit boundary.
    #[value(name = "v3-plain")]
    V3Plain,
    #[value(name = "v3-teach")]
    V3Teach,
    #[value(name = "v3-docs")]
    V3Docs,
    #[value(name = "v3-docs-teach")]
    V3DocsTeach,
}

impl Condition {
    const ALL: [Condition; 3] = [Condition::Bare, Condition::FullPack, Condition::HoleGuided];
    const SEPARATION: [Condition; 4] = [
        Condition::Docs,
        Condition::Query,
        Condition::DocsQuery,
        Condition::Blind,
    ];
    const V3: [Condition; 4] = [
        Condition::V3Plain,
        Condition::V3Teach,
        Condition::V3Docs,
        Condition::V3DocsTeach,
    ];

    fn name(self) -> &'static str {
        match self {
            Condition::Bare => "bare",
            Condition::FullPack => "full-pack",
            Condition::HoleGuided => "hole-guided",
            Condition::Docs => "docs",
            Condition::Query => "query",
            Condition::DocsQuery => "docs-query",
            Condition::Blind => "blind",
            Condition::V3Plain => "v3-plain",
            Condition::V3Teach => "v3-teach",
            Condition::V3Docs => "v3-docs",
            Condition::V3DocsTeach => "v3-docs-teach",
        }
    }

    /// The docs factor: these arms carry the std API table in the guide.
    fn has_api_table(self) -> bool {
        matches!(
            self,
            Condition::Docs | Condition::DocsQuery | Condition::V3Docs | Condition::V3DocsTeach
        )
    }

    /// The query factor: these arms get `xenith goals` appended to failing
    /// feedback. `hole-guided` predates the 2×2 and keeps its channel. No v3
    /// arm feeds goals — in v3, teaching must be the only feedback difference
    /// (0009 §4).
    fn feeds_goals(self) -> bool {
        matches!(
            self,
            Condition::HoleGuided | Condition::Query | Condition::DocsQuery
        )
    }

    /// The teaching factor (0009 §3): when false, every `check`/`run` this
    /// arm makes carries `--diagnostic-teaching=off`. Teaching is the
    /// compiler's default, and every pre-0009 arm ran before the flag
    /// existed — they keep the default so re-running a legacy cell still
    /// measures what its result file says it measured.
    fn teaching(self) -> bool {
        !matches!(self, Condition::V3Plain | Condition::V3Docs)
    }

    /// The 0009 arms store each failed round's feedback verbatim plus its
    /// diagnostic codes — the consumption-oracle raw material (0009 §1b).
    /// Teach-off rounds are recorded too: they are the oracle's control text.
    fn records_feedback(self) -> bool {
        matches!(
            self,
            Condition::V3Plain | Condition::V3Teach | Condition::V3Docs | Condition::V3DocsTeach
        )
    }
}

#[derive(Deserialize)]
struct Task {
    name: String,
    tier: u32,
    prompt: String,
    expected_stdout: String,
    reference: String,
}

fn main() -> ExitCode {
    let paths = Paths::locate();
    match Cli::parse().command {
        Command::Verify => verify(&paths),
        Command::Run {
            model,
            condition,
            task,
            rounds,
            timeout,
        } => run_models(&paths, model, condition, &task, rounds, timeout),
        Command::Summarize => summarize(&paths),
    }
}

// ----------------------------------------------------------------- summarize

/// One matrix cell rendered as `pass@1 · green (mean rounds-to-green)`, or
/// `—` where the cell has not been measured. Only canonical
/// `{model}-{condition}.json` files count; voided runs are archived under
/// other names precisely so this can never read them.
fn summarize(paths: &Paths) -> ExitCode {
    let mut table = String::from(
        "# Benchmark matrix\n\n\
         Generated by `xenith-bench summarize` — regenerate, don't edit. Each cell is\n\
         `pass@1 · green (mean rounds-to-green)` out of the tasks measured; `—` means\n\
         the cell has no canonical result file yet. Conditions and the measurement\n\
         story live in [README.md](../README.md).\n\n",
    );
    let (main_block, _) = matrix_block(paths, &Condition::ALL);
    table.push_str(&main_block);

    // The 0007 arms render only once at least one cell is measured: an
    // all-dash table would imply an experiment that has not started.
    let (separation_block, measured) = matrix_block(paths, &Condition::SEPARATION);
    if measured {
        table.push_str(
            "\n## Separation experiment (0007)\n\n\
             The 2×2 of design/0007 §5-1: std API table in the guide × goals/producers\n\
             in the feedback. Same cell format as above.\n\n",
        );
        table.push_str(&separation_block);
    }

    // Likewise for the 0009 teaching arms.
    let (teaching_block, measured) = matrix_block(paths, &Condition::V3);
    if measured {
        table.push_str(
            "\n## Teaching experiment (0009 v3)\n\n\
             The 2×2 of design/0009 §4: std API table in the guide × diagnostic teaching\n\
             in the compiler feedback. Same cell format as above.\n\n",
        );
        table.push_str(&teaching_block);
    }

    let out = paths.results.join("summary.md");
    if let Err(e) = std::fs::write(&out, &table) {
        eprintln!("cannot write {}: {e}", out.display());
        return ExitCode::FAILURE;
    }
    print!("{table}");
    println!("\nwrote {}", out.display());
    ExitCode::SUCCESS
}

/// One model × condition table over the given columns, plus whether any cell
/// had canonical results. Both summary tables must come from this one path,
/// or their cell formats drift apart.
fn matrix_block(paths: &Paths, conditions: &[Condition]) -> (String, bool) {
    let mut table = String::from("| model |");
    for condition in conditions {
        table.push_str(&format!(" {} |", condition.name()));
    }
    table.push_str("\n| --- |");
    for _ in conditions {
        table.push_str(" --- |");
    }
    table.push('\n');

    let mut measured = false;
    let mut totals: Vec<(u32, u32, u32)> = vec![(0, 0, 0); conditions.len()];
    for model in Model::ALL {
        table.push_str(&format!("| `{}` |", model.name()));
        for (i, condition) in conditions.iter().enumerate() {
            let file = paths
                .results
                .join(format!("{}-{}.json", model.name(), condition.name()));
            let reports = load_prior_reports(&file);
            if reports.is_empty() {
                table.push_str(" — |");
                continue;
            }
            measured = true;
            let tasks = reports.len() as u32;
            let pass1 = reports.iter().filter(|r| r.pass_at_1).count() as u32;
            let green = reports.iter().filter(|r| r.passed).count() as u32;
            let rounds: Vec<usize> = reports
                .iter()
                .filter(|r| r.passed)
                .map(|r| r.rounds.len())
                .collect();
            totals[i].0 += pass1;
            totals[i].1 += green;
            totals[i].2 += tasks;
            if rounds.is_empty() {
                table.push_str(&format!(" {pass1} · {green}/{tasks} |"));
            } else {
                let mean = rounds.iter().sum::<usize>() as f64 / rounds.len() as f64;
                table.push_str(&format!(" {pass1} · {green}/{tasks} ({mean:.1}) |"));
            }
        }
        table.push('\n');
    }
    table.push_str("| **total** |");
    for (pass1, green, tasks) in &totals {
        if *tasks == 0 {
            table.push_str(" — |");
        } else {
            table.push_str(&format!(" **{pass1} · {green}/{tasks}** |"));
        }
    }
    table.push('\n');
    (table, measured)
}

// -------------------------------------------------------------------- layout

struct Paths {
    root: PathBuf,
    xenith: PathBuf,
    tasks: PathBuf,
    scratch: PathBuf,
    results: PathBuf,
    field_guide: PathBuf,
    api_table: PathBuf,
    invoke: PathBuf,
}

impl Paths {
    fn locate() -> Paths {
        // tools/xenith-bench -> tools -> compiler -> repository root.
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .nth(3)
            .expect("repository root")
            .to_path_buf();
        Paths {
            xenith: root.join("compiler/target/debug/xenith.exe"),
            tasks: root.join("bench/ai/tasks"),
            scratch: root.join("bench/ai/scratch"),
            results: root.join("bench/ai/results"),
            field_guide: root.join("bench/ai/field-guide.md"),
            api_table: root.join("bench/ai/api-table.md"),
            invoke: root.join("bench/ai/invoke.ps1"),
            root,
        }
    }

    /// The compiler binary, building it if absent.
    fn ensure_xenith(&self) -> Result<&Path, String> {
        // On non-Windows CI the binary has no extension.
        let unix = self.root.join("compiler/target/debug/xenith");
        if self.xenith.exists() {
            return Ok(&self.xenith);
        }
        if unix.exists() {
            // Leak is fine: one path for the process lifetime.
            return Ok(Box::leak(unix.into_boxed_path()));
        }
        let status = std::process::Command::new("cargo")
            .args(["build", "-q", "-p", "xenith"])
            .current_dir(self.root.join("compiler"))
            .status()
            .map_err(|e| format!("cargo: {e}"))?;
        if !status.success() {
            return Err("building the compiler failed".to_string());
        }
        if self.xenith.exists() {
            Ok(&self.xenith)
        } else {
            Ok(Box::leak(
                self.root
                    .join("compiler/target/debug/xenith")
                    .into_boxed_path(),
            ))
        }
    }
}

fn load_tasks(dir: &Path) -> Result<Vec<Task>, String> {
    let mut tasks = Vec::new();
    let entries = std::fs::read_dir(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().is_none_or(|e| e != "toml") {
            continue;
        }
        let text =
            std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        let task: Task = toml::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
        tasks.push(task);
    }
    tasks.sort_by(|a, b| a.tier.cmp(&b.tier).then(a.name.cmp(&b.name)));
    if tasks.is_empty() {
        return Err(format!("no tasks in {}", dir.display()));
    }
    Ok(tasks)
}

// -------------------------------------------------------------------- verify

fn verify(paths: &Paths) -> ExitCode {
    let xenith = match paths.ensure_xenith() {
        Ok(path) => path.to_path_buf(),
        Err(message) => {
            eprintln!("{message}");
            return ExitCode::FAILURE;
        }
    };
    let tasks = match load_tasks(&paths.tasks) {
        Ok(tasks) => tasks,
        Err(message) => {
            eprintln!("{message}");
            return ExitCode::FAILURE;
        }
    };

    std::fs::create_dir_all(&paths.scratch).ok();
    let mut broken = 0;
    for task in &tasks {
        let file = paths.scratch.join(format!("ref-{}.xn", task.name));
        if std::fs::write(&file, &task.reference).is_err() {
            eprintln!("{}: cannot write scratch", task.name);
            broken += 1;
            continue;
        }
        // References are checked under the compiler's default (teaching on):
        // a clean reference produces no diagnostics for the flag to alter.
        match execute(&xenith, &file, true) {
            Execution::Passed { stdout } if stdout == task.expected_stdout => {
                println!("ok       {} (tier {})", task.name, task.tier);
            }
            Execution::Passed { stdout } => {
                eprintln!(
                    "BROKEN   {}: reference printed {stdout:?}, expected {:?}",
                    task.name, task.expected_stdout
                );
                broken += 1;
            }
            Execution::CheckFailed { output } => {
                eprintln!("BROKEN   {}: reference does not check\n{output}", task.name);
                broken += 1;
            }
            Execution::RunFailed { exit, error } => {
                eprintln!("BROKEN   {}: reference exited {exit}: {error}", task.name);
                broken += 1;
            }
        }
    }

    if broken > 0 {
        eprintln!("{broken} broken task(s) — a benchmark over broken tasks lies");
        ExitCode::FAILURE
    } else {
        println!("all {} references pass", tasks.len());
        ExitCode::SUCCESS
    }
}

enum Execution {
    Passed { stdout: String },
    CheckFailed { output: String },
    RunFailed { exit: i32, error: String },
}

const TEACHING_OFF: &str = "--diagnostic-teaching=off";

/// Build one compiler invocation. Every `check`/`run` the harness makes goes
/// through here, so the teaching flag cannot reach one phase and miss the
/// other.
fn xenith_cmd(
    xenith: &Path,
    subcommand: &str,
    teaching: bool,
    file: &Path,
) -> std::process::Command {
    let mut cmd = std::process::Command::new(xenith);
    cmd.arg(subcommand);
    if !teaching {
        // Strips the teaches section only; the diagnostics are otherwise
        // byte-identical (0009 §3, pinned by the compiler's frozen tests).
        cmd.arg(TEACHING_OFF);
    }
    cmd.arg(file);
    cmd
}

/// `xenith check` then `xenith run`, mirroring what a model's attempt faces.
/// `teaching` follows the condition (0009): the off arms see diagnostics
/// without the teaches section.
fn execute(xenith: &Path, file: &Path, teaching: bool) -> Execution {
    let check = xenith_cmd(xenith, "check", teaching, file).output();
    let check = match check {
        Ok(output) => output,
        Err(e) => {
            return Execution::CheckFailed {
                output: format!("cannot spawn xenith: {e}"),
            };
        }
    };
    if !check.status.success() {
        return Execution::CheckFailed {
            output: String::from_utf8_lossy(&check.stdout).to_string(),
        };
    }

    let run = xenith_cmd(xenith, "run", teaching, file).output();
    let run = match run {
        Ok(output) => output,
        Err(e) => {
            return Execution::RunFailed {
                exit: -1,
                error: format!("cannot spawn xenith: {e}"),
            };
        }
    };
    let exit = run.status.code().unwrap_or(-1);
    if exit == 0 {
        Execution::Passed {
            stdout: String::from_utf8_lossy(&run.stdout).to_string(),
        }
    } else {
        Execution::RunFailed {
            exit,
            error: String::from_utf8_lossy(&run.stderr).trim().to_string(),
        }
    }
}

// ---------------------------------------------------------------------- run

fn run_models(
    paths: &Paths,
    model: Model,
    condition: Condition,
    only: &[String],
    rounds: u32,
    timeout: u64,
) -> ExitCode {
    let xenith = match paths.ensure_xenith() {
        Ok(path) => path.to_path_buf(),
        Err(message) => {
            eprintln!("{message}");
            return ExitCode::FAILURE;
        }
    };
    let guide = std::fs::read_to_string(&paths.field_guide).unwrap_or_default();
    let api_table = std::fs::read_to_string(&paths.api_table).unwrap_or_default();
    // A docs-family cell run against a missing table would silently measure
    // `query`/`blind` under the wrong label; refuse instead.
    if condition.has_api_table() && api_table.trim().is_empty() {
        eprintln!(
            "{} is empty or missing; the {} condition is meaningless without it",
            paths.api_table.display(),
            condition.name()
        );
        return ExitCode::FAILURE;
    }
    let tasks = match load_tasks(&paths.tasks) {
        Ok(tasks) => tasks,
        Err(message) => {
            eprintln!("{message}");
            return ExitCode::FAILURE;
        }
    };
    let tasks: Vec<&Task> = tasks
        .iter()
        .filter(|t| only.is_empty() || only.contains(&t.name))
        .collect();

    std::fs::create_dir_all(&paths.scratch).ok();
    std::fs::create_dir_all(&paths.results).ok();

    let file = paths
        .results
        .join(format!("{}-{}.json", model.name(), condition.name()));

    // Resume by default: a matrix is accumulated over short bursts, not one
    // long run — model calls are slow and sessions have execution caps. Tasks
    // already recorded in the results file are kept and skipped.
    let mut reports = load_prior_reports(&file);
    if !reports.is_empty() {
        println!(
            "resuming: {} task(s) already recorded in {}",
            reports.len(),
            file.display()
        );
    }
    let done: Vec<String> = reports.iter().map(|r| r.task.clone()).collect();
    let tasks: Vec<&&Task> = tasks.iter().filter(|t| !done.contains(&t.name)).collect();

    for task in &tasks {
        println!("== {} / {} / {}", task.name, model.name(), condition.name());
        let report = run_one_task(
            paths, &xenith, &guide, &api_table, task, model, condition, rounds, timeout,
        );
        let verdict = if report.passed {
            format!("PASS in {} round(s)", report.rounds.len())
        } else {
            "FAIL".to_string()
        };
        println!("   -> {verdict}");
        reports.push(report);
        // Rewrite after every task: a run that dies mid-matrix keeps what it
        // measured instead of losing an hour of model time.
        if let Err(e) = write_results(&file, model, condition, rounds, &reports) {
            eprintln!("{}: {e}", file.display());
            return ExitCode::FAILURE;
        }
    }

    let passed = reports.iter().filter(|r| r.passed).count();
    let pass_at_1 = reports.iter().filter(|r| r.pass_at_1).count();
    println!(
        "\n{}/{} passed ({} at first attempt) -> {}",
        passed,
        reports.len(),
        pass_at_1,
        file.display()
    );
    ExitCode::SUCCESS
}

/// Reconstruct prior task reports from an existing results file, so a burst
/// picks up where the previous one stopped.
fn load_prior_reports(file: &Path) -> Vec<TaskReport> {
    let Ok(text) = std::fs::read_to_string(file) else {
        return Vec::new();
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) else {
        return Vec::new();
    };
    let Some(tasks) = value["tasks"].as_array() else {
        return Vec::new();
    };
    tasks
        .iter()
        .filter_map(|t| {
            Some(TaskReport {
                task: t["task"].as_str()?.to_string(),
                tier: t["tier"].as_u64()? as u32,
                passed: t["passed"].as_bool()?,
                pass_at_1: t["pass_at_1"].as_bool()?,
                rounds: t["rounds"]
                    .as_array()?
                    .iter()
                    .filter_map(|r| {
                        Some(RoundRecord {
                            attempt: r["attempt"].as_u64()? as u32,
                            outcome: r["outcome"].as_str()?.to_string(),
                            seconds: r["seconds"].as_f64()?,
                            // Absent in every pre-0007 report; absence is not
                            // an error, it is the common case. Same for the
                            // 0009 fields below.
                            goals: r["goals"].as_str().map(str::to_string),
                            diag_codes: r["diag_codes"].as_array().map(|codes| {
                                codes
                                    .iter()
                                    .filter_map(|c| c.as_str().map(str::to_string))
                                    .collect()
                            }),
                            feedback_text: r["feedback_text"].as_str().map(str::to_string),
                        })
                    })
                    .collect(),
            })
        })
        .collect()
}

fn write_results(
    file: &Path,
    model: Model,
    condition: Condition,
    rounds: u32,
    reports: &[TaskReport],
) -> Result<(), String> {
    let passed = reports.iter().filter(|r| r.passed).count();
    let pass_at_1 = reports.iter().filter(|r| r.pass_at_1).count();
    let unix_time = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let summary = serde_json::json!({
        "model": model.name(),
        "condition": condition.name(),
        "rounds_limit": rounds,
        "unix_time": unix_time,
        "tasks": reports.iter().map(report_json).collect::<Vec<_>>(),
        "totals": {
            "tasks": reports.len(),
            "passed": passed,
            "pass_at_1": pass_at_1,
        },
    });
    std::fs::write(file, serde_json::to_string_pretty(&summary).unwrap()).map_err(|e| e.to_string())
}

struct RoundRecord {
    attempt: u32,
    outcome: String,
    seconds: f64,
    /// The `xenith goals` text fed back this round, verbatim — the raw
    /// material for the oracle-hit-rate analysis (0007 §5-5): did the
    /// candidates name the methods the final solution used? Recorded only in
    /// the query-family separation arms; everywhere else it stays `None` so
    /// pre-0007 result files keep their exact shape.
    goals: Option<String>,
    /// The distinct XN codes in this round's feedback, and that feedback
    /// verbatim — the consumption-oracle raw material (0009 §1b): did the
    /// next attempt adopt an attached signature, and at what transcription
    /// cost? Recorded only in the v3 arms (teach-off rounds too — they are
    /// the control text); `None` everywhere else so earlier result files keep
    /// their exact shape.
    diag_codes: Option<Vec<String>>,
    feedback_text: Option<String>,
}

struct TaskReport {
    task: String,
    tier: u32,
    passed: bool,
    pass_at_1: bool,
    rounds: Vec<RoundRecord>,
}

fn report_json(report: &TaskReport) -> serde_json::Value {
    serde_json::json!({
        "task": report.task,
        "tier": report.tier,
        "passed": report.passed,
        "pass_at_1": report.pass_at_1,
        "rounds": report.rounds.iter().map(round_json).collect::<Vec<_>>(),
    })
}

fn round_json(round: &RoundRecord) -> serde_json::Value {
    let mut json = serde_json::json!({
        "attempt": round.attempt,
        "outcome": round.outcome,
        "seconds": (round.seconds * 10.0).round() / 10.0,
    });
    // Serialized only when present: rounds outside the experiment arms must
    // not grow null fields that old readers and old diffs never had.
    if let Some(goals) = &round.goals {
        json["goals"] = serde_json::json!(goals);
    }
    if let Some(codes) = &round.diag_codes {
        json["diag_codes"] = serde_json::json!(codes);
    }
    if let Some(text) = &round.feedback_text {
        json["feedback_text"] = serde_json::json!(text);
    }
    json
}

#[allow(clippy::too_many_arguments)]
fn run_one_task(
    paths: &Paths,
    xenith: &Path,
    guide: &str,
    api_table: &str,
    task: &Task,
    model: Model,
    condition: Condition,
    rounds: u32,
    timeout: u64,
) -> TaskReport {
    let mut report = TaskReport {
        task: task.name.clone(),
        tier: task.tier,
        passed: false,
        pass_at_1: false,
        rounds: Vec::new(),
    };

    let mut transcript = first_prompt(guide, api_table, task, condition);

    for attempt in 1..=rounds {
        let started = Instant::now();
        let reply = match ask_model(paths, model, &transcript, timeout) {
            Ok(reply) => reply,
            Err(message) => {
                report.rounds.push(RoundRecord {
                    attempt,
                    outcome: format!("model error: {message}"),
                    seconds: started.elapsed().as_secs_f64(),
                    goals: None,
                    diag_codes: None,
                    feedback_text: None,
                });
                return report;
            }
        };
        // An empty reply is a CLI failure, not a program. Judging it as one
        // poisons the cell twice over: the empty file passes `check` (an empty
        // module is legal), fails `run` with "no main", and gets recorded as a
        // runtime failure — and the repair prompt then quotes an empty
        // "previous attempt" the model never wrote. agy in headless mode does
        // exactly this when a tool request is auto-denied.
        if reply.trim().is_empty() {
            report.rounds.push(RoundRecord {
                attempt,
                outcome: "empty reply".into(),
                seconds: started.elapsed().as_secs_f64(),
                goals: None,
                diag_codes: None,
                feedback_text: None,
            });
            transcript.push_str(
                "\n\n--- note ---\nYour previous reply came back empty (the CLI produced no \
                 text). Do not use tools; answer directly. Reply with exactly one fenced \
                 code block containing the complete program.",
            );
            continue;
        }
        let code = extract_code(&reply);
        let file = paths.scratch.join(format!(
            "{}-{}-{}-r{attempt}.xn",
            task.name,
            model.name(),
            condition.name()
        ));
        if std::fs::write(&file, &code).is_err() {
            report.rounds.push(RoundRecord {
                attempt,
                outcome: "scratch write failed".into(),
                seconds: started.elapsed().as_secs_f64(),
                goals: None,
                diag_codes: None,
                feedback_text: None,
            });
            return report;
        }

        let judgement = judge(xenith, &file, task, condition);
        let done = judgement.feedback.is_none();
        report.rounds.push(RoundRecord {
            attempt,
            outcome: judgement.outcome,
            seconds: started.elapsed().as_secs_f64(),
            goals: judgement.goals,
            diag_codes: judgement.diag_codes,
            feedback_text: judgement.feedback_text,
        });

        if done {
            report.passed = true;
            report.pass_at_1 = attempt == 1;
            return report;
        }

        // Stateless CLIs: each round resends the accumulated exchange.
        transcript.push_str(&format!(
            "\n\n--- your previous attempt ---\n```xenith\n{code}\n```\n\n\
             --- compiler feedback ---\n{}\n\n\
             Fix the program. Reply again with exactly one fenced code block \
             containing the complete corrected file.",
            judgement.feedback.unwrap_or_default()
        ));
    }

    report
}

struct Judgement {
    outcome: String,
    /// `None` means the attempt passed.
    feedback: Option<String>,
    /// The goals text appended to `feedback`, when this round both failed
    /// check/run and belongs to a query-family separation arm (0007 §5-5).
    goals: Option<String>,
    /// v3 only (0009 §1b): the distinct XN codes in this round's feedback.
    diag_codes: Option<Vec<String>>,
    /// v3 only: the feedback verbatim — in teach-off arms this is the control
    /// text the consumption oracle compares against.
    feedback_text: Option<String>,
}

/// Judge one attempt. Goals enrichment happens only on failed check/run —
/// wrong output is not a hole problem — and only for conditions whose
/// feedback channel includes it.
fn judge(xenith: &Path, file: &Path, task: &Task, condition: Condition) -> Judgement {
    let (outcome, mut feedback, enrich) = match execute(xenith, file, condition.teaching()) {
        Execution::Passed { stdout } if stdout == task.expected_stdout => {
            return Judgement {
                outcome: "pass".to_string(),
                feedback: None,
                goals: None,
                diag_codes: None,
                feedback_text: None,
            };
        }
        Execution::Passed { stdout } => (
            "wrong output".to_string(),
            format!(
                "The program compiled and ran, but printed {stdout:?} where {:?} was required.",
                task.expected_stdout
            ),
            false,
        ),
        Execution::CheckFailed { output } => ("diagnostics".to_string(), output, true),
        Execution::RunFailed { exit, error } => (
            "runtime failure".to_string(),
            format!("The program exited with code {exit}: {error}"),
            true,
        ),
    };

    let mut goals = None;
    if enrich && condition.feeds_goals() {
        if let Some(text) = goals_output(xenith, file) {
            feedback.push_str("\n--- xenith goals ---\n");
            feedback.push_str(&text);
            // Kept for oracle-hit-rate only in the separation arms, so
            // hole-guided result files keep their pre-0007 byte shape.
            if matches!(condition, Condition::Query | Condition::DocsQuery) {
                goals = Some(text);
            }
        }
    }

    // The consumption oracle needs the round's codes and the exact text the
    // model saw, in every v3 arm — an adoption claim without the off-arm
    // control text would be indistinguishable from ordinary repair habit.
    let (diag_codes, feedback_text) = if condition.records_feedback() {
        (Some(distinct_xn_codes(&feedback)), Some(feedback.clone()))
    } else {
        (None, None)
    };

    Judgement {
        outcome,
        feedback: Some(feedback),
        goals,
        diag_codes,
        feedback_text,
    }
}

/// Distinct `XN`-prefixed diagnostic codes in first-appearance order. A code
/// is `XN` plus digits, not embedded in a longer identifier.
fn distinct_xn_codes(text: &str) -> Vec<String> {
    let bytes = text.as_bytes();
    let mut codes: Vec<String> = Vec::new();
    let mut i = 0;
    while i + 2 < bytes.len() {
        let boundary = i == 0 || !bytes[i - 1].is_ascii_alphanumeric();
        if boundary && bytes[i] == b'X' && bytes[i + 1] == b'N' && bytes[i + 2].is_ascii_digit() {
            let mut end = i + 2;
            while end < bytes.len() && bytes[end].is_ascii_digit() {
                end += 1;
            }
            let code = &text[i..end];
            if !codes.iter().any(|c| c == code) {
                codes.push(code.to_string());
            }
            i = end;
        } else {
            i += 1;
        }
    }
    codes
}

fn goals_output(xenith: &Path, file: &Path) -> Option<String> {
    let output = std::process::Command::new(xenith)
        .arg("goals")
        .arg(file)
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if text.is_empty() { None } else { Some(text) }
}

// ------------------------------------------------------------------- prompts

// The "do not use tools" sentence is load-bearing: some CLIs (agy) respond to
// compiler feedback by reaching for shell tools, which headless mode denies,
// and the reply comes back empty. The instruction is uniform across models so
// cells stay comparable.
const CONTRACT: &str = "Reply with exactly one fenced code block containing one complete \
Xenith source file, and no prose outside the fence. Answer directly from what you know: do \
not use tools or execute commands — the harness compiles and runs your code for you. The \
program must define `fn main` and print exactly the required output using io.write. io.write \
adds no newline.";

// The separation arms share one skeleton; these two sentences are the only
// permitted difference in it, and the delta between them is the description
// of the feedback channel — nothing behavioral (0007 §5-1: no asymmetric
// nudges, no warnings against anything).
const FEEDBACK_PLAIN: &str = "After each attempt you will receive compiler feedback.";
const FEEDBACK_WITH_GOALS: &str = "After each attempt you will receive compiler feedback; it \
may include hole goals and producer listings.";

// v2 of the experiment (0008 §3). The pilot showed the query channel never
// fires when nobody writes a hole — 2 of 50 failure rounds carried goals
// output. So the hole permission is part of the SHARED skeleton, identical
// in every arm: symmetric by construction, it cannot be a nudge toward
// either factor. The arms still differ only along docs × query.
const HOLE_PERMISSION: &str = "If you cannot determine some expression, write `??` there \
instead of guessing — the program still compiles with holes in it.";

fn first_prompt(guide: &str, api_table: &str, task: &Task, condition: Condition) -> String {
    match condition {
        Condition::Bare => format!(
            "You are writing Xenith, a programming language that resembles Rust and \
             TypeScript. You have no documentation for it; do your best from the name alone.\n\n\
             TASK: {}\n\n{CONTRACT}",
            task.prompt.trim()
        ),
        Condition::FullPack => format!(
            "{guide}\n\n---\n\nTASK: {}\n\n{CONTRACT}",
            task.prompt.trim()
        ),
        Condition::HoleGuided => format!(
            "{guide}\n\n---\n\n\
             You will get compiler feedback after every attempt, including `xenith goals` \
             output for any holes. If part of the program is uncertain, deliberately leave a \
             typed hole (`??name`) there and refine over rounds — the compiler will tell you \
             the required type, the scope, and candidate expressions.\n\n\
             TASK: {}\n\n{CONTRACT}",
            task.prompt.trim()
        ),
        // The 2×2: the guide either carries the API table or not, and the
        // feedback sentence either names the goals channel or not. Same
        // skeleton, same contract, same budgets — everything else must stay
        // byte-identical across the four arms.
        Condition::Docs | Condition::Query | Condition::DocsQuery | Condition::Blind => {
            let guide = if condition.has_api_table() {
                format!("{guide}\n\n## std API reference\n\n{api_table}")
            } else {
                guide.to_string()
            };
            let feedback = if condition.feeds_goals() {
                FEEDBACK_WITH_GOALS
            } else {
                FEEDBACK_PLAIN
            };
            format!(
                "{guide}\n\n---\n\n{HOLE_PERMISSION}\n\n{feedback}\n\nTASK: {}\n\n{CONTRACT}",
                task.prompt.trim()
            )
        }
        // v3 (0009 §4): docs × diagnostic teaching. Teaching exists only in
        // post-failure compiler output, so it must be invisible here — the
        // four arms produce exactly two prompt texts (with and without the
        // API table), asserted byte-for-byte in tests. The hole permission
        // stays, unchanged and shared (v2 showed it moves nothing), and the
        // feedback sentence never names a channel.
        Condition::V3Plain | Condition::V3Teach | Condition::V3Docs | Condition::V3DocsTeach => {
            let guide = if condition.has_api_table() {
                format!("{guide}\n\n## std API reference\n\n{api_table}")
            } else {
                guide.to_string()
            };
            format!(
                "{guide}\n\n---\n\n{HOLE_PERMISSION}\n\n{FEEDBACK_PLAIN}\n\nTASK: {}\n\n{CONTRACT}",
                task.prompt.trim()
            )
        }
    }
}

fn ask_model(paths: &Paths, model: Model, prompt: &str, timeout: u64) -> Result<String, String> {
    // Per-model file: runs for different models execute in parallel, and a
    // shared prompt file would race.
    let prompt_file = paths.scratch.join(format!("prompt-{}.txt", model.name()));
    std::fs::write(&prompt_file, prompt).map_err(|e| e.to_string())?;

    let mut child = std::process::Command::new("pwsh")
        .args([
            "-NoProfile",
            "-File",
            &paths.invoke.display().to_string(),
            "-Cli",
            model.name(),
            "-PromptFile",
            &prompt_file.display().to_string(),
        ])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .stdin(std::process::Stdio::null())
        .spawn()
        .map_err(|e| format!("pwsh: {e}"))?;

    let started = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) => {
                if started.elapsed().as_secs() > timeout {
                    let _ = child.kill();
                    return Err(format!("no reply within {timeout}s"));
                }
                std::thread::sleep(std::time::Duration::from_millis(500));
            }
            Err(e) => return Err(e.to_string()),
        }
    }
    let output = child.wait_with_output().map_err(|e| e.to_string())?;
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

/// The last fenced block in the reply — repair rounds tend to quote earlier
/// code first. No fence at all: assume the whole reply is code.
fn extract_code(reply: &str) -> String {
    let mut blocks = Vec::new();
    let mut current: Option<Vec<&str>> = None;
    for line in reply.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("```") {
            match current.take() {
                Some(block) => blocks.push(block.join("\n")),
                None => current = Some(Vec::new()),
            }
            continue;
        }
        if let Some(block) = &mut current {
            block.push(line);
        }
    }
    match blocks.last() {
        Some(block) => block.clone(),
        None => reply.trim().to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_last_fence_wins() {
        let reply = "Here is the old code:\n```xenith\nold\n```\nFixed:\n```xenith\nnew\n```\n";
        assert_eq!(extract_code(reply), "new");
    }

    #[test]
    fn an_unfenced_reply_is_taken_whole() {
        assert_eq!(extract_code("fn main() {}\n"), "fn main() {}");
    }

    #[test]
    fn fence_language_tags_are_ignored() {
        let reply = "```rust\nfn main() {}\n```";
        assert_eq!(extract_code(reply), "fn main() {}");
    }

    #[test]
    fn indented_fences_still_close() {
        let reply = "  ```\ncode line\n  ```";
        assert_eq!(extract_code(reply), "code line");
    }

    fn sample_task() -> Task {
        Task {
            name: "t4-xx".into(),
            tier: 4,
            prompt: "Print the answer.".into(),
            expected_stdout: String::new(),
            reference: String::new(),
        }
    }

    /// Stub guide and table: the separation assertions are about what the
    /// skeleton adds. The real field guide is shared verbatim by all four
    /// arms, so it cancels out of every between-arm comparison.
    fn separation_prompt(condition: Condition) -> String {
        first_prompt("GUIDE", "TABLE", &sample_task(), condition)
    }

    #[test]
    fn the_api_table_follows_the_docs_factor() {
        assert!(separation_prompt(Condition::Docs).contains("## std API reference"));
        assert!(separation_prompt(Condition::DocsQuery).contains("## std API reference"));
        assert!(!separation_prompt(Condition::Query).contains("## std API reference"));
        assert!(!separation_prompt(Condition::Blind).contains("## std API reference"));
    }

    #[test]
    fn the_goals_sentence_follows_the_query_factor() {
        let names = "hole goals and producer listings";
        assert!(separation_prompt(Condition::Query).contains(names));
        assert!(separation_prompt(Condition::DocsQuery).contains(names));
        assert!(!separation_prompt(Condition::Docs).contains("hole goals"));
        assert!(!separation_prompt(Condition::Blind).contains("hole goals"));
    }

    #[test]
    fn separation_arms_differ_only_along_the_two_factors() {
        // Swapping one factor's text must turn one arm's prompt into the
        // other's byte for byte; any third difference breaks these.
        assert_eq!(
            separation_prompt(Condition::Blind).replace(FEEDBACK_PLAIN, FEEDBACK_WITH_GOALS),
            separation_prompt(Condition::Query)
        );
        assert_eq!(
            separation_prompt(Condition::Docs).replace(FEEDBACK_PLAIN, FEEDBACK_WITH_GOALS),
            separation_prompt(Condition::DocsQuery)
        );
        assert_eq!(
            separation_prompt(Condition::Blind)
                .replace("GUIDE", "GUIDE\n\n## std API reference\n\nTABLE"),
            separation_prompt(Condition::Docs)
        );
    }

    #[test]
    fn no_separation_arm_is_nudged() {
        // 0008 §3 (v2): the hole permission is deliberately present — but it
        // must be the SAME sentence in every arm. Any asymmetric behavioral
        // wording would re-confound the factors the 2×2 exists to separate.
        for condition in Condition::SEPARATION {
            let prompt = separation_prompt(condition);
            assert!(
                prompt.contains(HOLE_PERMISSION),
                "{} lacks the shared hole permission",
                condition.name()
            );
            assert_eq!(
                prompt.matches("??").count(),
                HOLE_PERMISSION.matches("??").count(),
                "{} mentions holes outside the shared sentence",
                condition.name()
            );
        }
    }

    #[test]
    fn separation_condition_names_match_result_files() {
        let names: Vec<&str> = Condition::SEPARATION.iter().map(|c| c.name()).collect();
        assert_eq!(names, ["docs", "query", "docs-query", "blind"]);
    }

    #[test]
    fn goals_feedback_is_wired_to_the_query_family() {
        assert!(Condition::HoleGuided.feeds_goals());
        assert!(Condition::Query.feeds_goals());
        assert!(Condition::DocsQuery.feeds_goals());
        assert!(!Condition::Bare.feeds_goals());
        assert!(!Condition::FullPack.feeds_goals());
        assert!(!Condition::Docs.feeds_goals());
        assert!(!Condition::Blind.feeds_goals());

        assert!(Condition::Docs.has_api_table());
        assert!(Condition::DocsQuery.has_api_table());
        assert!(!Condition::Query.has_api_table());
        assert!(!Condition::Blind.has_api_table());
        assert!(!Condition::FullPack.has_api_table());
    }

    #[test]
    fn goals_survive_the_results_round_trip() {
        let dir = std::env::temp_dir().join(format!("xenith-bench-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("codex-query.json");
        let written = vec![TaskReport {
            task: "t4-01".into(),
            tier: 4,
            passed: true,
            pass_at_1: false,
            rounds: vec![
                RoundRecord {
                    attempt: 1,
                    outcome: "diagnostics".into(),
                    seconds: 1.5,
                    goals: Some("?? : Int — candidates: len, get".into()),
                    diag_codes: None,
                    feedback_text: None,
                },
                RoundRecord {
                    attempt: 2,
                    outcome: "pass".into(),
                    seconds: 2.0,
                    goals: None,
                    diag_codes: None,
                    feedback_text: None,
                },
            ],
        }];
        write_results(&file, Model::Codex, Condition::Query, 4, &written).unwrap();
        let loaded = load_prior_reports(&file);
        std::fs::remove_file(&file).ok();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].rounds.len(), 2);
        assert_eq!(
            loaded[0].rounds[0].goals.as_deref(),
            Some("?? : Int — candidates: len, get")
        );
        assert!(loaded[0].rounds[1].goals.is_none());
    }

    #[test]
    fn pre_0007_reports_still_load() {
        // A results file written before the goals field existed must load
        // exactly as before; resume and summarize both depend on it.
        let dir = std::env::temp_dir().join(format!("xenith-bench-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("legacy-hole-guided.json");
        std::fs::write(
            &file,
            r#"{"tasks":[{"task":"t1","tier":1,"passed":true,"pass_at_1":true,
               "rounds":[{"attempt":1,"outcome":"pass","seconds":3.0}]}]}"#,
        )
        .unwrap();
        let loaded = load_prior_reports(&file);
        std::fs::remove_file(&file).ok();
        assert_eq!(loaded.len(), 1);
        assert!(loaded[0].passed);
        assert!(loaded[0].rounds[0].goals.is_none());
        assert!(loaded[0].rounds[0].diag_codes.is_none());
        assert!(loaded[0].rounds[0].feedback_text.is_none());
    }

    #[test]
    fn v3_prompts_are_byte_identical_across_the_teaching_factor() {
        // 0009 §4: teaching lives in post-failure compiler output only. If
        // either pair diverges, the experiment is measuring prompts, not
        // diagnostics.
        assert_eq!(
            separation_prompt(Condition::V3Plain),
            separation_prompt(Condition::V3Teach)
        );
        assert_eq!(
            separation_prompt(Condition::V3Docs),
            separation_prompt(Condition::V3DocsTeach)
        );
    }

    #[test]
    fn the_v3_docs_factor_matches_the_original_docs_factor() {
        assert!(separation_prompt(Condition::V3Docs).contains("## std API reference"));
        assert!(separation_prompt(Condition::V3DocsTeach).contains("## std API reference"));
        assert!(!separation_prompt(Condition::V3Plain).contains("## std API reference"));
        assert!(!separation_prompt(Condition::V3Teach).contains("## std API reference"));
    }

    #[test]
    fn v3_prompts_never_name_a_feedback_channel() {
        // No goals invitation, no producers, no mention of teaching: the
        // model must not be told there is anything special to look for in
        // the diagnostics (0009 §4 — the v2 "query" wording confound).
        for condition in Condition::V3 {
            let prompt = separation_prompt(condition);
            assert!(!prompt.contains("goals"), "{}", condition.name());
            assert!(!prompt.contains("producer"), "{}", condition.name());
            assert!(!prompt.contains("teach"), "{}", condition.name());
            assert!(
                prompt.contains(HOLE_PERMISSION),
                "{} lacks the shared hole permission",
                condition.name()
            );
            assert!(
                prompt.contains(FEEDBACK_PLAIN),
                "{} lacks the shared feedback sentence",
                condition.name()
            );
        }
    }

    #[test]
    fn v3_condition_names_match_result_files() {
        let names: Vec<&str> = Condition::V3.iter().map(|c| c.name()).collect();
        assert_eq!(names, ["v3-plain", "v3-teach", "v3-docs", "v3-docs-teach"]);
        // The clap-facing value name is the result-file suffix; it must agree
        // with `name()` for every condition, not just the pinned v3 ones.
        for condition in Condition::ALL
            .into_iter()
            .chain(Condition::SEPARATION)
            .chain(Condition::V3)
        {
            let value = condition.to_possible_value().expect("hidden variant");
            assert_eq!(value.get_name(), condition.name());
        }
    }

    #[test]
    fn teaching_is_off_only_in_the_v3_off_arms() {
        assert!(!Condition::V3Plain.teaching());
        assert!(!Condition::V3Docs.teaching());
        assert!(Condition::V3Teach.teaching());
        assert!(Condition::V3DocsTeach.teaching());
        // Legacy arms predate the flag and keep the compiler default, so
        // re-running one measures what its result file says it measured.
        for condition in Condition::ALL.into_iter().chain(Condition::SEPARATION) {
            assert!(condition.teaching(), "{}", condition.name());
            assert!(!condition.records_feedback(), "{}", condition.name());
        }
        // And no v3 arm feeds goals or skips recording — teaching must be
        // the only feedback-channel difference inside the quartet.
        for condition in Condition::V3 {
            assert!(!condition.feeds_goals(), "{}", condition.name());
            assert!(condition.records_feedback(), "{}", condition.name());
        }
        assert!(Condition::V3Docs.has_api_table());
        assert!(Condition::V3DocsTeach.has_api_table());
        assert!(!Condition::V3Plain.has_api_table());
        assert!(!Condition::V3Teach.has_api_table());
    }

    #[test]
    fn the_teaching_flag_reaches_check_and_run() {
        let xenith = Path::new("xenith");
        let file = Path::new("t.xn");
        for subcommand in ["check", "run"] {
            let off: Vec<String> = xenith_cmd(xenith, subcommand, false, file)
                .get_args()
                .map(|a| a.to_string_lossy().into_owned())
                .collect();
            assert_eq!(off, [subcommand, TEACHING_OFF, "t.xn"]);
            let on: Vec<String> = xenith_cmd(xenith, subcommand, true, file)
                .get_args()
                .map(|a| a.to_string_lossy().into_owned())
                .collect();
            assert_eq!(on, [subcommand, "t.xn"]);
        }
    }

    #[test]
    fn xn_codes_are_distinct_and_in_first_appearance_order() {
        let text = "error[XN2003]: unknown method `shove`\nnote: XN2003 again\n\
                    error[XN3008]: bad call\nFOOXN9999 is an identifier, XN12 is a code";
        assert_eq!(distinct_xn_codes(text), ["XN2003", "XN3008", "XN12"]);
        assert!(distinct_xn_codes("no codes here").is_empty());
        assert!(distinct_xn_codes("XNX XN XN-3").is_empty());
    }

    #[test]
    fn v3_feedback_fields_survive_the_results_round_trip() {
        let dir = std::env::temp_dir().join(format!("xenith-bench-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("codex-v3-teach.json");
        let written = vec![TaskReport {
            task: "t4-02".into(),
            tier: 4,
            passed: true,
            pass_at_1: false,
            rounds: vec![
                RoundRecord {
                    attempt: 1,
                    outcome: "diagnostics".into(),
                    seconds: 1.5,
                    goals: None,
                    diag_codes: Some(vec!["XN2003".into(), "XN3008".into()]),
                    feedback_text: Some(
                        "error[XN2003]: unknown method `shove`\n\
                         teaches: push(item: T) -> Unit"
                            .into(),
                    ),
                },
                RoundRecord {
                    attempt: 2,
                    outcome: "pass".into(),
                    seconds: 2.0,
                    goals: None,
                    diag_codes: None,
                    feedback_text: None,
                },
            ],
        }];
        write_results(&file, Model::Codex, Condition::V3Teach, 4, &written).unwrap();
        let loaded = load_prior_reports(&file);
        std::fs::remove_file(&file).ok();
        assert_eq!(loaded.len(), 1);
        let rounds = &loaded[0].rounds;
        assert_eq!(rounds.len(), 2);
        assert_eq!(
            rounds[0].diag_codes.clone().unwrap(),
            vec!["XN2003", "XN3008"]
        );
        assert!(
            rounds[0]
                .feedback_text
                .as_deref()
                .unwrap()
                .contains("teaches:")
        );
        assert!(rounds[1].diag_codes.is_none());
        assert!(rounds[1].feedback_text.is_none());
    }
}
