//! Conformance suite for design/0015 — task boundaries v1 (CLI level).
//!
//! Written before the implementation (design/0015 §7: semantics first).
//! The checking-level half lives in `xenith-sema/tests/concurrency.rs`;
//! this half proves the execution semantics through the real binary —
//! eager completion at the spawn point, argument evaluation exactly once
//! in normal order, trap attribution, early-exit discard — plus the
//! canonical formatting of the new forms and the `explain` surface.

use std::path::PathBuf;
use std::process::Command;

#[path = "support/task_corpus.rs"]
mod task_corpus;

/// The program sources live in `support/task_corpus.rs`, shared with the
/// differential harness (`executor_equivalence.rs`). Sharing them is what
/// makes "the harness runs every conformance program" true by construction
/// rather than by inspection (design/0017 §5).
fn program(name: &str) -> &'static str {
    task_corpus::source(name)
}

/// A scratch directory unique to one test, outside any xenith project.
fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir()
        .join("xenith-0015-conformance")
        .join(format!("{name}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("scratch directory");
    dir
}

/// Write `source` as a lone file and `xenith run` it.
fn run(name: &str, source: &str) -> (i32, String, String) {
    let dir = scratch(name);
    let file = dir.join("main.xn");
    std::fs::write(&file, source).expect("write test program");
    let output = Command::new(env!("CARGO_BIN_EXE_xenith"))
        .current_dir(&dir)
        .args(["run", "main.xn"])
        .output()
        .expect("the compiler binary runs");
    (
        output.status.code().unwrap_or(-1),
        String::from_utf8(output.stdout).expect("stdout is UTF-8"),
        String::from_utf8(output.stderr).expect("stderr is UTF-8"),
    )
}

/// One-based line:column of the first occurrence of `needle`, counted the
/// way diagnostics count — characters, not bytes.
fn line_col_of(source: &str, needle: &str) -> (usize, usize) {
    let offset = source.find(needle).expect("needle present");
    let before = &source[..offset];
    let line = before.matches('\n').count() + 1;
    let column = before
        .rsplit_once('\n')
        .map(|(_, tail)| tail)
        .unwrap_or(before)
        .chars()
        .count()
        + 1;
    (line, column)
}

// ===================================================================
// EXECUTION — positive programs, observable behaviour
// ===================================================================

#[test]
fn two_child_fan_out_computes_and_prints() {
    let source = program("fan_out");
    let (exit, stdout, stderr) = run("fan_out", source);
    assert_eq!(exit, 0, "stderr: {stderr}");
    assert_eq!(stdout, "43", "16 + 27");
}

#[test]
fn a_result_child_propagates_err_through_await_try() {
    let source = program("result_err");
    let (exit, stdout, _) = run("result_err", source);
    assert_eq!(exit, 1, "main returned the child's Err");
    assert_eq!(stdout, "", "the write after the failed await never ran");
}

#[test]
fn a_result_child_propagates_ok_through_await_try() {
    let source = program("result_ok");
    let (exit, stdout, stderr) = run("result_ok", source);
    assert_eq!(exit, 0, "stderr: {stderr}");
    assert_eq!(stdout, "42");
}

#[test]
fn nested_scopes_run_inside_out() {
    let source = program("nested");
    let (exit, stdout, stderr) = run("nested", source);
    assert_eq!(exit, 0, "stderr: {stderr}");
    assert_eq!(stdout, "112", "11 + 101");
}

#[test]
fn statement_form_spawn_of_a_unit_callee_runs() {
    let source = program("statement_form");
    let (exit, stdout, stderr) = run("statement_form", source);
    assert_eq!(exit, 0, "stderr: {stderr}");
    assert_eq!(stdout, "done");
}

// ===================================================================
// EXECUTION — eager completion at the spawn point
// ===================================================================

// REPLACED (design/0017 §2). This slot used to hold
// `a_trap_in_the_child_fires_at_the_spawn_statement_not_at_await`: a program
// with `io.write(text: "between ")?;` sitting between the spawn and the
// await, asserting that stdout stopped at "before " and so proving the child
// had already run at the spawn point.
//
// That program is now **refused** by XN6011 — an effect while a task is in
// flight — and the two tests below take its place: the refusal itself, and
// the legal shape (effects after every await, still inside the scope) which
// still runs. The replacement is recorded rather than silently dropped: it
// is not a loss of coverage but the point of 0017. The old test measured
// *when* a child's trap became visible, and the whole design is that no
// program can construct that observation any more. What survives — the trap
// surfaces, names the child, exits 101 — is asserted below.
#[test]
fn an_effect_between_spawn_and_await_is_refused_as_xn6011() {
    let source = program("flight_effect_refusal");
    let (exit, stdout, _) = run("flight_effect", source);
    assert_eq!(exit, 2, "a file with diagnostics is refused, not run");
    assert!(
        stdout.contains("error[XN6011]"),
        "the in-flight rule is what refuses it:\n{stdout}"
    );
    // The refusal points at the effect, not at the spawn: the spawn is fine,
    // the write is what cannot be there.
    let (line, column) = line_col_of(source, "io.write(text: \"between \")");
    assert!(
        stdout.contains(&format!(":{line}:{column}")),
        "the refusal points at the effect ({line}:{column}):\n{stdout}"
    );
    assert!(
        stdout.contains("a task computes a plan — effects run in the parent, after await"),
        "the canonical sentence is the teach, unchanged:\n{stdout}"
    );
}

#[test]
fn effects_after_every_await_inside_the_scope_are_legal_and_run() {
    // The shape design/0017 §1 blesses: spawn, spawn, await both, then act —
    // and the acting happens inside the scope, which is legal because the
    // flight is over.
    let source = program("flight_over");
    let (exit, stdout, stderr) = run("flight_over", source);
    assert_eq!(exit, 0, "stderr: {stderr}");
    assert_eq!(stdout, "before 6 after");
}

#[test]
fn a_child_trap_surfaces_naming_the_child_and_exits_101() {
    // What the replaced test was really guarding, minus the observation
    // XN6011 now makes unconstructible: a trapping child stops the program,
    // the trap names the child, and the stdout written before the scope is
    // all there is.
    let source = program("child_trap");
    let (exit, stdout, stderr) = run("child_trap", source);
    assert_eq!(exit, 101, "a trap fired");
    assert_eq!(
        stdout, "before ",
        "nothing after the scope opened was written"
    );
    assert!(
        stderr.contains("boom") && stderr.contains("division by zero"),
        "the child and its trap are both named:\n{stderr}"
    );
}

#[test]
fn spawn_arguments_evaluate_exactly_once_in_normal_order() {
    let source = program("arg_order");
    let (exit, stdout, stderr) = run("arg_order", source);
    assert_eq!(exit, 0, "stderr: {stderr}");
    // a sees count = 1, b sees count = 2 and contributes 20: 21 total.
    // count = 2 at the end proves each argument ran exactly once; 21 rather
    // than 12 proves left-to-right.
    assert_eq!(stdout, "21 2");
}

#[test]
fn a_trapping_argument_traps_at_its_own_position_before_the_child_exists() {
    let source = program("arg_trap");
    let (exit, _, stderr) = run("arg_trap", source);
    assert_eq!(exit, 101);
    let (line, column) = line_col_of(source, "1 / 0");
    assert!(
        stderr.contains(&format!(":{line}:{column}")),
        "the first argument trapped first, at its own span ({line}:{column}):\n{stderr}"
    );
    assert!(
        !stderr.contains("trapped:"),
        "an argument trap is the parent's, not the child's:\n{stderr}"
    );
}

// ===================================================================
// EXECUTION — early exit discards the consumed-ready result
// ===================================================================

#[test]
fn an_early_question_mark_exit_discards_the_ready_result() {
    let source = program("early_discard");
    let (exit, stdout, stderr) = run("early_discard", source);
    assert_eq!(exit, 1, "main returned the Err from `fail()?`");
    assert_eq!(stdout, "", "the ready result was discarded, not printed");
    assert!(
        !stderr.contains("runtime error"),
        "discarding a pure child's result is silent:\n{stderr}"
    );
}

// ===================================================================
// Compatibility — the words stay ordinary identifiers elsewhere
// ===================================================================

#[test]
fn spawn_and_scope_still_run_as_ordinary_names() {
    let source = program("compat_names");
    let (exit, stdout, stderr) = run("compat_names", source);
    assert_eq!(exit, 0, "stderr: {stderr}");
    assert_eq!(stdout, "42");
}

// ===================================================================
// FORMAT — canonical forms for scope / spawn / await
// ===================================================================

#[test]
fn the_task_forms_have_one_canonical_spelling() {
    let canonical = r#"fn work(n: Int) -> Int {
    n
}

fn f() -> Int uses {Task.spawn} {
    scope {
        let j = spawn work(n: 1);
        j.await
    }
}
"#;
    let formatted = xenith_syntax::format(canonical).expect("canonical source formats");
    assert_eq!(formatted, canonical, "the canonical form is a fixed point");
}

