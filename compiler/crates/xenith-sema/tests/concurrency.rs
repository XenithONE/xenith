//! Conformance suite for design/0015 — task boundaries v1 (checking level).
//!
//! Written before the implementation (design/0015 §7: semantics first, code
//! after), so every case here is the normative reading of the RFC, not a
//! description of whatever the compiler happened to do.
//!
//! The shape of v1: `spawn f(args)` is legal only inside `scope { .. }`; the
//! callee is a named fn with an empty `uses` set and CaptureSafe parameters;
//! the child runs to completion at the spawn point; `Join<T>` is a
//! compiler-tracked consuming handle — bound by a bare `let`, consumed by
//! `.await` exactly once, and usable for nothing else. Execution-order cases
//! live in the CLI half of the suite (`xenith-cli/tests/concurrency.rs`).

use xenith_sema::analyze;
use xenith_syntax::parse;

/// Parse-clean sources only: the sema-side codes in source order.
fn codes_of(source: &str) -> Vec<String> {
    let parsed = parse(source);
    assert!(
        parsed.diagnostics.is_empty(),
        "test source must parse cleanly first: {:?}",
        parsed.diagnostics
    );
    analyze(&parsed.module)
        .diagnostics
        .iter()
        .map(|d| d.code.id().to_string())
        .collect()
}

fn diagnostics_of(source: &str) -> Vec<(String, String)> {
    let parsed = parse(source);
    assert!(
        parsed.diagnostics.is_empty(),
        "test source must parse cleanly first: {:?}",
        parsed.diagnostics
    );
    analyze(&parsed.module)
        .diagnostics
        .iter()
        .map(|d| (d.code.id().to_string(), d.message.clone()))
        .collect()
}

/// Parser and sema codes together, for the forms the parser itself refuses.
fn all_codes_of(source: &str) -> Vec<String> {
    let parsed = parse(source);
    let mut codes: Vec<String> = parsed
        .diagnostics
        .iter()
        .map(|d| d.code.id().to_string())
        .collect();
    codes.extend(
        analyze(&parsed.module)
            .diagnostics
            .iter()
            .map(|d| d.code.id().to_string()),
    );
    codes
}

fn expect_clean(source: &str) {
    let found = diagnostics_of(source);
    assert!(found.is_empty(), "expected no diagnostics, got {found:#?}");
}

/// The canonical teach (design/0015 §6): every taught task diagnostic
/// converges on this one sentence.
const TASK_TEACH: &str = "a task computes a plan — effects run in the parent, after await";

// ===================================================================
// POSITIVE — programs the RFC blesses must check cleanly
// ===================================================================

#[test]
fn two_child_fan_out_with_pure_workers_checks_cleanly() {
    expect_clean(
        r#"
fn square(n: Int) -> Int {
    n * n
}

fn cube(n: Int) -> Int {
    n * n * n
}

fn combine() -> Int uses {Task.spawn} {
    scope {
        let a = spawn square(n: 4);
        let b = spawn cube(n: 3);
        a.await + b.await
    }
}
"#,
    );
}

#[test]
fn a_result_returning_child_propagates_through_await_try() {
    // The child may fail purely (design/0015 §4): it returns `Result`, and
    // the parent propagates with `j.await?`.
    expect_clean(
        r#"
fn plan(n: Int) -> Result<Int, Error> {
    if n < 0 {
        Err(??)
    } else {
        Ok(n * 2)
    }
}

fn run() -> Result<Int, Error> uses {Task.spawn} {
    scope {
        let j = spawn plan(n: 21);
        let v = j.await?;
        return Ok(v);
    }
}
"#,
    );
}

#[test]
fn nested_scopes_check_cleanly_and_each_scope_owns_its_joins() {
    expect_clean(
        r#"
fn work(n: Int) -> Int {
    n + 1
}

fn nest() -> Int uses {Task.spawn} {
    scope {
        let outer = spawn work(n: 1);
        let inner_total = scope {
            let inner = spawn work(n: 2);
            inner.await
        };
        outer.await + inner_total
    }
}
"#,
    );
}

#[test]
fn awaiting_an_outer_join_inside_a_nested_scope_is_still_exactly_once() {
    // Ownership stays with the creating scope; a nested scope block is just
    // a block on the path, and the await still happens exactly once.
    expect_clean(
        r#"
fn work(n: Int) -> Int {
    n + 1
}

fn nest() -> Int uses {Task.spawn} {
    scope {
        let outer = spawn work(n: 1);
        scope {
            outer.await
        }
    }
}
"#,
    );
}

