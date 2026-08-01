use xenith_sema::def;
use xenith_syntax::parse;
use xenith_vm::run;

/// Check, then run. Panics on diagnostics — a test that executes unchecked
/// code is testing nothing.
fn execute(source: &str) -> xenith_vm::Outcome {
    let parsed = parse(source);
    assert!(
        parsed.diagnostics.is_empty(),
        "test source must parse: {:?}",
        parsed.diagnostics
    );
    let analysis = xenith_sema::analyze(&parsed.module);
    assert!(
        analysis.diagnostics.is_empty(),
        "test source must check: {:#?}",
        analysis
            .diagnostics
            .iter()
            .map(|d| (d.code.id(), d.message.clone()))
            .collect::<Vec<_>>()
    );
    let (table, _) = def::collect(&parsed.module);
    run(&parsed.module, &table)
}

fn stdout_of(source: &str) -> String {
    let outcome = execute(source);
    assert_eq!(outcome.exit, 0, "{:?}", outcome.error);
    String::from_utf8(outcome.stdout).expect("utf8")
}

/// Runs `main` and returns the trap message; panics if nothing trapped.
fn trap_of(source: &str) -> String {
    let outcome = execute(source);
    assert_eq!(outcome.exit, 101, "expected a trap");
    outcome.error.expect("trap message").0
}

/// A `fn main(io: Io)` wrapper writing the result of `body` as text.
fn printing(body: &str) -> String {
    format!(
        "fn main(io: Io) -> Result<Unit, Error> uses {{Io.write}} {{\n\
             let value = {body};\n\
             io.write(text: value.to_text())?;\n\
             return Ok(unit);\n\
         }}"
    )
}

// ---------------------------------------------------------------- evaluation

#[test]
fn arithmetic_evaluates() {
    assert_eq!(stdout_of(&printing("2 + 3 * 4")), "14");
    assert_eq!(stdout_of(&printing("(2 + 3) * 4")), "20");
    assert_eq!(stdout_of(&printing("7 / 2")), "3");
    assert_eq!(stdout_of(&printing("7 % 2")), "1");
    assert_eq!(stdout_of(&printing("-5 + 1")), "-4");
}

#[test]
fn bitwise_and_shifts_evaluate() {
    assert_eq!(stdout_of(&printing("6 & 3")), "2");
    assert_eq!(stdout_of(&printing("6 | 3")), "7");
    assert_eq!(stdout_of(&printing("6 ^ 3")), "5");
    assert_eq!(stdout_of(&printing("1 << 4")), "16");
    assert_eq!(stdout_of(&printing("16 >> 2")), "4");
}

#[test]
fn overflow_traps_deterministically() {
    // The kernel: overflow is a trap, never a wrap (design/0003).
    let source = printing("9_223_372_036_854_775_807 + 1");
    assert!(trap_of(&source).contains("overflow"));
}

#[test]
fn division_by_zero_traps() {
    assert!(trap_of(&printing("1 / 0")).contains("division by zero"));
    assert!(trap_of(&printing("1 % 0")).contains("remainder by zero"));
}

#[test]
fn shift_out_of_range_traps() {
    assert!(trap_of(&printing("1 << 64")).contains("out of range"));
}

#[test]
fn logic_short_circuits() {
    // If && evaluated its right side, this would trap on 1/0.
    let source = "fn explode() -> Bool {\n    1 / 0 == 1\n}\n\n\
                  fn main(io: Io) -> Result<Unit, Error> uses {Io.write} {\n    \
                      let safe = false && explode();\n    \
                      if safe {\n        io.write(text: \"boom\")?;\n    } else {\n        \
                          io.write(text: \"short-circuited\")?;\n    }\n    \
                      return Ok(unit);\n}";
    assert_eq!(stdout_of(source), "short-circuited");
}

