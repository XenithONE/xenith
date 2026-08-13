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

/// [`printing`] for a `body` that is already a `String`.
fn printing_text(body: &str) -> String {
    format!(
        "fn main(io: Io) -> Result<Unit, Error> uses {{Io.write}} {{\n\
             let value = {body};\n\
             io.write(text: value)?;\n\
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
fn consts_evaluate_to_their_folded_value() {
    // The checker proved the initializer is a literal or arithmetic over
    // literals and folded the integers; the interpreter reads the value off
    // the initializer the same way it reads any literal.
    let source = r#"
const LIMIT: Int = 1_000;
const HALF: Int = 1_000 / 2;
const NEG: Int = -5;
const NAME: String = "ada";
const ON: Bool = !false;

fn cap(n: Int) -> Int {
    if n > LIMIT {
        LIMIT
    } else {
        n
    }
}

fn main(io: Io) -> Result<Unit, Error> uses {Io.write} {
    io.write(text: cap(n: 5000).to_text())?;
    io.write(text: HALF.to_text())?;
    io.write(text: NEG.to_text())?;
    io.write(text: NAME)?;
    if ON {
        io.write(text: "!")?;
    }
    return Ok(unit);
}
"#;
    assert_eq!(stdout_of(source), "1000500-5ada!");
}

#[test]
fn generic_struct_literals_and_payload_less_generic_variants_run() {
    // They check now (the expected type seeds the arguments), and the
    // interpreter erases the arguments entirely — so the proof that they
    // are *constructible* has to run, not merely check.
    let source = r#"
struct Pair<T> {
    a: T,
    b: T,
}

enum Wrap<T> {
    Hollow,
    Full(T),
}

fn labelled(w: Wrap<Int>) -> String {
    match w {
        Wrap.Hollow => "hollow",
        Wrap.Full(v) => v.to_text(),
    }
}

fn main(io: Io) -> Result<Unit, Error> uses {Io.write} {
    let p: Pair<Int> = Pair { a: 1, b: 2 };
    let names: Pair<String> = Pair { a: "x", b: "y" };
    io.write(text: p.a.to_text().concat(other: names.b))?;
    io.write(text: labelled(w: Wrap.Hollow))?;
    io.write(text: labelled(w: Wrap.Full(7)))?;
    return Ok(unit);
}
"#;
    assert_eq!(stdout_of(source), "1yhollow7");
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

// -------------------------------------------------------------------- lists

#[test]
fn list_literals_len_and_get_run() {
    let source = r#"
fn main(io: Io) -> Result<Unit, Error> uses {Io.write} {
    let xs = [10, 20, 30];
    let picked = match xs.get(index: 1) {
        Some(value) => value,
        None => -1,
    };
    io.write(text: xs.len().to_text())?;
    io.write(text: ",")?;
    io.write(text: picked.to_text())?;
    return Ok(unit);
}
"#;
    assert_eq!(stdout_of(source), "3,20");
}

#[test]
fn get_out_of_range_and_negative_are_none_not_traps() {
    let source = r#"
fn describe(xs: List<Int>, index: Int) -> String {
    match xs.get(index: index) {
        Some(_) => "some",
        None => "none",
    }
}

fn main(io: Io) -> Result<Unit, Error> uses {Io.write} {
    let xs = [1, 2];
    io.write(text: describe(xs: xs, index: -1))?;
    io.write(text: ",")?;
    io.write(text: describe(xs: xs, index: 2))?;
    return Ok(unit);
}
"#;
    assert_eq!(stdout_of(source), "none,none");
}

#[test]
fn push_and_pop_mutate_in_place() {
    let source = r#"
fn main(io: Io) -> Result<Unit, Error> uses {Io.write} {
    var xs: List<Int> = [];
    xs.push(item: 1);
    xs.push(item: 2);
    xs.push(item: 3);
    let last = match xs.pop() {
        Some(value) => value,
        None => -1,
    };
    io.write(text: xs.len().to_text())?;
    io.write(text: ",")?;
    io.write(text: last.to_text())?;
    return Ok(unit);
}
"#;
    assert_eq!(stdout_of(source), "2,3");
}

#[test]
fn pop_on_an_empty_list_is_none() {
    let source = r#"
fn main(io: Io) -> Result<Unit, Error> uses {Io.write} {
    var xs: List<Int> = [];
    let text = match xs.pop() {
        Some(_) => "some",
        None => "none",
    };
    io.write(text: text)?;
    return Ok(unit);
}
"#;
    assert_eq!(stdout_of(source), "none");
}

#[test]
fn replace_returns_the_old_value_and_out_of_range_leaves_the_list_alone() {
    let source = r#"
fn main(io: Io) -> Result<Unit, Error> uses {Io.write} {
    var xs = [1, 2, 3];
    let old = match xs.replace(index: 1, value: 9) {
        Some(value) => value,
        None => -1,
    };
    let missed = match xs.replace(index: 9, value: 7) {
        Some(value) => value,
        None => -1,
    };
    io.write(text: old.to_text())?;
    io.write(text: ",")?;
    io.write(text: missed.to_text())?;
    io.write(text: "|")?;
    io.write(text: xs.join(sep: ","))?;
    return Ok(unit);
}
"#;
    assert_eq!(stdout_of(source), "2,-1|1,9,3");
}

#[test]
fn contains_compares_structurally() {
    let source = r#"
fn main(io: Io) -> Result<Unit, Error> uses {Io.write} {
    let xs = [1, 2, 3];
    if xs.contains(item: 2) {
        io.write(text: "yes")?;
    } else {
        io.write(text: "no")?;
    }
    io.write(text: ",")?;
    if xs.contains(item: 9) {
        io.write(text: "yes")?;
    } else {
        io.write(text: "no")?;
    }
    return Ok(unit);
}
"#;
    assert_eq!(stdout_of(source), "yes,no");
}

#[test]
fn sorted_returns_a_new_list_and_leaves_the_receiver_alone() {
    let source = r#"
fn main(io: Io) -> Result<Unit, Error> uses {Io.write} {
    let xs = [3, 1, 2];
    let sorted = xs.sorted();
    io.write(text: sorted.join(sep: ","))?;
    io.write(text: "|")?;
    io.write(text: xs.join(sep: ","))?;
    io.write(text: "|")?;
    io.write(text: ["pear", "apple", "fig"].sorted().join(sep: ","))?;
    return Ok(unit);
}
"#;
    assert_eq!(stdout_of(source), "1,2,3|3,1,2|apple,fig,pear");
}

#[test]
fn concat_builds_a_new_list() {
    let source = r#"
fn main(io: Io) -> Result<Unit, Error> uses {Io.write} {
    let a = [1, 2];
    let b = [3];
    io.write(text: a.concat(other: b).join(sep: ","))?;
    io.write(text: "|")?;
    io.write(text: a.len().to_text())?;
    return Ok(unit);
}
"#;
    assert_eq!(stdout_of(source), "1,2,3|2");
}

#[test]
fn join_renders_strings_verbatim_and_other_elements_as_text() {
    let source = r#"
fn main(io: Io) -> Result<Unit, Error> uses {Io.write} {
    let empty: List<String> = [];
    io.write(text: ["a", "b", "c"].join(sep: "-"))?;
    io.write(text: "|")?;
    io.write(text: [1, 2].join(sep: ","))?;
    io.write(text: "|")?;
    io.write(text: [true, false].join(sep: ","))?;
    io.write(text: "|")?;
    io.write(text: empty.join(sep: ","))?;
    io.write(text: "end")?;
    return Ok(unit);
}
"#;
    assert_eq!(stdout_of(source), "a-b-c|1,2|true,false|end");
}

#[test]
fn lists_are_values_so_a_binding_copies() {
    // D1: reads and bindings copy; mutating one copy must not touch another.
    let source = r#"
fn main(io: Io) -> Result<Unit, Error> uses {Io.write} {
    var a = [1];
    let b = a;
    a.push(item: 2);
    io.write(text: a.len().to_text())?;
    io.write(text: ",")?;
    io.write(text: b.len().to_text())?;
    return Ok(unit);
}
"#;
    assert_eq!(stdout_of(source), "2,1");
}

// ---------------------------------------------------- nested value copies
//
// design/0017 §4: the aggregate arms of `Value` share storage behind an
// `Arc` and copy on write. The sharing is invisible only if a write
// uniquifies **the whole path** it walks. These are the counterexamples the
// RFC names: take an inner aggregate out of an outer one, write to the
// inner, and the outer must not have moved. An implementation that copies
// the outermost node and then writes through a still-shared inner node
// passes `lists_are_values_so_a_binding_copies` above and fails here.

/// Length of the first element of a list of lists — a read, so it can never
/// be the thing that mutates.
const HEAD_LEN: &str = r#"fn head_len(xs: List<List<Int>>) -> Int {
    match xs.get(index: 0) {
        Some(head) => head.len(),
        None => 0,
    }
}
"#;

#[test]
fn writing_to_a_list_taken_out_of_a_list_leaves_the_outer_list_alone() {
    let source = format!(
        "{HEAD_LEN}
fn main(io: Io) -> Result<Unit, Error> uses {{Io.write}} {{
    var outer = [[1, 2]];
    match outer.get(index: 0) {{
        Some(head) => {{
            var inner = head;
            inner.push(item: 3);
            io.write(text: inner.len().to_text())?;
        }}
        None => {{}}
    }}
    io.write(text: \",\")?;
    io.write(text: head_len(xs: outer).to_text())?;
    return Ok(unit);
}}
"
    );
    // The taken copy grew to 3; the element still inside `outer` is 2.
    assert_eq!(stdout_of(&source), "3,2");
}

#[test]
fn the_same_holds_one_level_deeper() {
    let source = format!(
        "{HEAD_LEN}
fn main(io: Io) -> Result<Unit, Error> uses {{Io.write}} {{
    var outer = [[[1]]];
    match outer.get(index: 0) {{
        Some(taken) => {{
            var middle = taken;
            match middle.get(index: 0) {{
                Some(deep) => {{
                    var inner = deep;
                    inner.push(item: 2);
                    io.write(text: inner.len().to_text())?;
                }}
                None => {{}}
            }}
            io.write(text: \",\")?;
            io.write(text: head_len(xs: middle).to_text())?;
        }}
        None => {{}}
    }}
    return Ok(unit);
}}
"
    );
    // The innermost copy grew to 2; the list two levels up never moved.
    assert_eq!(stdout_of(&source), "2,1");
}

#[test]
fn writing_through_a_nested_struct_path_does_not_reach_the_other_copy() {
    // The write path descends binding -> struct -> struct -> list. Every
    // node on it must be uniquified before the next step, or `a` sees `b`'s
    // push.
    let source = r#"
struct Inner {
    var items: List<Int>,
}

struct Outer {
    var inner: Inner,
}

fn main(io: Io) -> Result<Unit, Error> uses {Io.write} {
    var a = Outer { inner: Inner { items: [1] } };
    var b = a;
    b.inner.items.push(item: 2);
    io.write(text: a.inner.items.join(sep: "-"))?;
    io.write(text: "|")?;
    io.write(text: b.inner.items.join(sep: "-"))?;
    return Ok(unit);
}
"#;
    assert_eq!(stdout_of(source), "1|1-2");
}

#[test]
fn writing_to_a_map_inside_a_struct_does_not_reach_the_other_copy() {
    let source = r#"
struct Ledger {
    var totals: Map<String, Int>,
}

fn main(io: Io) -> Result<Unit, Error> uses {Io.write} {
    var seed: Map<String, Int> = empty_map();
    seed.insert(key: "a", value: 1);
    var first = Ledger { totals: seed };
    var second = first;
    second.totals.insert(key: "b", value: 2);
    second.totals.remove(key: "a");
    io.write(text: first.totals.len().to_text())?;
    io.write(text: ",")?;
    io.write(text: second.totals.len().to_text())?;
    return Ok(unit);
}
"#;
    assert_eq!(stdout_of(source), "1,1");
}

#[test]
fn list_equality_is_structural() {
    let source = r#"
fn main(io: Io) -> Result<Unit, Error> uses {Io.write} {
    if [1, 2] == [1, 2] {
        io.write(text: "yes")?;
    } else {
        io.write(text: "no")?;
    }
    io.write(text: ",")?;
    if [1] == [2] {
        io.write(text: "yes")?;
    } else {
        io.write(text: "no")?;
    }
    return Ok(unit);
}
"#;
    assert_eq!(stdout_of(source), "yes,no");
}

#[test]
fn push_into_a_var_struct_field_runs() {
    let source = r#"
struct Deck {
    var cards: List<Int>,
}

fn main(io: Io) -> Result<Unit, Error> uses {Io.write} {
    var deck = Deck { cards: [1] };
    deck.cards.push(item: 2);
    io.write(text: deck.cards.join(sep: ","))?;
    return Ok(unit);
}
"#;
    assert_eq!(stdout_of(source), "1,2");
}

#[test]
fn the_iteration_idiom_while_len_get_runs() {
    // The idiom 0007 §2 fixes until an iteration RFC lands.
    let source = r#"
fn sum(xs: List<Int>) -> Int {
    var total = 0;
    var index = 0;
    while index < xs.len() {
        total = total + match xs.get(index: index) {
            Some(value) => value,
            None => 0,
        };
        index = index + 1;
    }
    total
}

fn main(io: Io) -> Result<Unit, Error> uses {Io.write} {
    io.write(text: sum(xs: [1, 2, 3, 4, 5]).to_text())?;
    return Ok(unit);
}
"#;
    assert_eq!(stdout_of(source), "15");
}

// ---------------------------------------------------------- strings (slice S)

#[test]
fn string_len_counts_unicode_scalars() {
    let source = r#"
fn main(io: Io) -> Result<Unit, Error> uses {Io.write} {
    io.write(text: "abc".len().to_text())?;
    io.write(text: ",")?;
    io.write(text: "あいう".len().to_text())?;
    io.write(text: ",")?;
    io.write(text: "".len().to_text())?;
    return Ok(unit);
}
"#;
    assert_eq!(stdout_of(source), "3,3,0");
}

#[test]
fn split_is_lossless_and_keeps_empty_pieces() {
    let source = r#"
fn main(io: Io) -> Result<Unit, Error> uses {Io.write} {
    let parts = "a,,b,".split(sep: ",");
    io.write(text: parts.len().to_text())?;
    io.write(text: "|")?;
    io.write(text: parts.join(sep: ","))?;
    io.write(text: "|")?;
    let single = "".split(sep: ",");
    io.write(text: single.len().to_text())?;
    io.write(text: single.join(sep: ","))?;
    io.write(text: "|")?;
    io.write(text: ",x,".split(sep: ",").join(sep: ","))?;
    return Ok(unit);
}
"#;
    assert_eq!(stdout_of(source), "4|a,,b,|1|,x,");
}

#[test]
fn split_with_an_empty_separator_is_per_scalar() {
    let source = r#"
fn main(io: Io) -> Result<Unit, Error> uses {Io.write} {
    let scalars = "あbc".split(sep: "");
    io.write(text: scalars.len().to_text())?;
    io.write(text: "|")?;
    io.write(text: scalars.join(sep: "-"))?;
    io.write(text: "|")?;
    io.write(text: scalars.join(sep: ""))?;
    io.write(text: "|")?;
    io.write(text: "".split(sep: "").len().to_text())?;
    return Ok(unit);
}
"#;
    assert_eq!(stdout_of(source), "3|あ-b-c|あbc|0");
}

#[test]
fn trim_strips_ascii_whitespace_only() {
    // The two wide characters around the second `x` are U+3000, which D2
    // deliberately leaves alone.
    let source = "fn main(io: Io) -> Result<Unit, Error> uses {Io.write} {\n    \
                      io.write(text: \" \\t x \\r\\n\".trim())?;\n    \
                      io.write(text: \"|\")?;\n    \
                      io.write(text: \" \\t \".trim().len().to_text())?;\n    \
                      io.write(text: \"|\")?;\n    \
                      io.write(text: \"\".trim().len().to_text())?;\n    \
                      io.write(text: \"|\")?;\n    \
                      let wide = \"　x　\";\n    \
                      if wide.trim() == wide {\n        io.write(text: \"kept\")?;\n    } else {\n        \
                          io.write(text: \"stripped\")?;\n    }\n    \
                      return Ok(unit);\n}";
    assert_eq!(stdout_of(source), "x|0|0|kept");
}

#[test]
fn try_to_int_accepts_signed_decimals_and_rejects_the_rest() {
    let source = r#"
fn describe(s: String) -> String {
    match s.try_to_int() {
        Ok(value) => value.to_text(),
        Err(_) => "err",
    }
}

fn main(io: Io) -> Result<Unit, Error> uses {Io.write} {
    let cases = ["42", " +7 ", "-0", "", "x", "1_000", "1.5", "99999999999999999999"];
    var out: List<String> = [];
    var index = 0;
    while index < cases.len() {
        let case = match cases.get(index: index) {
            Some(text) => text,
            None => "",
        };
        out.push(item: describe(s: case));
        index = index + 1;
    }
    io.write(text: out.join(sep: ","))?;
    return Ok(unit);
}
"#;
    assert_eq!(stdout_of(source), "42,7,0,err,err,err,err,err");
}

#[test]
fn starts_with_and_contains_handle_their_edges() {
    let source = r#"
fn tick(flag: Bool) -> String {
    if flag {
        "y"
    } else {
        "n"
    }
}

fn main(io: Io) -> Result<Unit, Error> uses {Io.write} {
    let answers = [
        tick(flag: "hello".starts_with(prefix: "he")),
        tick(flag: "hello".starts_with(prefix: "")),
        tick(flag: "hello".starts_with(prefix: "el")),
        tick(flag: "あいう".contains(sub: "い")),
        tick(flag: "hello".contains(sub: "")),
        tick(flag: "hello".contains(sub: "lo!")),
    ];
    io.write(text: answers.join(sep: ""))?;
    return Ok(unit);
}
"#;
    assert_eq!(stdout_of(source), "yynyyn");
}

#[test]
fn the_split_trim_parse_sum_idiom_runs() {
    // The t4-02 shape (0007 §5-4): split, trim, parse, skip failures, sum.
    let source = r#"
fn main(io: Io) -> Result<Unit, Error> uses {Io.write} {
    let parts = "12, 34, x, 56".split(sep: ",");
    var total = 0;
    var index = 0;
    while index < parts.len() {
        let piece = match parts.get(index: index) {
            Some(text) => text,
            None => "",
        };
        total = total + match piece.trim().try_to_int() {
            Ok(value) => value,
            Err(_) => 0,
        };
        index = index + 1;
    }
    io.write(text: total.to_text())?;
    return Ok(unit);
}
"#;
    assert_eq!(stdout_of(source), "102");
}

// ---------------------------------------------------------------------- maps

#[test]
fn map_insertion_order_is_normative() {
    // Update keeps the position, remove shifts, re-insert lands at the end
    // (0007 §3).
    let source = r#"
fn main(io: Io) -> Result<Unit, Error> uses {Io.write} {
    var m: Map<String, Int> = empty_map();
    m.insert(key: "a", value: 1);
    m.insert(key: "b", value: 2);
    m.insert(key: "c", value: 3);
    m.insert(key: "a", value: 10);
    m.remove(key: "b");
    m.insert(key: "b", value: 20);
    io.write(text: m.keys().join(sep: ","))?;
    io.write(text: "|")?;
    io.write(text: m.len().to_text())?;
    io.write(text: "|")?;
    let found = match m.get(key: "a") {
        Some(value) => value,
        None => -1,
    };
    io.write(text: found.to_text())?;
    return Ok(unit);
}
"#;
    assert_eq!(stdout_of(source), "a,c,b|3|10");
}

#[test]
fn insert_returns_the_old_value() {
    let source = r#"
fn describe(previous: Option<Int>) -> String {
    match previous {
        Some(value) => value.to_text(),
        None => "none",
    }
}

fn main(io: Io) -> Result<Unit, Error> uses {Io.write} {
    var m: Map<String, Int> = empty_map();
    io.write(text: describe(previous: m.insert(key: "a", value: 1)))?;
    io.write(text: ",")?;
    io.write(text: describe(previous: m.insert(key: "a", value: 2)))?;
    return Ok(unit);
}
"#;
    assert_eq!(stdout_of(source), "none,1");
}

#[test]
fn map_equality_ignores_insertion_order() {
    let source = r#"
fn main(io: Io) -> Result<Unit, Error> uses {Io.write} {
    var first: Map<String, Int> = empty_map();
    first.insert(key: "a", value: 1);
    first.insert(key: "b", value: 2);
    var second: Map<String, Int> = empty_map();
    second.insert(key: "b", value: 2);
    second.insert(key: "a", value: 1);
    var third: Map<String, Int> = empty_map();
    third.insert(key: "a", value: 1);
    third.insert(key: "b", value: 3);
    if first == second {
        io.write(text: "yes")?;
    } else {
        io.write(text: "no")?;
    }
    io.write(text: ",")?;
    if first == third {
        io.write(text: "yes")?;
    } else {
        io.write(text: "no")?;
    }
    return Ok(unit);
}
"#;
    assert_eq!(stdout_of(source), "yes,no");
}

#[test]
fn an_empty_map_answers_all_reads() {
    let source = r#"
fn main(io: Io) -> Result<Unit, Error> uses {Io.write} {
    var m: Map<String, Int> = empty_map();
    io.write(text: m.len().to_text())?;
    io.write(text: ",")?;
    if m.is_empty() {
        io.write(text: "empty")?;
    } else {
        io.write(text: "full")?;
    }
    io.write(text: ",")?;
    let got = match m.get(key: "x") {
        Some(_) => "some",
        None => "none",
    };
    io.write(text: got)?;
    io.write(text: ",")?;
    let removed = match m.remove(key: "x") {
        Some(_) => "some",
        None => "none",
    };
    io.write(text: removed)?;
    io.write(text: ",")?;
    if m.has_key(key: "x") {
        io.write(text: "has")?;
    } else {
        io.write(text: "lacks")?;
    }
    return Ok(unit);
}
"#;
    assert_eq!(stdout_of(source), "0,empty,none,none,lacks");
}

#[test]
fn keys_is_a_snapshot_not_a_view() {
    let source = r#"
fn main(io: Io) -> Result<Unit, Error> uses {Io.write} {
    var m: Map<String, Int> = empty_map();
    m.insert(key: "a", value: 1);
    let snapshot = m.keys();
    m.insert(key: "b", value: 2);
    io.write(text: snapshot.len().to_text())?;
    io.write(text: ",")?;
    io.write(text: m.keys().len().to_text())?;
    return Ok(unit);
}
"#;
    assert_eq!(stdout_of(source), "1,2");
}

#[test]
fn a_map_renders_deterministically_in_join() {
    let source = r#"
fn main(io: Io) -> Result<Unit, Error> uses {Io.write} {
    var m: Map<String, Int> = empty_map();
    m.insert(key: "a", value: 1);
    m.insert(key: "b", value: 2);
    io.write(text: [m].join(sep: ""))?;
    return Ok(unit);
}
"#;
    assert_eq!(stdout_of(source), "{a: 1, b: 2}");
}

// --------------------------------------------------- closures (design/0014)

#[test]
fn map_transforms_left_to_right() {
    assert_eq!(
        stdout_of(&printing_text("[1, 2, 3].map(|x| x * 2).join(sep: \",\")")),
        "2,4,6"
    );
}

#[test]
fn filter_keeps_the_hits_in_order() {
    assert_eq!(
        stdout_of(&printing_text(
            "[1, 2, 3, 4].filter(|x| x % 2 == 0).join(sep: \",\")"
        )),
        "2,4"
    );
}

#[test]
fn fold_is_a_left_fold() {
    assert_eq!(
        stdout_of(&printing("[1, 2, 3, 4].fold(init: 0, f: |acc, x| acc + x)")),
        "10"
    );
    // Left-to-right, observable through string building: 1 then 2 then 3.
    let source = "fn main(io: Io) -> Result<Unit, Error> uses {Io.write} {\n    \
                      let text = [1, 2, 3].fold(init: \"\", f: |acc, x| acc.concat(other: x.to_text()));\n    \
                      io.write(text: text)?;\n    return Ok(unit);\n}";
    assert_eq!(stdout_of(source), "123");
}

#[test]
fn find_returns_the_first_hit_and_short_circuits() {
    let source = "fn main(io: Io) -> Result<Unit, Error> uses {Io.write} {\n    \
                      match [1, 0, 2].find(|x| 10 / x > 4) {\n        \
                          Some(hit) => io.write(text: hit.to_text())?,\n        \
                          None => io.write(text: \"none\")?,\n    }\n    \
                      return Ok(unit);\n}";
    // The first element already matches; if `find` kept going, `10 / 0`
    // would trap — short-circuiting is the contract, not an optimisation.
    assert_eq!(stdout_of(source), "1");

    let source = "fn main(io: Io) -> Result<Unit, Error> uses {Io.write} {\n    \
                      match [1, 2].find(|x| x > 9) {\n        \
                          Some(hit) => io.write(text: hit.to_text())?,\n        \
                          None => io.write(text: \"none\")?,\n    }\n    \
                      return Ok(unit);\n}";
    assert_eq!(stdout_of(source), "none");
}

#[test]
fn a_capture_is_a_creation_time_copy() {
    assert_eq!(
        stdout_of(&printing_text(
            "{ let base = 10; [1, 2].map(|x| x + base).join(sep: \",\") }"
        )),
        "11,12"
    );
}

#[test]
fn a_discarded_parameter_still_consumes_its_argument() {
    assert_eq!(
        stdout_of(&printing_text("[1, 2, 3].map(|_| 7).join(sep: \",\")")),
        "7,7,7"
    );
}

#[test]
fn nested_closures_run_inside_out() {
    assert_eq!(
        stdout_of(&printing_text(
            "[[1, 2], [3]].map(|xs| xs.fold(init: 0, f: |acc, x| acc + x)).join(sep: \",\")"
        )),
        "3,3"
    );
}

#[test]
fn the_receiver_is_not_written_by_a_combinator() {
    let source = "fn main(io: Io) -> Result<Unit, Error> uses {Io.write} {\n    \
                      let xs = [1, 2, 3];\n    \
                      let ys = xs.filter(|x| x > 1);\n    \
                      io.write(text: xs.join(sep: \"\"))?;\n    \
                      io.write(text: \"/\")?;\n    \
                      io.write(text: ys.join(sep: \"\"))?;\n    \
                      return Ok(unit);\n}";
    assert_eq!(stdout_of(source), "123/23");
}

#[test]
fn a_trap_inside_a_closure_carries_its_span() {
    let message = trap_of(&printing_text("[1, 0].map(|x| 10 / x).join(sep: \",\")"));
    assert!(message.contains("division by zero"), "{message}");
}

#[test]
fn a_hole_inside_a_closure_body_traps_precisely() {
    // Holes type-check clean and run until reached — inside a closure too.
    let source = "fn main(io: Io) -> Result<Unit, Error> uses {Io.write} {\n    \
                      let ys = [1].filter(|x| ??keep);\n    \
                      io.write(text: ys.join(sep: \"\"))?;\n    return Ok(unit);\n}";
    let parsed = parse(source);
    assert!(parsed.diagnostics.is_empty());
    let analysis = xenith_sema::analyze(&parsed.module);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );
    let (table, _) = def::collect(&parsed.module);
    let outcome = run(&parsed.module, &table);
    assert_eq!(outcome.exit, 101);
    assert!(outcome.error.expect("trap").0.contains("??keep"));
}

// ------------------------------------------------- unshipped constructs

#[test]
fn a_misplaced_closure_is_refused_at_check_not_run() {
    // design/0014 §3: a closure is written only as a call argument, so a
    // `let`-bound one never reaches the interpreter.
    let parsed = parse("fn main() -> Int {\n    let add = |n| n + 1;\n    add(5)\n}");
    assert!(parsed.diagnostics.is_empty(), "the parser still accepts it");
    let analysis = xenith_sema::analyze(&parsed.module);
    assert!(
        analysis.diagnostics.iter().any(|d| d.code.id() == "XN1011"),
        "{:?}",
        analysis.diagnostics
    );
}

#[test]
fn async_fn_and_await_are_refused_at_check_not_run() {
    let source = "async fn compute() -> Int {\n    40 + 2\n}\n\n\
                  fn main(io: Io) -> Result<Unit, Error> uses {Io.write} {\n    \
                      let value = compute().await;\n    \
                      io.write(text: value.to_text())?;\n    return Ok(unit);\n}";
    let parsed = parse(source);
    assert!(parsed.diagnostics.is_empty(), "the parser still accepts it");
    let analysis = xenith_sema::analyze(&parsed.module);
    let unshipped = analysis
        .diagnostics
        .iter()
        .filter(|d| d.code.id() == "XN1008")
        .count();
    assert_eq!(unshipped, 2, "one for `async fn`, one for `.await`");
}

#[test]
fn a_non_exhaustive_match_is_refused_at_check_not_trapped() {
    // Before XN5001 this program ran and trapped on the missing arm; now it
    // never reaches the interpreter.
    let source = "fn main() -> Int {\n    let o: Option<Int> = Some(1);\n    \
                  match o {\n        Some(value) => value,\n    }\n}";
    let parsed = parse(source);
    assert!(parsed.diagnostics.is_empty());
    let analysis = xenith_sema::analyze(&parsed.module);
    assert!(
        analysis.diagnostics.iter().any(|d| d.code.id() == "XN5001"),
        "{:?}",
        analysis.diagnostics
    );
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
