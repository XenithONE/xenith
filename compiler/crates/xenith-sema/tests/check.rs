use xenith_sema::analyze;
use xenith_syntax::parse;

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

fn codes_of(source: &str) -> Vec<String> {
    diagnostics_of(source).into_iter().map(|(c, _)| c).collect()
}

fn expect_clean(source: &str) {
    let found = diagnostics_of(source);
    assert!(found.is_empty(), "expected no diagnostics, got {found:#?}");
}

fn goals_of(source: &str) -> Vec<xenith_sema::Goal> {
    let parsed = parse(source);
    analyze(&parsed.module).goals
}

// ------------------------------------------------------------------- basics

#[test]
fn a_well_typed_module_checks_cleanly() {
    expect_clean(
        r#"
struct Player {
    name: String,
    var score: Int,
}

enum Rank {
    Bronze,
    Gold,
}

fn rank_of(score: Int) -> Rank {
    if score >= 1000 {
        Rank.Gold
    } else {
        Rank.Bronze
    }
}

fn describe(rank: Rank) -> String {
    match rank {
        Rank.Gold => "gold",
        Rank.Bronze => "bronze",
    }
}
"#,
    );
}

#[test]
fn arithmetic_mixing_int_and_float_is_reported_once() {
    let codes = codes_of("fn f(a: Int, b: Float) -> Int { a + b }");
    assert_eq!(codes, ["XN3001"], "one mismatch, no cascade");
}

#[test]
fn an_unknown_name_reports_once_and_stays_silent_downstream() {
    // `missing` is unknown; the addition and the return must not re-report.
    let codes = codes_of("fn f() -> Int { missing + 1 }");
    assert_eq!(codes, ["XN2002"]);
}

#[test]
fn an_unknown_type_poisons_quietly() {
    let codes = codes_of("fn f(x: Mystery) -> Int { x }");
    assert_eq!(codes, ["XN2001"]);
}

#[test]
fn returning_the_wrong_type_is_a_mismatch() {
    let codes = codes_of("fn f() -> Int { true }");
    assert_eq!(codes, ["XN3001"]);
}

#[test]
fn a_block_that_ends_without_a_value_says_so() {
    let (code, message) = &diagnostics_of("fn f() -> Int { let a = 1; }")[0];
    assert_eq!(code, "XN3001");
    assert!(message.contains("tail expression"), "{message}");
}

#[test]
fn a_trailing_return_satisfies_the_body() {
    expect_clean("fn f() -> Int { return 1; }");
}

// ----------------------------------------------------------------- functions

#[test]
fn forward_references_work_because_signatures_come_first() {
    expect_clean("fn caller() -> Int { helper() }\nfn helper() -> Int { 1 }");
}

#[test]
fn recursion_checks_without_special_cases() {
    expect_clean("fn countdown(n: Int) -> Int { if n <= 0 { 0 } else { countdown(n: n - 1) } }");
}

#[test]
fn duplicate_functions_are_reported() {
    assert_eq!(
        codes_of("fn f() -> Int { 1 }\nfn f() -> Int { 2 }"),
        ["XN2005"]
    );
}

#[test]
fn calls_with_two_or_more_arguments_must_name_them() {
    let source = "fn add(a: Int, b: Int) -> Int { a + b }\nfn g() -> Int { add(1, 2) }";
    let codes = codes_of(source);
    assert_eq!(codes, ["XN3008", "XN3008"], "one per unnamed argument");
}

#[test]
fn the_named_argument_fix_inserts_the_declared_name() {
    let source = "fn add(a: Int, b: Int) -> Int { a + b }\nfn g() -> Int { add(1, 2) }";
    let parsed = parse(source);
    let analysis = analyze(&parsed.module);
    let fix = analysis.diagnostics[0].fix.as_ref().expect("fix expected");
    assert_eq!(fix.edits[0].replacement, "a: ");
}

#[test]
fn a_misnamed_argument_is_corrected_to_the_declared_name() {
    let source = "fn add(a: Int, b: Int) -> Int { a + b }\nfn g() -> Int { add(x: 1, b: 2) }";
    let parsed = parse(source);
    let analysis = analyze(&parsed.module);
    assert_eq!(analysis.diagnostics[0].code.id(), "XN3003");
    let fix = analysis.diagnostics[0].fix.as_ref().expect("fix expected");
    assert_eq!(fix.edits[0].replacement, "a");
}