#[test]
fn evaluation_order_is_left_to_right() {
    // Each call appends; the output records the order.
    let source = "fn tag(io: Io, label: String) -> Result<Int, Error> uses {Io.write} {\n    \
                      io.write(text: label)?;\n    Ok(1)\n}\n\n\
                  fn main(io: Io) -> Result<Unit, Error> uses {Io.write} {\n    \
                      let total = tag(io: io, label: \"a\")? + tag(io: io, label: \"b\")?;\n    \
                      io.write(text: total.to_text())?;\n    return Ok(unit);\n}";
    assert_eq!(stdout_of(source), "ab2");
}

// -------------------------------------------------------------- control flow

#[test]
fn while_loops_and_var_mutation_run() {
    let source = "fn main(io: Io) -> Result<Unit, Error> uses {Io.write} {\n    \
                      var total = 0;\n    var n = 1;\n    \
                      while n <= 10 {\n        total = total + n;\n        n = n + 1;\n    }\n    \
                      io.write(text: total.to_text())?;\n    return Ok(unit);\n}";
    assert_eq!(stdout_of(source), "55");
}

#[test]
fn break_and_continue_behave() {
    let source = "fn main(io: Io) -> Result<Unit, Error> uses {Io.write} {\n    \
                      var n = 0;\n    var sum = 0;\n    \
                      while true {\n        n = n + 1;\n        \
                          if n > 10 {\n            break;\n        }\n        \
                          if n % 2 == 0 {\n            continue;\n        }\n        \
                          sum = sum + n;\n    }\n    \
                      io.write(text: sum.to_text())?;\n    return Ok(unit);\n}";
    // 1 + 3 + 5 + 7 + 9
    assert_eq!(stdout_of(source), "25");
}

#[test]
fn recursion_runs() {
    let source = "fn factorial(n: Int) -> Int {\n    \
                      if n <= 1 {\n        1\n    } else {\n        n * factorial(n: n - 1)\n    }\n}\n\n\
                  fn main(io: Io) -> Result<Unit, Error> uses {Io.write} {\n    \
                      io.write(text: factorial(n: 10).to_text())?;\n    return Ok(unit);\n}";
    assert_eq!(stdout_of(source), "3628800");
}

#[test]
fn match_selects_by_variant_guard_and_alternative() {
    let source = r#"
enum Grade {
    Pass(Int),
    Fail,
    Absent,
}

fn describe(grade: Grade) -> String {
    match grade {
        Grade.Pass(score) if score >= 90 => "excellent",
        Grade.Pass(_) => "pass",
        Grade.Fail | Grade.Absent => "no credit",
    }
}

fn main(io: Io) -> Result<Unit, Error> uses {Io.write} {
    io.write(text: describe(grade: Grade.Pass(95)))?;
    io.write(text: ",")?;
    io.write(text: describe(grade: Grade.Pass(60)))?;
    io.write(text: ",")?;
    io.write(text: describe(grade: Grade.Absent))?;
    return Ok(unit);
}
"#;
    assert_eq!(stdout_of(source), "excellent,pass,no credit");
}

// ----------------------------------------------------- structs, enums, holes

#[test]
fn struct_literals_field_access_and_field_assignment_run() {
    let source = r#"
struct Player {
    name: String,
    var score: Int,
}

fn main(io: Io) -> Result<Unit, Error> uses {Io.write} {
    var player = Player { name: "ada", score: 10 };
    player.score = player.score + 5;
    io.write(text: player.name.concat(other: player.score.to_text()))?;
    return Ok(unit);
}
"#;
    assert_eq!(stdout_of(source), "ada15");
}

#[test]
fn option_result_and_question_mark_run() {
    let source = r#"
enum ScoreError {
    Overflow,
}

fn try_double(n: Int) -> Result<Int, ScoreError> {
    n.checked_add(other: n).to_result(error: ScoreError.Overflow)
}

fn main(io: Io) -> Result<Unit, Error> uses {Io.write} {
    let doubled = match try_double(n: 21) {
        Ok(value) => value,
        Err(_) => 0,
    };
    io.write(text: doubled.to_text())?;
    return Ok(unit);
}
"#;
    assert_eq!(stdout_of(source), "42");
}