#[test]
fn statement_form_spawn_of_a_unit_callee_checks_cleanly() {
    expect_clean(
        r#"
fn ping() {
    let x = 1;
}

fn fire() uses {Task.spawn} {
    scope {
        spawn ping();
    }
}
"#,
    );
}

#[test]
fn a_scope_block_is_an_expression_with_a_tail_value() {
    expect_clean(
        r#"
fn f() -> Int uses {Task.spawn} {
    let x = scope { 41 + 1 };
    x
}
"#,
    );
}

#[test]
fn spawn_may_sit_in_a_nested_block_inside_the_scope() {
    expect_clean(
        r#"
fn work(n: Int) -> Int {
    n
}

fn f(flag: Bool) -> Int uses {Task.spawn} {
    scope {
        if flag {
            let j = spawn work(n: 1);
            j.await
        } else {
            0
        }
    }
}
"#,
    );
}

#[test]
fn an_early_question_mark_exit_may_discard_a_live_join() {
    // design/0015 §3: on early exit the completed, unconsumed result is
    // discarded — the child was pure, so nothing observable is lost. Only
    // the normal-exit path must await.
    expect_clean(
        r#"
fn work(n: Int) -> Int {
    n
}

fn gate(flag: Bool) -> Result<Int, Error> {
    if flag { Ok(1) } else { Err(??) }
}

fn f(flag: Bool) -> Result<Int, Error> uses {Task.spawn} {
    scope {
        let j = spawn work(n: 1);
        let g = gate(flag: flag)?;
        return Ok(j.await + g);
    }
}
"#,
    );
}

#[test]
fn a_return_before_the_await_is_an_early_exit_and_discards_the_join() {
    expect_clean(
        r#"
fn work(n: Int) -> Int {
    n
}

fn f(flag: Bool) -> Int uses {Task.spawn} {
    scope {
        let j = spawn work(n: 1);
        if flag {
            return 0;
        }
        j.await
    }
}
"#,
    );
}

#[test]
fn a_unit_join_left_unawaited_is_not_an_error() {
    // XN6008 refuses silenced *results*; a Unit child has none to silence.
    // (The canonical spelling is the statement form; the binding is legal.)
    expect_clean(
        r#"
fn ping() {
    let x = 1;
}

fn f() uses {Task.spawn} {
    scope {
        let j = spawn ping();
    }
}
"#,
    );
}

#[test]
fn declared_but_unused_task_spawn_stays_inert_and_compiles() {
    // Backward compatibility (design/0015 §5): the open effect namespace
    // accepted `uses {Task.spawn}` before this RFC gave it meaning, and a
    // program that declares it without spawning must keep compiling.
    expect_clean(
        r#"
fn quiet() -> Int uses {Task.spawn} {
    7
}
"#,
    );
}

#[test]
fn spawn_and_scope_stay_ordinary_identifiers_away_from_their_forms() {
    // Contextual keywords: `spawn x` (identifier follows) and `scope {`
    // are claimed; everything else keeps its pre-0015 reading.
    expect_clean(
        r#"
fn f() -> Int {
    let spawn = 1;
    let scope = 2;
    let both = spawn + scope;
    if scope > 0 { both } else { spawn }
}
"#,
    );
}

#[test]
fn a_generic_pure_callee_spawns_when_its_arguments_are_capture_safe() {
    expect_clean(
        r#"
fn first<T>(items: List<T>) -> Option<T> {
    items.get(index: 0)
}

fn f() -> Option<Int> uses {Task.spawn} {
    scope {
        let j = spawn first(items: [1, 2, 3]);
        j.await
    }
}
"#,
    );
}

// ===================================================================
// REJECTION — every §6 diagnostic
// ===================================================================

// ----- XN6001: spawn outside scope -----

#[test]
fn spawn_outside_any_scope_block_is_xn6001() {
    let source = r#"
fn work(n: Int) -> Int {
    n
}

fn f() -> Int uses {Task.spawn} {
    let j = spawn work(n: 1);
    j.await
}
"#;
    let codes = codes_of(source);
    assert_eq!(codes, ["XN6001"], "one structural mistake, one diagnostic");
}