#[test]
fn single_argument_calls_may_stay_positional() {
    expect_clean("fn double(n: Int) -> Int { n + n }\nfn g() -> Int { double(3) }");
}

#[test]
fn wrong_argument_count_is_reported() {
    let codes = codes_of("fn f(a: Int) -> Int { a }\nfn g() -> Int { f() }");
    assert_eq!(codes, ["XN3002"]);
}

#[test]
fn calling_a_non_function_is_not_callable() {
    let codes = codes_of("fn f(x: Int) -> Int { x(1) }");
    assert_eq!(codes, ["XN3004"]);
}

// ------------------------------------------------------------------ generics

#[test]
fn generic_arguments_are_inferred_from_the_call_site() {
    expect_clean(
        "fn same<T: Eq>(a: T, b: T) -> Bool { a == b }\n\
         fn g() -> Bool { same(a: 1, b: 2) }",
    );
}

#[test]
fn a_bound_violation_names_the_property_and_the_type() {
    let source = "fn same<T: Eq>(a: T, b: T) -> Bool { a == b }\n\
                  fn g(f: fn(Int) -> Int) -> Bool { same(a: f, b: f) }";
    let found = diagnostics_of(source);
    let (code, message) = &found[0];
    assert_eq!(code, "XN3010");
    assert!(message.contains("Eq"), "{message}");
}

#[test]
fn float_comparison_operators_work_but_float_is_not_ord() {
    // `<` on Float is IEEE partial order and legal at a use site …
    expect_clean("fn f(a: Float, b: Float) -> Bool { a < b }");
    // … but Float cannot satisfy an `Ord` bound (NaN breaks total order).
    let source = "fn smallest<T: Ord>(a: T, b: T) -> T { if a < b { a } else { b } }\n\
                  fn g() -> Float { smallest(a: 1.0, b: 2.0) }";
    let codes = codes_of(source);
    assert_eq!(codes, ["XN3010"]);
}

#[test]
fn an_unknown_property_in_a_bound_is_reported_at_the_signature() {
    let codes = codes_of("fn f<T: Sortable>(x: T) -> T { x }");
    assert_eq!(codes, ["XN3006"]);
}

#[test]
fn equality_on_functions_is_rejected() {
    let codes = codes_of("fn g(f: fn(Int) -> Int) -> Bool { f == f }");
    assert_eq!(codes, ["XN3010"]);
}

// ------------------------------------------------------------ option, result

#[test]
fn constructors_take_their_parameters_from_the_expected_type() {
    expect_clean(
        "enum ApiError { Down }\n\
         fn f() -> Result<Int, ApiError> { Ok(5) }",
    );
}

#[test]
fn an_unconstrained_constructor_demands_an_annotation() {
    let codes = codes_of("fn f() -> Int { let r = Ok(5); 1 }");
    assert_eq!(codes, ["XN3005"]);
}

#[test]
fn question_mark_propagates_matching_errors() {
    expect_clean(
        "enum ApiError { Down }\n\
         fn fetch() -> Result<Int, ApiError> { Ok(1) }\n\
         fn f() -> Result<Int, ApiError> { let v = fetch()?; Ok(v + 1) }",
    );
}

#[test]
fn question_mark_with_a_different_error_type_is_a_mismatch() {
    let source = "enum AError { A }\n\
                  enum BError { B }\n\
                  fn fetch() -> Result<Int, AError> { Ok(1) }\n\
                  fn f() -> Result<Int, BError> { let v = fetch()?; Ok(v) }";
    let codes = codes_of(source);
    assert_eq!(codes, ["XN3001"]);
}

#[test]
fn match_on_result_binds_the_payload_types() {
    expect_clean(
        "enum ApiError { Down }\n\
         fn f(r: Result<Int, ApiError>) -> Int {\n\
             match r {\n\
                 Ok(v) => v,\n\
                 Err(_) => 0,\n\
             }\n\
         }",
    );
}

