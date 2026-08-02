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
}

#[derive(Clone, Copy, ValueEnum)]
enum Model {
    Codex,
    Grok,
    Agy,
    Opencode,
}

impl Model {
    fn name(self) -> &'static str {
        match self {
            Model::Codex => "codex",
            Model::Grok => "grok",
            Model::Agy => "agy",
            Model::Opencode => "opencode",
        }
    }
}

#[derive(Clone, Copy, ValueEnum, PartialEq)]
enum Condition {
    Bare,
    FullPack,
    HoleGuided,
}

impl Condition {
    fn name(self) -> &'static str {
        match self {
            Condition::Bare => "bare",
            Condition::FullPack => "full-pack",
            Condition::HoleGuided => "hole-guided",
        }
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
    }
}

// -------------------------------------------------------------------- layout

struct Paths {
    root: PathBuf,
    xenith: PathBuf,
    tasks: PathBuf,
    scratch: PathBuf,
    results: PathBuf,
    field_guide: PathBuf,
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
        match execute(&xenith, &file) {
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

/// `xenith check` then `xenith run`, mirroring what a model's attempt faces.
fn execute(xenith: &Path, file: &Path) -> Execution {
    let check = std::process::Command::new(xenith)
        .arg("check")
        .arg(file)
        .output();
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

    let run = std::process::Command::new(xenith)
        .arg("run")
        .arg(file)
        .output();
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

    let mut reports = Vec::new();
    for task in &tasks {
        println!("== {} / {} / {}", task.name, model.name(), condition.name());
        let report = run_one_task(
            paths, &xenith, &guide, task, model, condition, rounds, timeout,
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
        "rounds": report
            .rounds
            .iter()
            .map(|r| serde_json::json!({
                "attempt": r.attempt,
                "outcome": r.outcome,
                "seconds": (r.seconds * 10.0).round() / 10.0,
            }))
            .collect::<Vec<_>>(),
    })
}

#[allow(clippy::too_many_arguments)]
fn run_one_task(
    paths: &Paths,
    xenith: &Path,
    guide: &str,
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

    let mut transcript = first_prompt(guide, task, condition);

    for attempt in 1..=rounds {
        let started = Instant::now();
        let reply = match ask_model(paths, model, &transcript, timeout) {
            Ok(reply) => reply,
            Err(message) => {
                report.rounds.push(RoundRecord {
                    attempt,
                    outcome: format!("model error: {message}"),
                    seconds: started.elapsed().as_secs_f64(),
                });
                return report;
            }
        };
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
            });
            return report;
        }

        let (outcome, feedback) = judge(xenith, &file, task, condition);
        let done = feedback.is_none();
        report.rounds.push(RoundRecord {
            attempt,
            outcome,
            seconds: started.elapsed().as_secs_f64(),
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
            feedback.unwrap_or_default()
        ));
    }

    report
}

/// Judge one attempt. `None` feedback means the attempt passed.
fn judge(
    xenith: &Path,
    file: &Path,
    task: &Task,
    condition: Condition,
) -> (String, Option<String>) {
    match execute(xenith, file) {
        Execution::Passed { stdout } if stdout == task.expected_stdout => {
            ("pass".to_string(), None)
        }
        Execution::Passed { stdout } => (
            "wrong output".to_string(),
            Some(format!(
                "The program compiled and ran, but printed {stdout:?} where {:?} was required.",
                task.expected_stdout
            )),
        ),
        Execution::CheckFailed { output } => {
            let mut feedback = output;
            if condition == Condition::HoleGuided {
                if let Some(goals) = goals_output(xenith, file) {
                    feedback.push_str("\n--- xenith goals ---\n");
                    feedback.push_str(&goals);
                }
            }
            ("diagnostics".to_string(), Some(feedback))
        }
        Execution::RunFailed { exit, error } => {
            let mut feedback = format!("The program exited with code {exit}: {error}");
            if condition == Condition::HoleGuided {
                if let Some(goals) = goals_output(xenith, file) {
                    feedback.push_str("\n--- xenith goals ---\n");
                    feedback.push_str(&goals);
                }
            }
            ("runtime failure".to_string(), Some(feedback))
        }
    }
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

const CONTRACT: &str = "Reply with exactly one fenced code block containing one complete \
Xenith source file, and no prose outside the fence. The program must define `fn main` and \
print exactly the required output using io.write. io.write adds no newline.";

fn first_prompt(guide: &str, task: &Task, condition: Condition) -> String {
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
}