#[test]
fn statement_form_spawn_outside_scope_is_also_xn6001() {
    let source = r#"
fn ping() {
    let x = 1;
}

fn f() uses {Task.spawn} {
    spawn ping();
}
"#;
    assert_eq!(codes_of(source), ["XN6001"]);
}

// ----- XN4001: spawn requires the Task.spawn effect -----

#[test]
fn spawn_without_task_spawn_in_uses_is_xn4001_with_the_uses_fix() {
    let source = r#"
fn work(n: Int) -> Int {
    n
}

fn f() -> Int {
    scope {
        let j = spawn work(n: 1);
        j.await
    }
}
"#;
    let parsed = parse(source);
    assert!(parsed.diagnostics.is_empty());
    let analysis = analyze(&parsed.module);
    let codes: Vec<&str> = analysis.diagnostics.iter().map(|d| d.code.id()).collect();
    assert_eq!(codes, ["XN4001"]);
    assert!(
        analysis.diagnostics[0].message.contains("Task.spawn"),
        "{}",
        analysis.diagnostics[0].message
    );
    let fix = analysis.diagnostics[0].fix.as_ref().expect("the uses fix");
    assert!(
        fix.edits[0].replacement.contains("Task.spawn"),
        "the fix inserts the effect: {fix:?}"
    );
}

// ----- XN6002: callee with a non-empty uses set -----

#[test]
fn spawning_an_effectful_callee_is_xn6002_with_the_canonical_teach() {
    let source = r#"
fn shout(io: Io, text: String) -> Result<Unit, Error> uses {Io.write} {
    io.write(text: text)
}

fn f(io: Io) uses {Task.spawn} {
    scope {
        spawn shout(io: io, text: "hi");
    }
}
"#;
    let found = diagnostics_of(source);
    let (code, message) = &found[0];
    assert_eq!(code, "XN6002");
    assert!(message.contains("Io.write"), "{message}");
    assert!(
        message.contains(TASK_TEACH),
        "the teach note converges on the canonical sentence: {message}"
    );
    // The child's effects are refused once, at the boundary — not charged a
    // second time to the parent as XN4001.
    assert!(
        !found.iter().any(|(c, _)| c == "XN4001"),
        "one mistake, one diagnostic: {found:#?}"
    );
}

// ----- XN6003: non-CaptureSafe argument (a capability included) -----

#[test]
fn passing_a_capability_to_a_child_is_xn6003() {
    let source = r#"
fn use_io(io: Io) -> Int {
    1
}

fn f(io: Io) uses {Task.spawn} {
    scope {
        spawn use_io(io: io);
    }
}
"#;
    let found = diagnostics_of(source);
    let (code, message) = &found[0];
    assert_eq!(code, "XN6003");
    assert!(message.contains("Io"), "{message}");
    assert!(message.contains("CaptureSafe"), "{message}");
    assert!(message.contains(TASK_TEACH), "{message}");
}

#[test]
fn an_unbounded_type_parameter_argument_is_not_capture_safe_either() {
    // The 0014 inductive rule, reused verbatim: an unresolved parameter has
    // nothing to promise safety with.
    let source = r#"
fn hold<T>(item: T) -> Int {
    1
}

fn f<T>(item: T) -> Int uses {Task.spawn} {
    scope {
        let j = spawn hold(item: item);
        j.await
    }
}
"#;
    let codes = codes_of(source);
    assert!(codes.contains(&"XN6003".to_string()), "{codes:?}");
}

// ----- XN6004: method / computed / non-fn callee -----

#[test]
fn spawning_a_method_call_is_xn6004() {
    let source = r#"
fn f(xs: List<Int>) uses {Task.spawn} {
    scope {
        spawn xs.get(index: 0);
    }
}
"#;
    let codes = codes_of(source);
    assert_eq!(codes, ["XN6004"]);
}

#[test]
fn spawning_a_computed_callee_is_xn6004_at_parse_level() {
    let source = r#"
fn f(g: Int) uses {Task.spawn} {
    scope {
        spawn (g)(x: 1);
    }
}
"#;
    let codes = all_codes_of(source);
    assert!(codes.contains(&"XN6004".to_string()), "{codes:?}");
    assert!(
        !codes.contains(&"XN2002".to_string()),
        "recovery must not invent an unknown `spawn` name: {codes:?}"
    );
}