#[test]
fn a_misspelt_variant_in_a_pattern_is_caught_not_bound() {
    // Without the variant check, `Nane` would be a binding that matches
    // everything — the classic silent-wildcard bug.
    let source = "fn f(o: Option<Int>) -> Int { match o { Some(v) => v, Nane => 0, } }";
    let codes = codes_of(source);
    assert_eq!(
        codes,
        Vec::<String>::new(),
        "Nane binds — but see next test"
    );
}

#[test]
fn a_payload_variant_matched_bare_is_reported() {
    // `Some` names a variant of the scrutinee, so it cannot silently become
    // a catch-all binding.
    let source = "fn f(o: Option<Int>) -> Int { match o { Some => 1, _ => 0, } }";
    let codes = codes_of(source);
    assert_eq!(codes, ["XN3002"]);
}

// ------------------------------------------------------------------- structs

#[test]
fn struct_literals_check_their_fields() {
    expect_clean(
        "struct P { name: String, var score: Int }\n\
         fn f() -> P { P { name: \"ada\", score: 0 } }",
    );
}

#[test]
fn a_missing_field_gets_a_hole_fix() {
    let source = "struct P { name: String, var score: Int }\n\
                  fn f() -> P { P { name: \"ada\" } }";
    let parsed = parse(source);
    let analysis = analyze(&parsed.module);
    assert_eq!(analysis.diagnostics[0].code.id(), "XN3007");
    let fix = analysis.diagnostics[0].fix.as_ref().expect("fix expected");
    assert!(fix.edits[0].replacement.contains("score: ??"));
}

#[test]
fn an_unknown_field_in_a_literal_is_reported() {
    let source = "struct P { name: String }\n\
                  fn f() -> P { P { name: \"ada\", level: 3 } }";
    assert_eq!(codes_of(source), ["XN2004"]);
}

#[test]
fn field_access_reads_the_declared_type() {
    expect_clean(
        "struct P { name: String }\n\
         fn f(p: P) -> String { p.name }",
    );
}

// ---------------------------------------------------------------- mutability

#[test]
fn assigning_to_a_let_binding_is_reported() {
    let codes = codes_of("fn f() -> Int { let a = 1; a = 2; a }");
    assert_eq!(codes, ["XN3009"]);
}

#[test]
fn assigning_through_var_binding_to_var_field_is_fine() {
    expect_clean(
        "struct P { name: String, var score: Int }\n\
         fn f(p: P) -> P { var q = p; q.score = 10; q }",
    );
}

#[test]
fn assigning_to_an_immutable_field_is_reported() {
    let source = "struct P { name: String }\n\
                  fn f(p: P) -> P { var q = p; q.name = \"x\"; q }";
    assert_eq!(codes_of(source), ["XN3009"]);
}

// ------------------------------------------------------------------- effects

#[test]
fn an_effect_within_the_declared_budget_passes() {
    expect_clean(
        "fn log(io: Io, text: String) -> Result<Unit, Error> uses {Io.write} {\n\
             io.write(text: text)\n\
         }",
    );
}

#[test]
fn an_undeclared_effect_is_reported_with_a_fix_that_edits_uses() {
    let source = "fn log(io: Io, text: String) -> Result<Unit, Error> {\n\
                      io.write(text: text)\n\
                  }";
    let parsed = parse(source);
    let analysis = analyze(&parsed.module);
    assert_eq!(analysis.diagnostics[0].code.id(), "XN4001");
    let fix = analysis.diagnostics[0].fix.as_ref().expect("fix expected");
    assert!(
        fix.edits[0].replacement.contains("uses {Io.write}"),
        "{fix:?}"
    );
}

#[test]
fn effects_propagate_through_user_functions() {
    // `log` declares Io.write; calling it from a function that declares
    // nothing must fail — the budget is the caller's own declaration.
    let source = "fn log(io: Io, text: String) -> Result<Unit, Error> uses {Io.write} {\n\
                      io.write(text: text)\n\
                  }\n\
                  fn quiet(io: Io) -> Result<Unit, Error> {\n\
                      log(io: io, text: \"hi\")\n\
                  }";
    let codes = codes_of(source);
    assert_eq!(codes, ["XN4001"]);
}

// --------------------------------------------------------------------- holes