#[test]
fn parentheses_around_a_spawn_initializer_vanish() {
    let source = "fn work(n: Int) -> Int { n }\nfn f() -> Int uses {Task.spawn} { scope { let j = (spawn work(n: 1)); j.await } }\n";
    let formatted = xenith_syntax::format(source).expect("formats");
    assert!(
        formatted.contains("let j = spawn work(n: 1);"),
        "{formatted}"
    );
    assert!(!formatted.contains("(spawn"), "{formatted}");
}

#[test]
fn a_scope_statement_formats_like_a_block_construct() {
    let source = "fn ping() {}\nfn f() uses {Task.spawn} { scope { spawn ping(); } }\n";
    let formatted = xenith_syntax::format(source).expect("formats");
    let expected = r#"fn ping() {}

fn f() uses {Task.spawn} {
    scope {
        spawn ping();
    }
}
"#;
    assert_eq!(formatted, expected);
}

#[test]
fn formatting_the_task_forms_is_idempotent() {
    let source = "fn work(n: Int) -> Int { n }\nfn f(flag: Bool) -> Int uses {Task.spawn} { scope { let j = spawn work(n: 1); if flag { j.await } else { j.await + 1 } } }\n";
    let once = xenith_syntax::format(source).expect("formats once");
    let twice = xenith_syntax::format(&once).expect("formats twice");
    assert_eq!(once, twice, "canonical output is a fixed point");
}