#[test]
fn spawning_an_enum_constructor_is_xn6004() {
    let source = r#"
enum Job {
    Ready(Int),
}

fn f() uses {Task.spawn} {
    scope {
        spawn Job.Ready(1);
    }
}
"#;
    let codes = codes_of(source);
    assert_eq!(codes, ["XN6004"]);
}

#[test]
fn spawning_a_local_value_is_xn6004() {
    let source = r#"
fn f(worker: Int) uses {Task.spawn} {
    scope {
        spawn worker(x: 1);
    }
}
"#;
    let codes = codes_of(source);
    assert_eq!(codes, ["XN6004"]);
}

// ----- XN6005: the Join escapes -----

#[test]
fn a_join_stored_in_a_container_is_xn6005() {
    let source = r#"
fn work(n: Int) -> Int {
    n
}

fn f() uses {Task.spawn} {
    scope {
        let j = spawn work(n: 1);
        let xs = [j];
    }
}
"#;
    let codes = codes_of(source);
    assert!(codes.contains(&"XN6005".to_string()), "{codes:?}");
    assert!(
        !codes.contains(&"XN6008".to_string()),
        "an escape poisons the handle; it does not also report unawaited: {codes:?}"
    );
}

#[test]
fn a_join_returned_from_the_function_is_xn6005() {
    let source = r#"
fn work(n: Int) -> Int {
    n
}

fn f() -> Int uses {Task.spawn} {
    scope {
        let j = spawn work(n: 1);
        return j;
    }
}
"#;
    let codes = codes_of(source);
    assert!(codes.contains(&"XN6005".to_string()), "{codes:?}");
}

#[test]
fn a_join_passed_as_an_argument_is_xn6005() {
    let source = r#"
fn work(n: Int) -> Int {
    n
}

fn take(n: Int) -> Int {
    n
}

fn f() uses {Task.spawn} {
    scope {
        let j = spawn work(n: 1);
        let x = take(n: j);
    }
}
"#;
    let codes = codes_of(source);
    assert!(codes.contains(&"XN6005".to_string()), "{codes:?}");
}

#[test]
fn a_join_captured_by_a_closure_is_xn6005() {
    let source = r#"
fn work(n: Int) -> Int {
    n
}

fn f(xs: List<Int>) uses {Task.spawn} {
    scope {
        let j = spawn work(n: 1);
        let ys = xs.map(|x| j);
    }
}
"#;
    let codes = codes_of(source);
    assert!(codes.contains(&"XN6005".to_string()), "{codes:?}");
}

#[test]
fn rebinding_a_join_with_let_is_the_dedicated_no_copy_diagnostic() {
    // Join is outside the D1 copy regime (design/0015 §4): `let b = a` of a
    // Join is refused by a dedicated diagnostic, not smuggled through as a
    // value copy that would let two handles await one child.
    let source = r#"
fn work(n: Int) -> Int {
    n
}

fn f() -> Int uses {Task.spawn} {
    scope {
        let a = spawn work(n: 1);
        let b = a;
        b.await
    }
}
"#;
    let found = diagnostics_of(source);
    assert_eq!(found[0].0, "XN6005", "{found:#?}");
    assert!(found[0].1.contains(TASK_TEACH), "{}", found[0].1);
}

#[test]
fn a_spawn_in_plain_expression_position_is_xn6005() {
    let source = r#"
fn work(n: Int) -> Int {
    n
}

fn f() -> Int uses {Task.spawn} {
    scope {
        1 + spawn work(n: 1)
    }
}
"#;
    let codes = codes_of(source);
    assert!(codes.contains(&"XN6005".to_string()), "{codes:?}");
}

#[test]
fn inline_await_of_a_spawn_expression_is_xn6005() {
    // One canonical spelling: bind, then await. `spawn f(x).await` would be
    // a second spelling of the same program.
    let source = r#"
fn work(n: Int) -> Int {
    n
}

fn f() -> Int uses {Task.spawn} {
    scope {
        spawn work(n: 1).await
    }
}
"#;
    let codes = codes_of(source);
    assert!(codes.contains(&"XN6005".to_string()), "{codes:?}");
}