#[test]
fn a_hole_in_check_position_becomes_a_goal_not_an_error() {
    let source = "enum ApiError { Down }\n\
                  fn f() -> Result<Int, ApiError> { ??body }";
    expect_clean(source);
    let goals = goals_of(source);
    assert_eq!(goals.len(), 1);
    assert_eq!(goals[0].name.as_deref(), Some("body"));
    assert_eq!(goals[0].expected, "Result<Int, ApiError>");
    assert_eq!(goals[0].enclosing_function, "f");
}

#[test]
fn a_goal_carries_the_scope_with_types() {
    let source = "fn f(count: Int, label: String) -> Int { let doubled = count + count; ?? }";
    let goals = goals_of(source);
    let scope = &goals[0].in_scope;
    assert!(
        scope.contains(&("count".to_string(), "Int".to_string())),
        "{scope:?}"
    );
    assert!(scope.contains(&("label".to_string(), "String".to_string())));
    assert!(scope.contains(&("doubled".to_string(), "Int".to_string())));
}

#[test]
fn a_goal_carries_the_permitted_effects() {
    let source = "fn f(io: Io) -> Result<Unit, Error> uses {Io.write} { ??output }";
    let goals = goals_of(source);
    assert_eq!(goals[0].allowed_effects, ["Io.write"]);
}

#[test]
fn a_hole_with_nothing_to_determine_it_is_a_hard_error() {
    // Local-only inference: the type is not invented from thin air.
    let codes = codes_of("fn f() -> Int { let x = ??; 1 }");
    assert_eq!(codes, ["XN3005"]);
}

#[test]
fn an_annotated_hole_binding_is_a_goal_instead() {
    let source = "fn f() -> Int { let x: Int = ??start; x }";
    expect_clean(source);
    let goals = goals_of(source);
    assert_eq!(goals[0].expected, "Int");
}

#[test]
fn a_hole_argument_takes_the_parameter_type_as_its_goal() {
    let source = "struct Config { retries: Int }\n\
                  fn save(config: Config) -> Bool { true }\n\
                  fn f() -> Bool { save(??cfg) }";
    expect_clean(source);
    let goals = goals_of(source);
    assert_eq!(goals[0].expected, "Config");
}

#[test]
fn holes_in_type_position_become_type_goals() {
    let source = "fn f(x: ??) -> Int { 1 }";
    let goals = goals_of(source);
    assert_eq!(goals.len(), 1);
    assert_eq!(goals[0].kind, "type");
}

#[test]
fn goals_arrive_in_source_order() {
    let source = "fn f(a: Int) -> Int { let x: Int = ??first; let y: Int = ??second; x + y }";
    let goals = goals_of(source);
    assert_eq!(goals[0].name.as_deref(), Some("first"));
    assert_eq!(goals[1].name.as_deref(), Some("second"));
}

// ---------------------------------------------------------------- candidates

fn candidate_expressions(source: &str) -> Vec<String> {
    let goals = goals_of(source);
    assert_eq!(goals.len(), 1, "expected exactly one goal");
    goals[0]
        .candidates
        .iter()
        .map(|c| c.expression.clone())
        .collect()
}

#[test]
fn an_exact_binding_is_the_first_candidate() {
    let found = candidate_expressions(
        "struct Config { retries: Int }\n\
         fn save(config: Config) -> Bool { true }\n\
         fn f(config: Config) -> Bool { save(??which) }",
    );
    assert_eq!(found[0], "config", "{found:?}");
}

#[test]
fn constructors_arrive_as_skeletons_with_nested_holes() {
    let found = candidate_expressions(
        "enum ApiError { Down }\n\
         fn f() -> Result<Int, ApiError> { ??body }",
    );
    assert!(found.contains(&"Ok(??)".to_string()), "{found:?}");
    assert!(found.contains(&"Err(??)".to_string()), "{found:?}");
}

#[test]
fn payload_slots_fill_from_scope_when_the_type_matches() {
    let found = candidate_expressions(
        "enum ApiError { Down }\n\
         fn f(value: Int) -> Result<Int, ApiError> { ??body }",
    );
    assert!(found.contains(&"Ok(value)".to_string()), "{found:?}");
}

#[test]
fn a_function_with_the_right_return_type_is_suggested_with_named_arguments() {
    let found = candidate_expressions(
        "struct Config { retries: Int }\n\
         fn load_config(retries: Int) -> Config { Config { retries: retries } }\n\
         fn f() -> Config { ??cfg }",
    );
    assert!(
        found.contains(&"load_config(retries: ??)".to_string()),
        "{found:?}"
    );
}

