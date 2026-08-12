//! Tier-5 constrained integration (design/0011): frozen project tasks, the
//! api-dump exporter, the six campaign arms, and their verify gates.
//!
//! Tier-5 tasks are whole projects, not single files. A task directory under
//! `bench/ai/tasks-t5/` carries a frozen skeleton (manifest + provided
//! modules, plus — in the t5a family — the frozen `src/main.xn` calling
//! contract), the statement shown to the model, the one target path the
//! model writes, the hidden expected stdout, a reference solution, and a
//! frozen machine-generated `api-dump.txt` for the `api` arms.
//!
//! Three 0011 commitments are enforced here rather than remembered:
//!
//! - **The byte manifest (§3).** `first_prompt` is the only assembler of a
//!   tier-5 round-1 prompt, and its tests pin what each arm may contain:
//!   provided module sources never, the api table only in the docs arms,
//!   the frozen `main.xn` in every t5a arm, teaching in none (it lives in
//!   post-failure compiler output only).
//! - **The golden gate (§7).** `verify` regenerates every frozen dump and
//!   fails on drift, then checks that each provided-module surface the
//!   reference solution consumes appears in the dump — a broken map fails
//!   before any measurement runs on it.
//! - **The frozen run order (§5).** The committed shuffle table is a pure
//!   function of the model, task and arm names; `verify` recomputes it, so
//!   the file cannot be quietly reordered to favour a resume pattern.

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Instant;

use serde::Deserialize;

use crate::{
    Condition, Execution, Model, Paths, RoundRecord, TaskReport, ask_model, distinct_xn_codes,
    execute, extract_code, load_prior_reports, write_results,
};

/// The frozen dump's file name inside a task directory.
pub const DUMP_FILE: &str = "api-dump.txt";
/// The frozen run-order table inside `tasks-t5/`.
pub const SHUFFLE_FILE: &str = "shuffle-order.tsv";
/// The dump hash — re-exported from the shared model so the shuffle table
/// and the dumps keep hashing identically. The dump's version header lives
/// with the model too ([`xenith_driver::api::BENCH_DUMP_VERSION`]); the name
/// is frozen with the dumps (design/0011 §7, shared since design/0013 §2).
pub use xenith_driver::api::fnv1a64;

/// The docs slot of a tier-5 arm: the one factor besides teaching.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum T5Docs {
    /// The full field guide (plus the std api table): the full-pack lineage.
    Guide,
    /// The task's frozen machine-generated dump (plus the std api table).
    Api,
    /// Nothing beyond the shared primer — "primer-only, exactly zero more".
    None,
}

// -------------------------------------------------------------------- tasks

#[derive(Deserialize)]
struct T5TaskFile {
    name: String,
    tier: u32,
    family: String,
    target: String,
    prompt: String,
    expected_stdout: String,
}

pub struct T5Task {
    pub name: String,
    pub tier: u32,
    /// The one file the model writes, relative to the project root with
    /// forward slashes ("src/manifest.xn").
    pub target: String,
    pub prompt: String,
    pub expected_stdout: String,
    pub skeleton: PathBuf,
    pub solution: String,
    /// The frozen calling contract — present exactly in the t5a grafts,
    /// whose skeleton ships `src/main.xn`; `None` in the t5b wiring tasks,
    /// where writing it is the task.
    pub frozen_main: Option<String>,
    /// The frozen api-dump, LF-normalised. Empty only before first freeze.
    pub api_dump: String,
    /// Dotted module paths of the provided modules (never `main`).
    pub provided_modules: Vec<String>,
}

pub fn load_t5_tasks(dir: &Path) -> Result<Vec<T5Task>, String> {
    let entries = std::fs::read_dir(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let mut dirs: Vec<PathBuf> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.is_dir())
        .collect();
    dirs.sort();

    let mut tasks = Vec::new();
    for task_dir in dirs {
        let toml_path = task_dir.join("task.toml");
        if !toml_path.is_file() {
            continue;
        }
        let text = read_file(&toml_path)?;
        let file: T5TaskFile =
            toml::from_str(&text).map_err(|e| format!("{}: {e}", toml_path.display()))?;
        let graft = match file.family.as_str() {
            "t5a" => true,
            "t5b" => false,
            other => {
                return Err(format!(
                    "{}: unknown family `{other}` (expected t5a or t5b)",
                    toml_path.display()
                ));
            }
        };
        let skeleton = task_dir.join("skeleton");
        let main_path = skeleton.join("src").join("main.xn");
        // The family split is structural (0011 §1), so a task that violates
        // it is a broken task, not a variant.
        if graft && file.target == "src/main.xn" {
            return Err(format!("{}: a t5a task cannot target main.xn", file.name));
        }
        if !graft && file.target != "src/main.xn" {
            return Err(format!("{}: a t5b task must target src/main.xn", file.name));
        }
        if !graft && main_path.exists() {
            return Err(format!(
                "{}: a t5b skeleton must not ship a main.xn — writing it is the task",
                file.name
            ));
        }
        let frozen_main = if graft {
            Some(read_file(&main_path)?)
        } else {
            None
        };
        let solution = read_file(&task_dir.join("solution.xn"))?;
        // Missing only in the moment between authoring a task and freezing
        // its dump; verify treats empty as broken.
        let api_dump = std::fs::read_to_string(task_dir.join(DUMP_FILE))
            .map(|text| normalise(&text))
            .unwrap_or_default();
        let provided_modules = provided_modules(&skeleton)?;

        tasks.push(T5Task {
            name: file.name,
            tier: file.tier,
            target: file.target,
            prompt: file.prompt,
            expected_stdout: file.expected_stdout,
            skeleton,
            solution,
            frozen_main,
            api_dump,
            provided_modules,
        });
    }
    if tasks.is_empty() {
        return Err(format!("no tier-5 tasks in {}", dir.display()));
    }
    Ok(tasks)
}