#[test]
fn a_var_bound_spawn_is_xn6005() {
    let source = r#"
fn work(n: Int) -> Int {
    n
}

fn f() -> Int uses {Task.spawn} {
    scope {
        var j = spawn work(n: 1);
        j.await
    }
}
"#;
    let codes = codes_of(source);
    assert!(codes.contains(&"XN6005".to_string()), "{codes:?}");
}

#[test]
fn an_annotated_spawn_binding_is_xn6005() {
    // Join<T> is not a written type; there is nothing true to annotate.
    let source = r#"
fn work(n: Int) -> Int {
    n
}

fn f() -> Int uses {Task.spawn} {
    scope {
        let j: Int = spawn work(n: 1);
        j.await
    }
}
"#;
    let codes = codes_of(source);
    assert!(codes.contains(&"XN6005".to_string()), "{codes:?}");
}

#[test]
fn a_wildcard_spawn_binding_is_xn6005() {
    let source = r#"
fn work(n: Int) -> Int {
    n
}

fn f() uses {Task.spawn} {
    scope {
        let _ = spawn work(n: 1);
    }
}
"#;
    let codes = codes_of(source);
    assert!(codes.contains(&"XN6005".to_string()), "{codes:?}");
}

// ----- XN6006: awaited more than once -----

#[test]
fn a_second_await_is_xn6006() {
    let source = r#"
fn work(n: Int) -> Int {
    n
}

fn f() -> Int uses {Task.spawn} {
    scope {
        let j = spawn work(n: 1);
        let a = j.await;
        let b = j.await;
        a + b
    }
}
"#;
    let codes = codes_of(source);
    assert_eq!(codes, ["XN6006"], "the second await is the mistake");
}

#[test]
fn awaiting_inside_a_loop_a_join_created_outside_it_is_xn6006() {
    let source = r#"
fn work(n: Int) -> Int {
    n
}

fn f() -> Int uses {Task.spawn} {
    scope {
        let j = spawn work(n: 1);
        var total = 0;
        var i = 0;
        while i < 3 {
            total = total + j.await;
            i = i + 1;
        }
        total
    }
}
"#;
    let codes = codes_of(source);
    assert!(codes.contains(&"XN6006".to_string()), "{codes:?}");
}

// ----- XN6007: awaited on one branch but not the other -----

#[test]
fn a_branch_partial_await_is_xn6007() {
    let source = r#"
fn work(n: Int) -> Int {
    n
}

fn f(flag: Bool) -> Int uses {Task.spawn} {
    scope {
        let j = spawn work(n: 1);
        var total = 0;
        if flag {
            total = j.await;
        } else {
            total = 0;
        }
        total
    }
}
"#;
    let codes = codes_of(source);
    assert!(codes.contains(&"XN6007".to_string()), "{codes:?}");
    assert!(
        !codes.contains(&"XN6008".to_string()),
        "the partial await is reported at the branch, not again at exit: {codes:?}"
    );
}

#[test]
fn an_if_without_else_that_awaits_is_also_branch_partial() {
    let source = r#"
fn work(n: Int) -> Int {
    n
}

fn f(flag: Bool) uses {Task.spawn} {
    scope {
        let j = spawn work(n: 1);
        var total = 0;
        if flag {
            total = j.await;
        }
    }
}
"#;
    let codes = codes_of(source);
    assert!(codes.contains(&"XN6007".to_string()), "{codes:?}");
}

#[test]
fn match_arms_must_agree_on_consumption() {
    let source = r#"
fn work(n: Int) -> Int {
    n
}

fn f(n: Int) -> Int uses {Task.spawn} {
    scope {
        let j = spawn work(n: 1);
        match n {
            0 => j.await,
            _ => 0,
        }
    }
}
"#;
    let codes = codes_of(source);
    assert!(codes.contains(&"XN6007".to_string()), "{codes:?}");
}

#[test]
fn awaiting_on_every_branch_is_exactly_once_and_clean() {
    expect_clean(
        r#"
fn work(n: Int) -> Int {
    n
}

fn f(flag: Bool) -> Int uses {Task.spawn} {
    scope {
        let j = spawn work(n: 1);
        if flag {
            j.await
        } else {
            j.await + 1
        }
    }
}
"#,
    );
}

// ----- XN6008: a non-Unit Join silently dropped on normal exit -----