// ===================================================================
// Diagnostics through the binary — explain and the teaching switch
// ===================================================================

#[test]
fn every_new_code_is_explained_by_the_binary() {
    for code in [
        "XN6001", "XN6002", "XN6003", "XN6004", "XN6005", "XN6006", "XN6007", "XN6008", "XN6009",
        "XN6010", // design/0017 §1, added to the same family and the same gate.
        "XN6011",
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_xenith"))
            .args(["explain", code])
            .output()
            .expect("the compiler binary runs");
        assert!(
            output.status.success(),
            "`xenith explain {code}` must know the code"
        );
        let text = String::from_utf8(output.stdout).expect("UTF-8");
        assert!(text.len() > 80, "{code} deserves a real explanation");
    }
}

#[test]
fn teaching_off_strips_exactly_the_canonical_sentence() {
    let dir = scratch("teaching_switch");
    let file = dir.join("main.xn");
    std::fs::write(
        &file,
        r#"fn shout(io: Io, text: String) -> Result<Unit, Error> uses {Io.write} {
    io.write(text: text)
}

fn f(io: Io) uses {Task.spawn} {
    scope {
        spawn shout(io: io, text: "hi");
    }
}
"#,
    )
    .expect("write test program");

    let check = |args: &[&str]| -> serde_json::Value {
        let output = Command::new(env!("CARGO_BIN_EXE_xenith"))
            .current_dir(&dir)
            .arg("check")
            .args(args)
            .arg("main.xn")
            .output()
            .expect("the compiler binary runs");
        serde_json::from_slice(&output.stdout).expect("JSON diagnostics")
    };

    let on = check(&["--json"]);
    let off = check(&["--json", "--diagnostic-teaching=off"]);

    let on_message = on[0]["diagnostics"][0]["message"]
        .as_str()
        .expect("message");
    let off_message = off[0]["diagnostics"][0]["message"]
        .as_str()
        .expect("message");
    assert_eq!(on[0]["diagnostics"][0]["code"], "XN6002");
    assert!(
        on_message.contains("a task computes a plan — effects run in the parent, after await"),
        "{on_message}"
    );
    assert!(
        on_message.starts_with(off_message),
        "teaching only appends: {on_message:?} vs {off_message:?}"
    );
    assert!(
        !off_message.contains("a task computes a plan"),
        "{off_message}"
    );
}
