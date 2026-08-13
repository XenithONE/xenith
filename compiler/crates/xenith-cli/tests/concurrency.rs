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
    let source = r#"fn square(n: Int) -> Int {
    n * n
}

fn cube(n: Int) -> Int {
    n * n * n
}

fn main(io: Io) -> Result<Unit, Error> uses {Io.write, Task.spawn} {
    let total = scope {
        let a = spawn square(n: 4);
        let b = spawn cube(n: 3);
        a.await + b.await
    };
    io.write(text: total.to_text())?;
    return Ok(unit);
}
"#;
    let (exit, stdout, stderr) = run("fan_out", source);
    assert_eq!(exit, 0, "stderr: {stderr}");
    assert_eq!(stdout, "43", "16 + 27");
}

#[test]
fn a_result_child_propagates_err_through_await_try() {
    let source = r#"fn parse(text: String) -> Result<Int, Error> {
    text.try_to_int()
}

fn main(io: Io) -> Result<Unit, Error> uses {Io.write, Task.spawn} {
    scope {
        let j = spawn parse(text: "not-a-number");
        let v = j.await?;
        io.write(text: v.to_text())?;
    }
    return Ok(unit);
}
"#;
    let (exit, stdout, _) = run("result_err", source);
    assert_eq!(exit, 1, "main returned the child's Err");
    assert_eq!(stdout, "", "the write after the failed await never ran");
}

#[test]
fn a_result_child_propagates_ok_through_await_try() {
    let source = r#"fn parse(text: String) -> Result<Int, Error> {
    text.try_to_int()
}

fn main(io: Io) -> Result<Unit, Error> uses {Io.write, Task.spawn} {
    scope {
        let j = spawn parse(text: "42");
        let v = j.await?;
        io.write(text: v.to_text())?;
    }
    return Ok(unit);
}
"#;
    let (exit, stdout, stderr) = run("result_ok", source);
    assert_eq!(exit, 0, "stderr: {stderr}");
    assert_eq!(stdout, "42");
}

#[test]
fn nested_scopes_run_inside_out() {
    let source = r#"fn work(n: Int) -> Int {
    n + 1
}

fn main(io: Io) -> Result<Unit, Error> uses {Io.write, Task.spawn} {
    let total = scope {
        let outer = spawn work(n: 10);
        let inner_total = scope {
            let inner = spawn work(n: 100);
            inner.await
        };
        outer.await + inner_total
    };
    io.write(text: total.to_text())?;
    return Ok(unit);
}
"#;
    let (exit, stdout, stderr) = run("nested", source);
    assert_eq!(exit, 0, "stderr: {stderr}");
    assert_eq!(stdout, "112", "11 + 101");
}

#[test]
fn statement_form_spawn_of_a_unit_callee_runs() {
    let source = r#"fn ping() {
    let x = 1;
}

fn main(io: Io) -> Result<Unit, Error> uses {Io.write, Task.spawn} {
    scope {
        spawn ping();
    }
    io.write(text: "done")?;
    return Ok(unit);
}
"#;
    let (exit, stdout, stderr) = run("statement_form", source);
    assert_eq!(exit, 0, "stderr: {stderr}");
    assert_eq!(stdout, "done");
}

// ===================================================================
// EXECUTION — eager completion at the spawn point
// ===================================================================

#[test]
fn a_trap_in_the_child_fires_at_the_spawn_statement_not_at_await() {
    let source = r#"fn boom() -> Int {
    1 / 0
}

fn main(io: Io) -> Result<Unit, Error> uses {Io.write, Task.spawn} {
    io.write(text: "before ")?;
    scope {
        let j = spawn boom();
        io.write(text: "between ")?;
        let x = j.await;
        io.write(text: x.to_text())?;
    }
    return Ok(unit);
}
"#;
    let (exit, stdout, stderr) = run("eager_trap", source);
    assert_eq!(exit, 101, "a trap fired");
    assert_eq!(
        stdout, "before ",
        "the child ran at the spawn point, so nothing between spawn and await executed"
    );
    // The trap is attributed to the spawn site, carrying the child context.
    let (line, column) = line_col_of(source, "spawn boom()");
    assert!(
        stderr.contains(&format!(":{line}:{column}")),
        "the trap points at the spawn statement ({line}:{column}):\n{stderr}"
    );
    assert!(
        stderr.contains("boom") && stderr.contains("division by zero"),
        "the child and its trap are both named:\n{stderr}"
    );
}

#[test]
fn spawn_arguments_evaluate_exactly_once_in_normal_order() {
    let source = r#"fn add(a: Int, b: Int) -> Int {
    a + b
}

fn main(io: Io) -> Result<Unit, Error> uses {Io.write, Task.spawn} {
    var count = 0;
    scope {
        let j = spawn add(a: { count = count + 1; count }, b: { count = count + 1; count * 10 });
        let v = j.await;
        io.write(text: v.to_text())?;
        io.write(text: " ")?;
        io.write(text: count.to_text())?;
    }
    return Ok(unit);
}
"#;
    let (exit, stdout, stderr) = run("arg_order", source);
    assert_eq!(exit, 0, "stderr: {stderr}");
    // a sees count = 1, b sees count = 2 and contributes 20: 21 total.
    // count = 2 at the end proves each argument ran exactly once; 21 rather
    // than 12 proves left-to-right.
    assert_eq!(stdout, "21 2");
}

#[test]
fn a_trapping_argument_traps_at_its_own_position_before_the_child_exists() {
    let source = r#"fn add(a: Int, b: Int) -> Int {
    a + b
}

fn main(io: Io) -> Result<Unit, Error> uses {Io.write, Task.spawn} {
    scope {
        let j = spawn add(a: 1 / 0, b: 2 / 0);
        let v = j.await;
        io.write(text: v.to_text())?;
    }
    return Ok(unit);
}
"#;
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
    let source = r#"fn work(n: Int) -> Int {
    n * 2
}

fn fail() -> Result<Int, Error> {
    "x".try_to_int()
}

fn main(io: Io) -> Result<Unit, Error> uses {Io.write, Task.spawn} {
    scope {
        let j = spawn work(n: 21);
        let g = fail()?;
        io.write(text: j.await.to_text())?;
        io.write(text: g.to_text())?;
    }
    return Ok(unit);
}
"#;
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
    let source = r#"fn main(io: Io) -> Result<Unit, Error> uses {Io.write} {
    let spawn = 40;
    let scope = 2;
    let total = spawn + scope;
    if scope > 0 {
        io.write(text: total.to_text())?;
    }
    return Ok(unit);
}
"#;
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
        "XN6010",
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