/// The provided modules of a skeleton: everything except the entry module.
fn provided_modules(skeleton: &Path) -> Result<Vec<String>, String> {
    let project = xenith_driver::project::load(skeleton)?;
    Ok(project
        .files
        .iter()
        .map(|file| file.module.clone())
        .filter(|module| module != "main")
        .collect())
}

fn read_file(path: &Path) -> Result<String, String> {
    std::fs::read_to_string(path)
        .map(|text| normalise(&text))
        .map_err(|e| format!("{}: {e}", path.display()))
}

/// Git may check text out with CRLF; every comparison here is over LF.
fn normalise(text: &str) -> String {
    text.replace("\r\n", "\n")
}

// ----------------------------------------------------------------- api-dump

/// The deterministic public-surface dump of a project's provided modules
/// (design/0011 §7): pub fn full signatures, pub struct/enum full
/// definitions, effect sets — rendered from the compiler's own model of the
/// surface, never hand-written. Since design/0013 §2 this is the shared
/// ApiSurface model's bench renderer; the characterization test below pins
/// every frozen dump to a byte-identical regeneration through it. The entry
/// module `main` is excluded by the renderer: it is the calling contract,
/// not a provided library surface.
pub fn api_dump(root: &Path) -> Result<String, String> {
    let project = xenith_driver::project::load(root)?;
    let surface = xenith_driver::api::surface(&project)?;
    Ok(xenith_driver::api::render_bench_dump(&surface))
}

pub fn api_dump_command(project: &Path, out: Option<&Path>) -> ExitCode {
    match api_dump(project) {
        Ok(dump) => {
            if let Some(path) = out {
                if let Err(e) = std::fs::write(path, dump.as_bytes()) {
                    eprintln!("{}: {e}", path.display());
                    return ExitCode::FAILURE;
                }
                println!("wrote {}", path.display());
            } else {
                print!("{dump}");
            }
            ExitCode::SUCCESS
        }
        Err(message) => {
            eprintln!("{message}");
            ExitCode::FAILURE
        }
    }
}

// ------------------------------------------------------------------- verify

/// Verify every tier-5 task: reference through the real pipe, frozen dump
/// against a regeneration, and the golden gate — every provided surface the
/// reference consumes must appear in the dump (design/0011 §7). Returns
/// (ok, broken) so the caller can fold the counts into the one verdict.
pub fn verify_all(paths: &Paths, xenith: &Path) -> (usize, usize) {
    let tasks = match load_t5_tasks(&paths.tasks_t5) {
        Ok(tasks) => tasks,
        Err(message) => {
            eprintln!("BROKEN   tier-5 tasks: {message}");
            return (0, 1);
        }
    };
    let mut ok = 0;
    let mut broken = 0;
    for task in &tasks {
        match verify_one(paths, xenith, task) {
            Ok(()) => {
                println!("ok       {} (tier {}, project)", task.name, task.tier);
                ok += 1;
            }
            Err(message) => {
                eprintln!("BROKEN   {}: {message}", task.name);
                broken += 1;
            }
        }
    }
    // The run order is data the runner follows, so it is verified like any
    // other frozen artifact: recomputed, not trusted.
    match std::fs::read_to_string(paths.tasks_t5.join(SHUFFLE_FILE)) {
        Ok(table) => {
            let names: Vec<String> = tasks.iter().map(|t| t.name.clone()).collect();
            if normalise(&table) != canonical_shuffle_table(&names) {
                eprintln!(
                    "BROKEN   {SHUFFLE_FILE}: does not match the canonical order — regenerate \
                     with `xenith-bench t5-shuffle`"
                );
                broken += 1;
            }
        }
        Err(e) => {
            eprintln!("BROKEN   {SHUFFLE_FILE}: {e}");
            broken += 1;
        }
    }
    (ok, broken)
}

fn verify_one(paths: &Paths, xenith: &Path, task: &T5Task) -> Result<(), String> {
    // 1. The reference solution, through exactly the pipe a model faces.
    let work = paths.scratch.join(format!("t5-ref-{}", task.name));
    assemble(&task.skeleton, &work, &task.target, &task.solution)?;
    match execute(xenith, &target_path(&work, &task.target), true) {
        Execution::Passed { stdout } if stdout == task.expected_stdout => {}
        Execution::Passed { stdout } => {
            return Err(format!(
                "reference printed {stdout:?}, expected {:?}",
                task.expected_stdout
            ));
        }
        Execution::CheckFailed { output } => {
            return Err(format!("reference does not check\n{output}"));
        }
        Execution::RunFailed { exit, error } => {
            return Err(format!("reference exited {exit}: {error}"));
        }
    }

    // 2. The frozen dump must be the regenerated dump, byte for byte —
    //    hand edits and generator drift both fail here.
    let regenerated = api_dump(&task.skeleton)?;
    if task.api_dump != regenerated {
        return Err(format!(
            "{DUMP_FILE} does not match a regenerated dump — \
             regenerate with `xenith-bench api-dump`"
        ));
    }

    // 3. The golden gate: every provided-module name the reference consumes
    //    appears in that module's dump section. Running the api arms on a
    //    dump that omits a needed surface would measure a broken map.
    let consumed = consumed_surface(&task.solution, &task.provided_modules);
    if consumed.is_empty() {
        return Err(
            "reference never references a provided module — it must consume the pub API".into(),
        );
    }
    for (module, item) in &consumed {
        let Some(section) = dump_section(&task.api_dump, module) else {
            return Err(format!("{DUMP_FILE} has no section for module `{module}`"));
        };
        if !contains_word(section, item) {
            return Err(format!(
                "reference consumes `{module}.{item}` but the api-dump does not carry it"
            ));
        }
    }
    Ok(())
}