#[test]
fn an_unawaited_non_unit_join_at_normal_exit_is_xn6008() {
    let source = r#"
fn work(n: Int) -> Int {
    n
}

fn f() uses {Task.spawn} {
    scope {
        let j = spawn work(n: 1);
    }
}
"#;
    let codes = codes_of(source);
    assert_eq!(codes, ["XN6008"]);
}

// ----- XN6009: statement-form spawn of a non-Unit callee -----

#[test]
fn statement_form_spawn_of_a_non_unit_callee_is_xn6009() {
    let source = r#"
fn work(n: Int) -> Int {
    n
}

fn f() uses {Task.spawn} {
    scope {
        spawn work(n: 1);
    }
}
"#;
    let found = diagnostics_of(source);
    assert_eq!(found[0].0, "XN6009", "{found:#?}");
    assert!(found[0].1.contains(TASK_TEACH), "{}", found[0].1);
}

// ----- XN6010: spawn / scope / .await inside a closure body -----

#[test]
fn spawn_inside_a_closure_body_is_xn6010() {
    let source = r#"
fn work(n: Int) -> Int {
    n
}

fn f(xs: List<Int>) uses {Task.spawn} {
    scope {
        let ys = xs.map(|x| spawn work(n: x));
    }
}
"#;
    let found = diagnostics_of(source);
    assert_eq!(found[0].0, "XN6010", "{found:#?}");
    assert!(found[0].1.contains(TASK_TEACH), "{}", found[0].1);
}

#[test]
fn scope_inside_a_closure_body_is_xn6010() {
    let source = r#"
fn f(xs: List<Int>) uses {Task.spawn} {
    let ys = xs.map(|x| scope { x });
}
"#;
    let codes = codes_of(source);
    assert!(codes.contains(&"XN6010".to_string()), "{codes:?}");
}

#[test]
fn await_inside_a_closure_body_is_xn6010() {
    let source = r#"
fn work(n: Int) -> Int {
    n
}

fn f(xs: List<Int>) -> Int uses {Task.spawn} {
    scope {
        let j = spawn work(n: 1);
        let ys = xs.map(|x| x + j.await);
        j.await
    }
}
"#;
    let codes = codes_of(source);
    assert!(codes.contains(&"XN6010".to_string()), "{codes:?}");
}

// ----- the XN1008 carve-out: .await elsewhere stays banned -----

#[test]
fn await_on_a_non_join_expression_stays_xn1008() {
    let source = "fn g() -> Int { 1 }\nfn f() -> Int { g().await }";
    assert_eq!(codes_of(source), ["XN1008"]);
}

#[test]
fn await_on_an_ordinary_binding_stays_xn1008() {
    let source = "fn f(n: Int) -> Int { n.await }";
    assert_eq!(codes_of(source), ["XN1008"]);
}

#[test]
fn async_fn_stays_refused() {
    assert_eq!(codes_of("async fn g() -> Int { 1 }"), ["XN1008"]);
}

// ===================================================================
// Diagnostic hygiene
// ===================================================================

#[test]
fn every_new_code_has_an_explanation() {
    for id in [
        "XN6001", "XN6002", "XN6003", "XN6004", "XN6005", "XN6006", "XN6007", "XN6008", "XN6009",
        "XN6010",
    ] {
        let code = xenith_diag::DiagCode::from_id(id)
            .unwrap_or_else(|| panic!("{id} must exist as a stable code"));
        assert!(
            code.explain().len() > 40,
            "{id} needs a real explanation for `xenith explain`"
        );
    }
}

#[test]
fn the_teach_notes_strip_to_byte_identity() {
    // --diagnostic-teaching=off byte-identity, at the unit level: stripping
    // removes exactly the canonical sentence and nothing else.
    let source = r#"
fn work(n: Int) -> Int {
    n
}

fn f() uses {Task.spawn} {
    scope {
        spawn work(n: 1);
    }
}
"#;
    let parsed = parse(source);
    let analysis = analyze(&parsed.module);
    let mut diagnostic = analysis.diagnostics[0].clone();
    assert!(diagnostic.message.contains(TASK_TEACH));
    diagnostic.strip_teaching();
    assert!(
        !diagnostic.message.contains(TASK_TEACH),
        "off-mode drops the sentence: {}",
        diagnostic.message
    );
    assert!(
        !diagnostic.message.ends_with(' '),
        "stripping leaves no residue: {:?}",
        diagnostic.message
    );
}