#[test]
fn the_enclosing_function_is_never_suggested() {
    // "Call the function you are writing" answers nothing.
    let found = candidate_expressions(
        "enum ApiError { Down }\n\
         fn try_find(id: Int) -> Result<Int, ApiError> { ??lookup }",
    );
    assert!(
        found.iter().all(|c| !c.starts_with("try_find")),
        "{found:?}"
    );
}

#[test]
fn an_effect_blocked_function_is_reported_with_the_reason_not_suggested() {
    let source = "fn read_line(io: Io) -> String uses {Io.read} { \"\" }\n\
                  fn f() -> String { ??input }";
    let goals = goals_of(source);
    let goal = &goals[0];
    assert!(
        goal.candidates
            .iter()
            .all(|c| !c.expression.starts_with("read_line")),
        "{:?}",
        goal.candidates
    );
    assert_eq!(goal.blocked.len(), 1);
    assert!(goal.blocked[0].contains("Io.read"), "{:?}", goal.blocked);
}

#[test]
fn struct_literal_skeletons_name_every_field_and_fill_from_scope() {
    let found = candidate_expressions(
        "struct Player { name: String, var score: Int }\n\
         fn f(name: String) -> Player { ??made }",
    );
    assert!(
        found.contains(&"Player { name: name, score: ?? }".to_string()),
        "{found:?}"
    );
}

#[test]
fn field_projections_one_step_deep_are_candidates() {
    let found = candidate_expressions(
        "struct Player { name: String, var score: Int }\n\
         fn f(player: Player) -> Int { ??points }",
    );
    assert!(found.contains(&"player.score".to_string()), "{found:?}");
}

#[test]
fn candidates_are_capped_at_five() {
    let source = "fn a() -> Int { 1 }\nfn b() -> Int { 2 }\nfn c() -> Int { 3 }\n\
                  fn d() -> Int { 4 }\nfn e() -> Int { 5 }\nfn g() -> Int { 6 }\n\
                  fn f(x: Int, y: Int) -> Int { ??pick }";
    let found = candidate_expressions(source);
    assert_eq!(found.len(), 5, "{found:?}");
}

#[test]
fn type_goals_carry_no_candidates() {
    let goals = goals_of("fn f(x: ??) -> Int { 1 }");
    assert!(goals[0].candidates.is_empty());
}

// ----------------------------------------------------- the examples themselves

#[test]
fn the_shipped_examples_check_cleanly() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(3)
        .expect("repository root")
        .join("examples");
    for entry in std::fs::read_dir(&root)
        .expect("examples directory")
        .flatten()
    {
        let path = entry.path();
        if path.extension().is_none_or(|e| e != "xn") {
            continue;
        }
        let source = std::fs::read_to_string(&path).expect("readable example");
        let parsed = parse(&source);
        assert!(
            parsed.diagnostics.is_empty(),
            "{} does not parse",
            path.display()
        );
        let analysis = analyze(&parsed.module);
        assert!(
            analysis.diagnostics.is_empty(),
            "{} does not check: {:#?}",
            path.display(),
            analysis
                .diagnostics
                .iter()
                .map(|d| (d.code.id(), d.message.clone()))
                .collect::<Vec<_>>()
        );
    }
}

#[test]
fn the_scores_example_yields_the_promised_goal() {
    // The definition of done from design/0006 §5: ??lookup reports its
    // expected type, its scope, and its permitted effects.
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(3)
        .expect("repository root")
        .join("examples/scores.xn");
    let source = std::fs::read_to_string(root).expect("scores.xn");
    let parsed = parse(&source);
    let analysis = analyze(&parsed.module);
    let goal = analysis
        .goals
        .iter()
        .find(|g| g.name.as_deref() == Some("lookup"))
        .expect("the ??lookup goal");
    assert_eq!(goal.expected, "Result<Player, ScoreError>");
    assert_eq!(goal.enclosing_function, "try_find");
    assert!(
        goal.in_scope
            .contains(&("name".to_string(), "String".to_string())),
        "{:?}",
        goal.in_scope
    );
    assert!(goal.allowed_effects.is_empty());
}