/// The `(module, item)` pairs a source text consumes from the given modules,
/// judged textually: every `module.ident` occurrence, plus the variant when
/// the reference goes on into `module.Type.Variant`.
pub fn consumed_surface(text: &str, modules: &[String]) -> Vec<(String, String)> {
    let mut pairs: Vec<(String, String)> = Vec::new();
    for module in modules {
        let needle = format!("{module}.");
        let mut from = 0;
        while let Some(found) = text[from..].find(&needle) {
            let at = from + found;
            from = at + needle.len();
            // A word boundary before the module path, or `a.b` inside
            // `game.a.b` would count as a reference to module `a.b`.
            let preceded = text[..at]
                .chars()
                .next_back()
                .is_some_and(|c| c.is_alphanumeric() || c == '_' || c == '.');
            if preceded {
                continue;
            }
            let rest = &text[at + needle.len()..];
            let Some(item) = leading_ident(rest) else {
                continue;
            };
            let mut record = |name: &str| {
                let pair = (module.clone(), name.to_string());
                if !pairs.contains(&pair) {
                    pairs.push(pair);
                }
            };
            record(item);
            // `module.Type.Variant` — the variant is consumed surface too.
            if item.starts_with(|c: char| c.is_ascii_uppercase()) {
                let after = &rest[item.len()..];
                if let Some(tail) = after.strip_prefix('.') {
                    if let Some(variant) = leading_ident(tail) {
                        record(variant);
                    }
                }
            }
        }
    }
    pairs
}

fn leading_ident(text: &str) -> Option<&str> {
    let end = text
        .find(|c: char| !(c.is_alphanumeric() || c == '_'))
        .unwrap_or(text.len());
    if end == 0 { None } else { Some(&text[..end]) }
}

/// The dump text of one module's section: from its `module` line to the
/// next one.
fn dump_section<'a>(dump: &'a str, module: &str) -> Option<&'a str> {
    let header = format!("module {module}\n");
    let start = if let Some(rest) = dump.strip_prefix(header.as_str()) {
        dump.len() - rest.len()
    } else {
        let marker = format!("\nmodule {module}\n");
        dump.find(&marker)? + 1 + header.len()
    };
    let end = dump[start..]
        .find("\nmodule ")
        .map_or(dump.len(), |at| start + at);
    Some(&dump[start..end])
}

fn contains_word(text: &str, word: &str) -> bool {
    let mut from = 0;
    while let Some(found) = text[from..].find(word) {
        let at = from + found;
        from = at + word.len();
        let before_ok = !text[..at]
            .chars()
            .next_back()
            .is_some_and(|c| c.is_alphanumeric() || c == '_');
        let after_ok = !text[at + word.len()..]
            .chars()
            .next()
            .is_some_and(|c| c.is_alphanumeric() || c == '_');
        if before_ok && after_ok {
            return true;
        }
    }
    false
}

// ------------------------------------------------------------ shuffle order

/// The canonical run order: every model × task × arm row exactly once,
/// sorted by the FNV-1a 64 of the row text (ties lexicographic). A pure
/// function of the names, so the committed table can always be re-derived
/// and never quietly reordered (design/0011 §5).
pub fn canonical_shuffle_table(task_names: &[String]) -> String {
    let mut rows: Vec<String> = Vec::new();
    for model in Model::ALL {
        for task in task_names {
            for condition in Condition::T5 {
                rows.push(format!("{}\t{}\t{}", model.name(), task, condition.name()));
            }
        }
    }
    rows.sort_by_cached_key(|row| (fnv1a64(row), row.clone()));
    let mut out = String::from(
        "# Frozen run order for the 0011 tier-5 campaign: model <TAB> task <TAB> arm.\n\
         # Deterministic: rows sorted by FNV-1a 64 of the row text, ties lexicographic.\n\
         # Regenerate with `xenith-bench t5-shuffle`; `verify` fails on any drift.\n",
    );
    for row in rows {
        out.push_str(&row);
        out.push('\n');
    }
    out
}

pub fn shuffle_command(paths: &Paths) -> ExitCode {
    match load_t5_tasks(&paths.tasks_t5) {
        Ok(tasks) => {
            let names: Vec<String> = tasks.iter().map(|t| t.name.clone()).collect();
            print!("{}", canonical_shuffle_table(&names));
            ExitCode::SUCCESS
        }
        Err(message) => {
            eprintln!("{message}");
            ExitCode::FAILURE
        }
    }
}

/// The task order this model × arm cell follows, read from the committed
/// table — the runner follows the data file, not the function that made it.
/// The row lookup goes through `shuffle_arm`, so the 0012 t5v2 pair replays
/// its 0011 counterpart's rows instead of demanding new ones in a frozen
/// file.
fn ordered_task_names(
    table: &str,
    model: Model,
    condition: Condition,
    tasks: &[T5Task],
) -> Result<Vec<String>, String> {
    let mut order = Vec::new();
    for line in table.lines() {
        let line = line.trim_end();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut parts = line.split('\t');
        let (Some(m), Some(t), Some(c)) = (parts.next(), parts.next(), parts.next()) else {
            return Err(format!("{SHUFFLE_FILE}: malformed row `{line}`"));
        };
        if m == model.name() && c == condition.shuffle_arm() {
            order.push(t.to_string());
        }
    }
    for task in tasks {
        let count = order.iter().filter(|name| **name == task.name).count();
        if count != 1 {
            return Err(format!(
                "{SHUFFLE_FILE}: `{}` appears {count} times for {} × {}",
                task.name,
                model.name(),
                condition.name()
            ));
        }
    }
    if order.len() != tasks.len() {
        return Err(format!(
            "{SHUFFLE_FILE}: lists a task that does not exist for {} × {}",
            model.name(),
            condition.name()
        ));
    }
    Ok(order)
}

// ------------------------------------------------------------------ prompts

/// The tier-5 contract: one file, not one program. The tool ban and the
/// no-prose fence rule are carried over verbatim in spirit from the
/// single-file CONTRACT; the harness owns the rest of the project.
pub const T5_CONTRACT: &str = "Reply with exactly one fenced code block containing the \
complete content of the target file, and no prose outside the fence. Answer directly from \
what you know: do not use tools or execute commands — the harness writes your reply to the \
target path inside the existing project, then compiles and runs the whole project. Output \
exactly that one file.";