#[test]
fn question_mark_propagates_err_out_of_main() {
    let source = r#"
enum ScoreError {
    Overflow,
}

fn try_double(n: Int) -> Result<Int, ScoreError> {
    n.checked_add(other: n).to_result(error: ScoreError.Overflow)
}

fn main() -> Result<Int, ScoreError> {
    let doubled = try_double(n: 9_223_372_036_854_775_807)?;
    Ok(doubled)
}
"#;
    let outcome = execute(source);
    assert_eq!(outcome.exit, 1, "main returned Err");
}

#[test]
fn reaching_a_hole_traps_and_points_at_goals() {
    let source = "fn main() -> Int {\n    let x: Int = ??start;\n    x\n}";
    let message = trap_of(source);
    assert!(message.contains("??start"), "{message}");
    assert!(message.contains("goals"), "{message}");
}

#[test]
fn a_hole_behind_an_untaken_branch_does_not_trap() {
    // Partial programs run right up to the missing part — that is the point.
    let source = "fn main(io: Io) -> Result<Unit, Error> uses {Io.write} {\n    \
                      if true {\n        io.write(text: \"took the finished path\")?;\n    } else {\n        \
                          let x: Int = ??later;\n        io.write(text: x.to_text())?;\n    }\n    \
                      return Ok(unit);\n}";
    assert_eq!(stdout_of(source), "took the finished path");
}

// ------------------------------------------------------------------ closures

#[test]
fn lambdas_capture_by_value() {
    let source = "fn main(io: Io) -> Result<Unit, Error> uses {Io.write} {\n    \
                      var base = 10;\n    \
                      let add_base = |n: Int| n + base;\n    \
                      base = 100;\n    \
                      io.write(text: add_base(5).to_text())?;\n    return Ok(unit);\n}";
    // Captured when the lambda was made: 10, not 100. Value semantics.
    assert_eq!(stdout_of(source), "15");
}

#[test]
fn async_fn_returns_a_task_and_await_unwraps_it() {
    let source = "async fn compute() -> Int {\n    40 + 2\n}\n\n\
                  fn main(io: Io) -> Result<Unit, Error> uses {Io.write} {\n    \
                      let value = compute().await;\n    \
                      io.write(text: value.to_text())?;\n    return Ok(unit);\n}";
    assert_eq!(stdout_of(source), "42");
}

// ------------------------------------------------------------------ strings

#[test]
fn string_escapes_resolve() {
    let source = "fn main(io: Io) -> Result<Unit, Error> uses {Io.write} {\n    \
                      io.write(text: \"line one\\nline two\")?;\n    return Ok(unit);\n}";
    assert_eq!(stdout_of(source), "line one\nline two");
}

#[test]
fn float_comparisons_follow_ieee() {
    let source = "fn main(io: Io) -> Result<Unit, Error> uses {Io.write} {\n    \
                      let nan = 0.0 / 0.0;\n    \
                      if nan == nan {\n        io.write(text: \"equal\")?;\n    } else {\n        \
                          io.write(text: \"nan != nan\")?;\n    }\n    \
                      return Ok(unit);\n}";
    assert_eq!(stdout_of(source), "nan != nan");
}

// ------------------------------------------------------------------- guards

#[test]
fn a_module_without_main_says_so() {
    let parsed = parse("fn helper() -> Int {\n    1\n}");
    let (table, _) = def::collect(&parsed.module);
    let outcome = run(&parsed.module, &table);
    assert_eq!(outcome.exit, 101);
    assert!(outcome.error.expect("message").0.contains("fn main"));
}

#[test]
fn the_hello_example_runs_and_greets() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(3)
        .expect("repository root")
        .join("examples/hello.xn");
    let source = std::fs::read_to_string(root).expect("hello.xn");
    assert_eq!(stdout_of(&source), "Hello, world");
}