/// Assemble a tier-5 round-1 prompt. This is the byte manifest of
/// design/0011 §3 in executable form: primer, then the docs slot (guide or
/// api-dump, each with the std api table; nothing in the none arms), then
/// the statement, the target path, the t5a calling contract, the CONTRACT.
/// Teaching does not appear anywhere here — the on/off pairs are
/// byte-identical, asserted in tests.
pub fn first_prompt(
    primer: &str,
    guide: &str,
    api_table: &str,
    task: &T5Task,
    condition: Condition,
) -> String {
    let docs = condition
        .t5_docs()
        .expect("only tier-5 conditions assemble tier-5 prompts");
    let mut prompt = String::new();
    prompt.push_str(primer.trim_end());
    prompt.push_str("\n\n---\n\n");
    match docs {
        T5Docs::Guide => {
            prompt.push_str(guide.trim_end());
            prompt.push_str("\n\n## std API reference\n\n");
            prompt.push_str(api_table.trim_end());
            prompt.push_str("\n\n---\n\n");
        }
        T5Docs::Api => {
            prompt.push_str("## Provided-module API (machine generated)\n\n");
            prompt.push_str(task.api_dump.trim_end());
            prompt.push_str("\n\n## std API reference\n\n");
            prompt.push_str(api_table.trim_end());
            prompt.push_str("\n\n---\n\n");
        }
        T5Docs::None => {}
    }
    prompt.push_str(&format!(
        "TASK: {}\n\nTarget file: {}\n\n",
        task.prompt.trim(),
        task.target
    ));
    if let Some(main) = &task.frozen_main {
        prompt.push_str(&format!(
            "--- the calling contract: `src/main.xn`, frozen, already in the project ---\n\
             ```xenith\n{}\n```\n\n",
            main.trim_end()
        ));
    }
    prompt.push_str(T5_CONTRACT);
    prompt
}

// ---------------------------------------------------------------------- run

/// Drive one model through the tier-5 tasks under one arm, in the committed
/// shuffle order, with the same resume ledger and round cap as every other
/// condition. Round 1 sees zero compiler output (0011 §2: pass@1 purity);
/// each repair round rebuilds the project from a fresh skeleton copy.
#[allow(clippy::too_many_arguments)]
pub fn run_campaign(
    paths: &Paths,
    xenith: &Path,
    guide: &str,
    api_table: &str,
    model: Model,
    condition: Condition,
    only: &[String],
    rounds: u32,
    timeout: u64,
) -> ExitCode {
    let primer = std::fs::read_to_string(&paths.primer).unwrap_or_default();
    if primer.trim().is_empty() {
        eprintln!(
            "{} is empty or missing; every tier-5 arm requires the primer",
            paths.primer.display()
        );
        return ExitCode::FAILURE;
    }
    let docs = condition
        .t5_docs()
        .expect("run_campaign is only called for tier-5 conditions");
    if docs == T5Docs::Guide && guide.trim().is_empty() {
        eprintln!("the field guide is empty or missing; the guide arms are meaningless");
        return ExitCode::FAILURE;
    }

    let tasks = match load_t5_tasks(&paths.tasks_t5) {
        Ok(tasks) => tasks,
        Err(message) => {
            eprintln!("{message}");
            return ExitCode::FAILURE;
        }
    };
    if docs == T5Docs::Api {
        if let Some(task) = tasks.iter().find(|t| t.api_dump.trim().is_empty()) {
            eprintln!(
                "{}/{DUMP_FILE} is empty or missing; the api arms are meaningless without it",
                task.name
            );
            return ExitCode::FAILURE;
        }
    }

    // The committed shuffle table is the run order (0011 §5) — no table, no
    // campaign, because an ad-hoc order is exactly the resume bias the table
    // exists to prevent.
    let table = match std::fs::read_to_string(paths.tasks_t5.join(SHUFFLE_FILE)) {
        Ok(table) => table,
        Err(e) => {
            eprintln!(
                "{}: {e} — the tier-5 campaign runs only in the frozen shuffle order",
                paths.tasks_t5.join(SHUFFLE_FILE).display()
            );
            return ExitCode::FAILURE;
        }
    };
    let order = match ordered_task_names(&table, model, condition, &tasks) {
        Ok(order) => order,
        Err(message) => {
            eprintln!("{message}");
            return ExitCode::FAILURE;
        }
    };

    std::fs::create_dir_all(&paths.scratch).ok();
    std::fs::create_dir_all(&paths.results).ok();
    let file = paths
        .results
        .join(format!("{}-{}.json", model.name(), condition.name()));
    let mut reports = load_prior_reports(&file);
    if !reports.is_empty() {
        println!(
            "resuming: {} task(s) already recorded in {}",
            reports.len(),
            file.display()
        );
    }
    let done: Vec<String> = reports.iter().map(|r| r.task.clone()).collect();

    for name in &order {
        if !only.is_empty() && !only.contains(name) {
            continue;
        }
        if done.contains(name) {
            continue;
        }
        let task = tasks
            .iter()
            .find(|t| &t.name == name)
            .expect("ordered names come from the loaded tasks");
        println!("== {} / {} / {}", task.name, model.name(), condition.name());
        let report = run_one_task(
            paths, xenith, &primer, guide, api_table, task, model, condition, rounds, timeout,
        );
        let verdict = if report.passed {
            format!("PASS in {} round(s)", report.rounds.len())
        } else {
            "FAIL".to_string()
        };
        println!("   -> {verdict}");
        reports.push(report);
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

#[allow(clippy::too_many_arguments)]
fn run_one_task(
    paths: &Paths,
    xenith: &Path,
    primer: &str,
    guide: &str,
    api_table: &str,
    task: &T5Task,
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

    let mut transcript = first_prompt(primer, guide, api_table, task, condition);

    for attempt in 1..=rounds {
        let started = Instant::now();
        let reply = match ask_model(paths, model, &transcript, timeout) {
            Ok(reply) => reply,
            Err(message) => {
                report.rounds.push(RoundRecord::bare(
                    attempt,
                    format!("model error: {message}"),
                    started.elapsed().as_secs_f64(),
                ));
                return report;
            }
        };
        if reply.trim().is_empty() {
            report.rounds.push(RoundRecord::bare(
                attempt,
                "empty reply".into(),
                started.elapsed().as_secs_f64(),
            ));
            transcript.push_str(
                "\n\n--- note ---\nYour previous reply came back empty (the CLI produced no \
                 text). Do not use tools; answer directly. Reply with exactly one fenced \
                 code block containing the complete target file.",
            );
            continue;
        }
        let code = extract_code(&reply);
        let work = paths.scratch.join(format!(
            "t5-{}-{}-{}-r{attempt}",
            task.name,
            model.name(),
            condition.name()
        ));
        // A fresh copy per round: no state leaks between attempts, and the
        // only file the model owns is the one it wrote.
        if let Err(message) = assemble(&task.skeleton, &work, &task.target, &code) {
            report.rounds.push(RoundRecord::bare(
                attempt,
                format!("scratch write failed: {message}"),
                started.elapsed().as_secs_f64(),
            ));
            return report;
        }

        let (outcome, feedback) = match execute(
            xenith,
            &target_path(&work, &task.target),
            condition.teaching(),
        ) {
            Execution::Passed { stdout } if stdout == task.expected_stdout => {
                (String::from("pass"), None)
            }
            Execution::Passed { stdout } => (
                "wrong output".to_string(),
                Some(wrong_output_feedback(&stdout)),
            ),
            Execution::CheckFailed { output } => ("diagnostics".to_string(), Some(output)),
            Execution::RunFailed { exit, error } => (
                "runtime failure".to_string(),
                Some(format!("The program exited with code {exit}: {error}")),
            ),
        };

        // Everything 0011 §6 needs later is recorded now: the submitted file,
        // the codes, the verbatim feedback, and how much machine help — fixes
        // offered, teach lines — that feedback carried.
        let done = feedback.is_none();
        report.rounds.push(RoundRecord {
            attempt,
            outcome,
            seconds: started.elapsed().as_secs_f64(),
            goals: None,
            diag_codes: feedback.as_deref().map(distinct_xn_codes),
            feedback_text: feedback.clone(),
            submitted: Some(code.clone()),
            fix_count: feedback.as_deref().map(count_fix_lines),
            teach_count: feedback.as_deref().map(count_teach_lines),
        });

        if done {
            report.passed = true;
            report.pass_at_1 = attempt == 1;
            return report;
        }

        transcript.push_str(&format!(
            "\n\n--- your previous attempt ({}) ---\n```xenith\n{code}\n```\n\n\
             --- compiler feedback ---\n{}\n\n\
             Fix the file. Reply again with exactly one fenced code block containing the \
             complete corrected content of `{}`.",
            task.target,
            feedback.unwrap_or_default(),
            task.target
        ));
    }

    report
}

/// Machine-fix availability this round: `  fix: ` lines in the rendered
/// feedback. Adoption is a post-hoc judgement over consecutive `submitted`
/// texts, so only availability is counted here.
/// Repair feedback for a program that ran but printed the wrong thing.
/// Takes only the model's own output: the hidden expected stdout is the
/// measurement oracle (0011 §3), and a function that never sees it
/// cannot echo it back as an answer key.
fn wrong_output_feedback(stdout: &str) -> String {
    format!(
        "The program compiled and ran, but printed {stdout:?}, which is not the required \
         output. The required output is not disclosed; derive it from the provided \
         modules' public API."
    )
}

fn count_fix_lines(feedback: &str) -> u64 {
    feedback
        .lines()
        .filter(|line| line.starts_with("  fix: "))
        .count() as u64
}

/// Teach lines this round, across the three teach renderings (call shapes,
/// method catalogues, use candidates).
fn count_teach_lines(feedback: &str) -> u64 {
    feedback
        .lines()
        .filter(|line| {
            line.starts_with("  call shape: ")
                || line.starts_with("  methods of ")
                || line.ends_with("is pub in more than one module:")
        })
        .count() as u64
}

// ------------------------------------------------------------ project setup

/// A fresh working project: skeleton copied whole, the model's (or the
/// reference's) file written at the target path.
fn assemble(skeleton: &Path, work: &Path, target: &str, content: &str) -> Result<(), String> {
    let _ = std::fs::remove_dir_all(work);
    copy_tree(skeleton, work)?;
    let file = target_path(work, target);
    if let Some(parent) = file.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
    }
    std::fs::write(&file, content).map_err(|e| format!("{}: {e}", file.display()))
}

fn target_path(work: &Path, target: &str) -> PathBuf {
    let mut file = work.to_path_buf();
    for part in target.split('/') {
        file.push(part);
    }
    file
}

fn copy_tree(from: &Path, to: &Path) -> Result<(), String> {
    std::fs::create_dir_all(to).map_err(|e| format!("{}: {e}", to.display()))?;
    let entries = std::fs::read_dir(from).map_err(|e| format!("{}: {e}", from.display()))?;
    for entry in entries.flatten() {
        let source = entry.path();
        let dest = to.join(entry.file_name());
        if source.is_dir() {
            copy_tree(&source, &dest)?;
        } else {
            std::fs::copy(&source, &dest).map_err(|e| format!("{}: {e}", source.display()))?;
        }
    }
    Ok(())
}

// ----------------------------------------------------------------- summary

/// The tier-5 additions to the generated summary beyond the matrix block:
/// rounds-to-green distributions per arm, and the 0011 §6 static flag —
/// green cells whose final submission never references a provided module.
pub fn summary_extras(paths: &Paths) -> String {
    let Ok(tasks) = load_t5_tasks(&paths.tasks_t5) else {
        return String::new();
    };

    let mut out = String::from(
        "\nRounds to green, all models pooled — how many green cells closed after\n\
         exactly N rounds (censored = never green within the cap):\n\n\
         | condition | 1 | 2 | 3 | 4+ | censored |\n\
         | --- | --- | --- | --- | --- | --- |\n",
    );
    let mut flagged: Vec<String> = Vec::new();
    for condition in Condition::T5 {
        let mut buckets = [0u32; 4];
        let mut censored = 0u32;
        for model in Model::ALL {
            let file = paths
                .results
                .join(format!("{}-{}.json", model.name(), condition.name()));
            for report in load_prior_reports(&file) {
                if !report.passed {
                    censored += 1;
                    continue;
                }
                let rounds = report.rounds.len().min(4);
                buckets[rounds - 1] += 1;
                // The 0011 §6 flag: a pass whose final file never mentions
                // any provided module is green without integration.
                let Some(task) = tasks.iter().find(|t| t.name == report.task) else {
                    continue;
                };
                let referenced = report
                    .rounds
                    .last()
                    .and_then(|round| round.submitted.as_deref())
                    .is_none_or(|text| !consumed_surface(text, &task.provided_modules).is_empty());
                if !referenced {
                    flagged.push(format!(
                        "`{}` / {} / {}",
                        model.name(),
                        report.task,
                        condition.name()
                    ));
                }
            }
        }
        out.push_str(&format!(
            "| {} | {} | {} | {} | {} | {} |\n",
            condition.name(),
            buckets[0],
            buckets[1],
            buckets[2],
            buckets[3],
            censored
        ));
    }

    out.push_str(
        "\nGreen-but-never-references-a-provided-module (0011 §6, from the final submitted\n\
         file text): ",
    );
    if flagged.is_empty() {
        out.push_str("none observed.\n");
    } else {
        out.push('\n');
        for entry in flagged {
            out.push_str(&format!("- {entry}\n"));
        }
    }
    out
}

// -------------------------------------------------------------------- tests

#[cfg(test)]
mod tests {
    use super::*;

    use xenith_driver::api::BENCH_DUMP_VERSION as DUMP_VERSION;

    #[test]
    fn wrong_output_feedback_carries_own_stdout_and_never_the_oracle() {
        let text = wrong_output_feedback("ada: 7");
        assert!(text.contains("\"ada: 7\""));
        assert!(text.contains("not disclosed"));
        // The signature admits only the model's own stdout — the expected
        // string is not a parameter, so it cannot appear here. This test
        // pins the phrasing so a future edit that re-inlines the old
        // `where {expected:?} was required` format fails loudly.
        assert!(!text.contains("was required"));
    }

    fn sample_task(graft: bool) -> T5Task {
        T5Task {
            name: "t5-xx".into(),
            tier: 5,
            target: if graft {
                "src/manifest.xn".into()
            } else {
                "src/main.xn".into()
            },
            prompt: "Do the thing.".into(),
            expected_stdout: "42".into(),
            skeleton: PathBuf::from("unused"),
            solution: String::new(),
            frozen_main: graft.then(|| "use manifest;\n\nfn main() {}".to_string()),
            api_dump: "# dump\nmodule depot.locker\n\npub fn stow(load: Int) -> Int".into(),
            provided_modules: vec!["depot.locker".into()],
        }
    }

    fn prompt(condition: Condition, graft: bool) -> String {
        first_prompt("PRIMER", "GUIDE", "TABLE", &sample_task(graft), condition)
    }

    #[test]
    fn t5_prompts_are_byte_identical_across_the_teaching_factor() {
        // 0011 §2: teaching lives in post-failure compiler output only. A
        // prompt that reveals the arm would be measuring prompts.
        for graft in [true, false] {
            assert_eq!(
                prompt(Condition::T5GuideOn, graft),
                prompt(Condition::T5GuideOff, graft)
            );
            assert_eq!(
                prompt(Condition::T5ApiOn, graft),
                prompt(Condition::T5ApiOff, graft)
            );
            assert_eq!(
                prompt(Condition::T5NoneOn, graft),
                prompt(Condition::T5NoneOff, graft)
            );
            // The 0012 §2 pair: byte-identical to each other and to the
            // 0011 none pair — "zero prompt changes" is the operational
            // check that keeps t5v2 attributable to the compiler alone.
            assert_eq!(
                prompt(Condition::T5V2NoneOn, graft),
                prompt(Condition::T5V2NoneOff, graft)
            );
            assert_eq!(
                prompt(Condition::T5V2NoneOn, graft),
                prompt(Condition::T5NoneOn, graft)
            );
        }
    }

    #[test]
    fn the_docs_slot_is_the_only_difference_between_arms() {
        // The byte manifest (0011 §3): swapping the docs block maps one
        // arm's prompt onto another byte for byte. Any third difference
        // breaks these equalities.
        let none = prompt(Condition::T5NoneOn, true);
        let guide_block = "GUIDE\n\n## std API reference\n\nTABLE\n\n---\n\n";
        let api_block = "## Provided-module API (machine generated)\n\n\
                         # dump\nmodule depot.locker\n\npub fn stow(load: Int) -> Int\
                         \n\n## std API reference\n\nTABLE\n\n---\n\n";
        assert_eq!(
            none.replace("---\n\nTASK:", &format!("---\n\n{guide_block}TASK:")),
            prompt(Condition::T5GuideOn, true)
        );
        assert_eq!(
            none.replace("---\n\nTASK:", &format!("---\n\n{api_block}TASK:")),
            prompt(Condition::T5ApiOn, true)
        );
    }

    #[test]
    fn the_none_arm_is_primer_only() {
        // "none = nothing beyond the primer" is strict (0011 §3): no guide,
        // no api table, no dump.
        let none = prompt(Condition::T5NoneOn, true);
        assert!(!none.contains("GUIDE"));
        assert!(!none.contains("TABLE"));
        assert!(!none.contains("Provided-module API"));
        assert!(none.starts_with("PRIMER\n\n---\n\nTASK:"));
    }

    #[test]
    fn the_api_table_rides_with_both_docs_arms_and_only_them() {
        assert!(prompt(Condition::T5GuideOn, true).contains("## std API reference"));
        assert!(prompt(Condition::T5ApiOn, true).contains("## std API reference"));
        assert!(!prompt(Condition::T5NoneOn, true).contains("## std API reference"));
        // And the dump is the api arms' alone.
        assert!(prompt(Condition::T5ApiOn, true).contains("module depot.locker"));
        assert!(!prompt(Condition::T5GuideOn, true).contains("module depot.locker"));
    }

    #[test]
    fn the_calling_contract_appears_in_every_t5a_arm_and_no_t5b_arm() {
        for condition in Condition::T5.into_iter().chain(Condition::T5V2) {
            assert!(
                prompt(condition, true).contains("use manifest;"),
                "{} lacks the frozen main",
                condition.name()
            );
            assert!(
                !prompt(condition, false).contains("calling contract"),
                "{} carries a calling contract in t5b",
                condition.name()
            );
        }
    }

    #[test]
    fn every_t5_prompt_ends_with_the_contract_and_names_the_target() {
        for condition in Condition::T5.into_iter().chain(Condition::T5V2) {
            for graft in [true, false] {
                let text = prompt(condition, graft);
                assert!(text.ends_with(T5_CONTRACT), "{}", condition.name());
                assert!(
                    text.contains(&format!("Target file: {}", sample_task(graft).target)),
                    "{}",
                    condition.name()
                );
            }
        }
    }

    #[test]
    fn consumed_surface_reads_calls_types_and_variants() {
        let modules = vec!["depot.locker".to_string(), "mill.rules".to_string()];
        let text = "use depot.locker;\n\
                    let a = depot.locker.arrive(label: \"ax\");\n\
                    let r = match s { depot.locker.Rank.Gold => 1, _ => 0 };\n\
                    mill.rules.keeps(plank: p);";
        let pairs = consumed_surface(text, &modules);
        assert!(pairs.contains(&("depot.locker".into(), "arrive".into())));
        assert!(pairs.contains(&("depot.locker".into(), "Rank".into())));
        assert!(pairs.contains(&("depot.locker".into(), "Gold".into())));
        assert!(pairs.contains(&("mill.rules".into(), "keeps".into())));
        // The bare `use depot.locker;` is a dependency, not a consumed item.
        assert!(!pairs.iter().any(|(_, item)| item == "locker"));
        // And a prefixed occurrence is not a boundary match.
        assert!(consumed_surface("game.mill.rules.keeps(p)", &modules[1..]).is_empty());
    }

    #[test]
    fn word_containment_respects_identifier_boundaries() {
        assert!(contains_word("pub fn stow(load: Int)", "stow"));
        assert!(!contains_word("pub fn stowage(load: Int)", "stow"));
        assert!(!contains_word("pub fn restow()", "stow"));
    }

    #[test]
    fn dump_sections_split_on_module_headers() {
        let dump = "# header\n\nmodule alpha\n\npub fn one()\n\nmodule beta\n\npub fn two()\n";
        assert!(dump_section(dump, "alpha").unwrap().contains("one"));
        assert!(!dump_section(dump, "alpha").unwrap().contains("two"));
        assert!(dump_section(dump, "beta").unwrap().contains("two"));
        assert!(dump_section(dump, "gamma").is_none());
    }

    #[test]
    fn the_shuffle_table_is_deterministic_and_complete() {
        let names: Vec<String> = (1..=6).map(|i| format!("t5-0{i}")).collect();
        let table = canonical_shuffle_table(&names);
        assert_eq!(
            table,
            canonical_shuffle_table(&names),
            "not a pure function"
        );
        let rows: Vec<&str> = table
            .lines()
            .filter(|line| !line.starts_with('#') && !line.is_empty())
            .collect();
        // 7 models × 6 tasks × 6 arms, each exactly once.
        assert_eq!(rows.len(), 252);
        let mut sorted = rows.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(sorted.len(), 252, "a row repeats");
        // And it must not be the nested-loop order it was built in: the
        // whole point is that no model or arm runs as a contiguous block.
        let built: Vec<String> = rows.iter().map(|r| r.to_string()).collect();
        let mut loops = Vec::new();
        for model in Model::ALL {
            for task in &names {
                for condition in Condition::T5 {
                    loops.push(format!("{}\t{}\t{}", model.name(), task, condition.name()));
                }
            }
        }
        assert_ne!(built, loops, "the shuffle did not shuffle");
    }

    #[test]
    fn the_shuffle_order_reader_follows_the_table() {
        let names: Vec<String> = (1..=6).map(|i| format!("t5-0{i}")).collect();
        let table = canonical_shuffle_table(&names);
        let tasks: Vec<T5Task> = names
            .iter()
            .map(|name| {
                let mut task = sample_task(true);
                task.name = name.clone();
                task
            })
            .collect();
        let order = ordered_task_names(&table, Model::Codex, Condition::T5ApiOn, &tasks).unwrap();
        assert_eq!(order.len(), 6);
        let mut sorted = order.clone();
        sorted.sort();
        assert_eq!(sorted, names, "every task exactly once");
        // The 0012 t5v2 pair replays the frozen none rows — same order,
        // no new rows demanded of a frozen table.
        let replayed =
            ordered_task_names(&table, Model::Codex, Condition::T5V2NoneOn, &tasks).unwrap();
        let none = ordered_task_names(&table, Model::Codex, Condition::T5NoneOn, &tasks).unwrap();
        assert_eq!(replayed, none);
        let replayed_off =
            ordered_task_names(&table, Model::Codex, Condition::T5V2NoneOff, &tasks).unwrap();
        let none_off =
            ordered_task_names(&table, Model::Codex, Condition::T5NoneOff, &tasks).unwrap();
        assert_eq!(replayed_off, none_off);
        // A truncated table is refused rather than silently reordered.
        let truncated: String = table
            .lines()
            .take(30)
            .map(|line| format!("{line}\n"))
            .collect();
        assert!(ordered_task_names(&truncated, Model::Codex, Condition::T5ApiOn, &tasks).is_err());
    }

    #[test]
    fn fix_and_teach_lines_are_counted_from_the_rendered_shapes() {
        let feedback = concat!(
            "src/main.xn:3:5: error[XN2002]: unknown name `stow`\n",
            "  call shape: stow(load: Int) -> Int\n",
            "3 | stow(4)\n",
            "  |  ^^^^\n",
            "  fix: insert `use depot.locker;`\n",
            "  run `xenith explain XN2002` for the rule\n",
            "src/main.xn:9:1: error[XN2003]: unknown method\n",
            "  methods of List<Int> (2 of 9):\n",
            "      len() -> Int\n",
            "      get(index: Int) -> Option<Int>\n",
        );
        assert_eq!(count_fix_lines(feedback), 1);
        assert_eq!(count_teach_lines(feedback), 2);
    }

    #[test]
    fn the_frozen_tasks_obey_the_family_split_and_the_byte_manifest() {
        // Over the real frozen artifacts, not fabricated ones: the 0011 §1
        // family split (4× t5a, 2× t5b) and the §3 rule that provided module
        // sources never enter any prompt — the whole file text, verbatim, is
        // the exact thing declared invisible.
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .nth(3)
            .expect("repository root")
            .to_path_buf();
        let tasks = load_t5_tasks(&root.join("bench/ai/tasks-t5")).unwrap();
        assert_eq!(tasks.len(), 6);
        let grafts = tasks.iter().filter(|t| t.frozen_main.is_some()).count();
        assert_eq!(grafts, 4, "0011 §1: four t5a implementation grafts");

        for task in &tasks {
            assert!(!task.provided_modules.is_empty(), "{}", task.name);
            assert!(
                !task.api_dump.is_empty(),
                "{} has no frozen dump",
                task.name
            );
            let project = xenith_driver::project::load(&task.skeleton).unwrap();
            for condition in Condition::T5 {
                let prompt = first_prompt("PRIMER", "GUIDE", "TABLE", task, condition);
                for file in &project.files {
                    if file.module == "main" {
                        continue;
                    }
                    assert!(
                        !prompt.contains(file.source.trim()),
                        "{}: provided module `{}` source leaked into the {} prompt",
                        task.name,
                        file.module,
                        condition.name()
                    );
                }
                match &task.frozen_main {
                    Some(main) => assert!(
                        prompt.contains(main.trim_end()),
                        "{}: the {} prompt lacks the frozen calling contract",
                        task.name,
                        condition.name()
                    ),
                    None => assert!(
                        !prompt.contains("calling contract"),
                        "{}: a t5b prompt claims a calling contract",
                        task.name
                    ),
                }
            }
            // The teaching factor is invisible in round 1, on real data too.
            let pairs = [
                (Condition::T5GuideOn, Condition::T5GuideOff),
                (Condition::T5ApiOn, Condition::T5ApiOff),
                (Condition::T5NoneOn, Condition::T5NoneOff),
            ];
            for (on, off) in pairs {
                assert_eq!(
                    first_prompt("PRIMER", "GUIDE", "TABLE", task, on),
                    first_prompt("PRIMER", "GUIDE", "TABLE", task, off),
                    "{}",
                    task.name
                );
            }
        }
    }

    #[test]
    fn every_frozen_dump_regenerates_byte_identical_through_the_shared_model() {
        // The characterization gate of design/0013 §2: the six frozen
        // bench/ai/tasks-t5/*/api-dump.txt artifacts, regenerated through
        // the shared ApiSurface model, byte for byte. The frozen files are
        // read only — a mismatch is a bug in the model or the renderer,
        // never a reason to touch the artifacts.
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .nth(3)
            .expect("repository root")
            .to_path_buf();
        let tasks = load_t5_tasks(&root.join("bench/ai/tasks-t5")).unwrap();
        assert_eq!(tasks.len(), 6, "the 0011 campaign froze six tasks");
        for task in &tasks {
            assert!(
                !task.api_dump.is_empty(),
                "{} has no frozen dump to characterize",
                task.name
            );
            let regenerated = api_dump(&task.skeleton).unwrap();
            assert_eq!(
                task.api_dump, regenerated,
                "{}: the shared model does not reproduce the frozen dump",
                task.name
            );
        }
    }

    #[test]
    fn api_dump_is_deterministic_and_renders_the_full_surface() {
        let dir = std::env::temp_dir().join(format!("xenith-t5-dump-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src/depot")).unwrap();
        std::fs::write(dir.join("xenith.toml"), "name = \"dump-test\"\n").unwrap();
        std::fs::write(
            dir.join("src/depot/locker.xn"),
            "pub struct Locker {\n    label: String,\n    var weight: Int,\n}\n\n\
             fn hidden() -> Int {\n    1\n}\n\n\
             pub enum Rank {\n    Bronze,\n    Gold(Int),\n}\n\n\
             pub const CAP: Int = 40;\n\n\
             pub fn emit(io: Io, total: Int) -> Result<Unit, Error> uses {Io.write} {\n\
                 io.write(text: total.to_text())\n}\n",
        )
        .unwrap();
        std::fs::write(dir.join("src/main.xn"), "fn main() {\n}\n").unwrap();

        let first = api_dump(&dir).unwrap();
        let second = api_dump(&dir).unwrap();
        std::fs::remove_dir_all(&dir).ok();
        assert_eq!(first, second, "not deterministic");

        assert!(first.starts_with(&format!("# {DUMP_VERSION}\n# hash: fnv1a64:")));
        assert!(first.contains("module depot.locker\n"));
        assert!(first.contains("pub struct Locker {\n    label: String,\n    var weight: Int,\n}"));
        assert!(first.contains("pub enum Rank {\n    Bronze,\n    Gold(Int),\n}"));
        assert!(first.contains("pub const CAP: Int"));
        assert!(
            first
                .contains("pub fn emit(io: Io, total: Int) -> Result<Unit, Error> uses {Io.write}")
        );
        // Private items and the entry module stay out.
        assert!(!first.contains("hidden"));
        assert!(!first.contains("module main"));
        // The recorded hash is the hash of the body it precedes.
        let body_at = first.find("\n\n").unwrap() + 2;
        let recorded = first
            .lines()
            .nth(1)
            .and_then(|line| line.strip_prefix("# hash: fnv1a64:"))
            .unwrap();
        assert_eq!(recorded, format!("{:016x}", fnv1a64(&first[body_at..])));
    }
}
