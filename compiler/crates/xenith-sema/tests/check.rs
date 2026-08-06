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
    // `Shared` satisfies no property — its identity is compared with `is`.
    let source = "fn same<T: Eq>(a: T, b: T) -> Bool { a == b }\n\
                  fn g(s: Shared<Int>) -> Bool { same(a: s, b: s) }";
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
    // Function-type annotations are unshipped (0008 §1), but a named
    // function bound as a value still exists — and still compares as nothing.
    let source = "fn double(n: Int) -> Int { n + n }\n\
                  fn g() -> Bool { let f = double; f == f }";
    let codes = codes_of(source);
    assert_eq!(codes, ["XN3010"]);
}

// ------------------------------------------------------------ option, result

#[test]
fn qualified_variant_construction_with_a_payload_checks() {
    // `Grade.Pass(95)` parses as a method call, and the checker once synthed
    // the receiver and reported `Grade` as an unknown *value*. Found by the
    // interpreter's test suite — no earlier test ever constructed a qualified
    // variant that carries a payload.
    expect_clean(
        "enum Grade { Pass(Int), Fail }\n\
         fn f() -> Grade { Grade.Pass(95) }\n\
         fn g(grade: Grade) -> Int { 1 }\n\
         fn h() -> Int { g(grade: Grade.Pass(60)) }",
    );
}

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

// --------------------------------------------------------------------- lists

#[test]
fn a_list_literal_takes_its_element_type_from_the_expected_type() {
    expect_clean("fn f() -> List<Int> { [1, 2, 3] }");
    expect_clean("fn f() -> List<List<Int>> { [[1], []] }");
}

#[test]
fn an_empty_list_literal_checks_against_an_annotation() {
    expect_clean("fn f() -> List<Int> { [] }");
    expect_clean("fn f() -> Int { let xs: List<Int> = []; xs.len() }");
}

#[test]
fn an_empty_list_with_nothing_to_determine_it_demands_an_annotation() {
    // Same policy as bare holes (0006 §1-1): the element type is not invented.
    let codes = codes_of("fn f() -> Int { let xs = []; 1 }");
    assert_eq!(codes, ["XN3005"]);
}

#[test]
fn a_mismatched_element_is_reported_against_the_expected_element_type() {
    let codes = codes_of("fn f() -> List<Int> { [1, true] }");
    assert_eq!(codes, ["XN3001"]);
}

#[test]
fn a_list_literal_in_a_non_list_position_is_a_mismatch() {
    assert_eq!(codes_of("fn f() -> Int { [1] }"), ["XN3001"]);
    assert_eq!(codes_of("fn f() -> Int { [] }"), ["XN3001"]);
}

#[test]
fn a_list_literal_synthesises_from_its_first_element() {
    expect_clean("fn f() -> List<Int> { let xs = [1, 2]; xs }");
    let codes = codes_of("fn f() -> Int { let xs = [1, true]; 1 }");
    assert_eq!(codes, ["XN3001"]);
}

#[test]
fn a_hole_element_inside_a_list_becomes_a_goal() {
    let source = "fn f() -> List<Int> { [1, ??gap, 3] }";
    expect_clean(source);
    let goals = goals_of(source);
    assert_eq!(goals[0].name.as_deref(), Some("gap"));
    assert_eq!(goals[0].expected, "Int");
}

#[test]
fn the_list_reads_check_with_their_declared_signatures() {
    expect_clean(
        "fn a(xs: List<Int>) -> Int { xs.len() }\n\
         fn b(xs: List<Int>) -> Bool { xs.is_empty() }\n\
         fn c(xs: List<Int>) -> Option<Int> { xs.get(index: 0) }\n\
         fn d(xs: List<Int>) -> Bool { xs.contains(item: 3) }\n\
         fn e(xs: List<Int>) -> List<Int> { xs.sorted() }\n\
         fn g(xs: List<Int>) -> List<Int> { xs.concat(other: xs) }\n\
         fn h(xs: List<String>) -> String { xs.join(sep: \", \") }",
    );
}

#[test]
fn push_on_a_let_binding_is_reported() {
    let codes = codes_of("fn f() -> Int { let xs = [1]; xs.push(item: 2); xs.len() }");
    assert_eq!(codes, ["XN3009"]);
}

#[test]
fn pop_and_replace_also_require_a_var_binding() {
    assert_eq!(
        codes_of("fn f() -> Option<Int> { let xs = [1]; xs.pop() }"),
        ["XN3009"]
    );
    assert_eq!(
        codes_of("fn f() -> Option<Int> { let xs = [1]; xs.replace(index: 0, value: 2) }"),
        ["XN3009"]
    );
}

#[test]
fn the_mutators_are_fine_on_a_var_binding() {
    expect_clean(
        "fn f() -> Int { var xs = [1]; xs.push(item: 2); xs.pop(); \
         xs.replace(index: 0, value: 9); xs.len() }",
    );
}

#[test]
fn push_on_a_temporary_is_reported() {
    let codes = codes_of("fn f() -> Unit { [1].push(item: 2) }");
    assert_eq!(codes, ["XN3009"]);
}

#[test]
fn push_through_an_immutable_field_is_reported() {
    let source = "struct Deck { cards: List<Int> }\n\
                  fn f() -> Unit { var deck = Deck { cards: [1] }; deck.cards.push(item: 2) }";
    assert_eq!(codes_of(source), ["XN3009"]);
}

#[test]
fn push_through_a_let_binding_to_a_var_field_is_reported() {
    let source = "struct Deck { var cards: List<Int> }\n\
                  fn f() -> Unit { let deck = Deck { cards: [1] }; deck.cards.push(item: 2) }";
    assert_eq!(codes_of(source), ["XN3009"]);
}

#[test]
fn push_through_a_var_binding_to_a_var_field_is_fine() {
    expect_clean(
        "struct Deck { var cards: List<Int> }\n\
         fn f() -> Unit { var deck = Deck { cards: [1] }; deck.cards.push(item: 2) }",
    );
}

#[test]
fn sorted_on_floats_is_rejected_by_the_ord_bound() {
    // NaN breaks total order, so `Float` is not `Ord` (0006 §3).
    let found = diagnostics_of("fn f(xs: List<Float>) -> List<Float> { xs.sorted() }");
    let (code, message) = &found[0];
    assert_eq!(code, "XN3010");
    assert!(message.contains("Ord"), "{message}");
    assert_eq!(
        codes_of("fn f() -> List<Float> { [1.0].sorted() }"),
        ["XN3010"]
    );
}

#[test]
fn contains_on_a_non_eq_element_is_rejected() {
    // `Shared` satisfies no property, so a list of them has no `contains`.
    let found = diagnostics_of(
        "fn f(xs: List<Shared<Int>>, s: Shared<Int>) -> Bool { xs.contains(item: s) }",
    );
    let (code, message) = &found[0];
    assert_eq!(code, "XN3010");
    assert!(message.contains("Eq"), "{message}");
}

#[test]
fn a_generic_ord_bound_carries_through_to_sorted() {
    expect_clean("fn f<T: Ord>(xs: List<T>) -> List<T> { xs.sorted() }");
    assert_eq!(
        codes_of("fn f<T>(xs: List<T>) -> List<T> { xs.sorted() }"),
        ["XN3010"]
    );
}

#[test]
fn replace_takes_named_arguments_like_any_two_parameter_call() {
    let codes = codes_of("fn f() -> Option<Int> { var xs = [1]; xs.replace(0, 5) }");
    assert_eq!(codes, ["XN3008", "XN3008"]);
}

// ------------------------------------------------------------------- strings

#[test]
fn the_string_additions_check_with_their_declared_signatures() {
    expect_clean(
        "fn a(s: String) -> Int { s.len() }\n\
         fn b(s: String) -> List<String> { s.split(sep: \",\") }\n\
         fn c(s: String) -> String { s.trim() }\n\
         fn d(s: String) -> Result<Int, Error> { s.try_to_int() }\n\
         fn e(s: String) -> Bool { s.starts_with(prefix: \"x\") }\n\
         fn g(s: String) -> Bool { s.contains(sub: \"x\") }",
    );
}

#[test]
fn try_to_int_threads_through_question_mark() {
    expect_clean(
        "fn parse_next(s: String) -> Result<Int, Error> { let v = s.try_to_int()?; Ok(v + 1) }",
    );
}

#[test]
fn a_wrong_separator_type_is_a_mismatch() {
    assert_eq!(
        codes_of("fn f(s: String) -> List<String> { s.split(sep: 1) }"),
        ["XN3001"]
    );
}

// ---------------------------------------------------------------------- maps

#[test]
fn map_reads_check_with_their_declared_signatures() {
    expect_clean(
        "fn a(m: Map<String, Int>) -> Int { m.len() }\n\
         fn b(m: Map<String, Int>) -> Bool { m.is_empty() }\n\
         fn c(m: Map<String, Int>) -> Option<Int> { m.get(key: \"x\") }\n\
         fn d(m: Map<String, Int>) -> Bool { m.has_key(key: \"x\") }\n\
         fn e(m: Map<String, Int>) -> List<String> { m.keys() }\n\
         fn g(m: Map<String, Int>) -> String { m.keys().join(sep: \",\") }",
    );
}

#[test]
fn empty_map_takes_its_arguments_from_the_expected_type() {
    expect_clean("fn f() -> Map<String, Int> { empty_map() }");
    expect_clean("fn f() -> Int { let m: Map<String, Int> = empty_map(); m.len() }");
}

#[test]
fn a_bare_empty_map_demands_an_annotation() {
    // Same policy as `let x = ??;` (0006 §1-1): one refusal per parameter
    // nothing determines.
    let codes = codes_of("fn f() -> Int { let m = empty_map(); 1 }");
    assert_eq!(codes, ["XN3005", "XN3005"]);
}

#[test]
fn a_float_key_is_refused_at_empty_map() {
    // NaN breaks hashing, so `Float` is not `Hash` (0006 §3).
    let found = diagnostics_of("fn f() -> Int { let m: Map<Float, Int> = empty_map(); 1 }");
    let (code, message) = &found[0];
    assert_eq!(code, "XN3010");
    assert!(message.contains("Hash"), "{message}");
}

#[test]
fn a_float_key_is_refused_at_every_map_method() {
    // Container type arguments are not bound-checked at the annotation, so
    // the receiver is where a smuggled `Map<Float, _>` is caught.
    assert_eq!(
        codes_of("fn f(m: Map<Float, Int>) -> Int { m.len() }"),
        ["XN3010"]
    );
}

#[test]
fn map_mutators_require_a_var_binding() {
    assert_eq!(
        codes_of(
            "fn f() -> Option<Int> { let m: Map<String, Int> = empty_map(); \
             m.insert(key: \"a\", value: 1) }"
        ),
        ["XN3009"]
    );
    assert_eq!(
        codes_of(
            "fn f() -> Option<Int> { let m: Map<String, Int> = empty_map(); \
             m.remove(key: \"a\") }"
        ),
        ["XN3009"]
    );
}

#[test]
fn map_mutators_are_fine_on_a_var_binding() {
    expect_clean(
        "fn f() -> Int { var m: Map<String, Int> = empty_map(); \
         m.insert(key: \"a\", value: 1); m.remove(key: \"a\"); m.len() }",
    );
}

#[test]
fn a_user_function_may_not_redeclare_empty_map() {
    // The prelude registered it first, so the ordinary duplicate check fires;
    // the body is then checked against the surviving (prelude) signature,
    // exactly as any duplicate's body is checked against the first one's.
    assert_eq!(
        codes_of("fn empty_map() -> Int { 1 }"),
        ["XN2005", "XN3001"]
    );
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

// ------------------------------------------------------- unshipped constructs

#[test]
fn closures_are_rejected_until_their_rfc() {
    // 0007 D3: function values do not exist. The parser accepts the syntax
    // for recovery; the checker refuses it (0008 §1).
    let codes = codes_of("fn f() -> Int { let g = |x: Int| x; 1 }");
    assert_eq!(codes, ["XN1008"]);
}

#[test]
fn await_is_rejected_until_the_async_rfc() {
    let source = "fn g() -> Int { 1 }\nfn f() -> Int { g().await }";
    assert_eq!(codes_of(source), ["XN1008"]);
}

#[test]
fn async_fn_is_rejected_until_the_async_rfc() {
    assert_eq!(codes_of("async fn g() -> Int { 1 }"), ["XN1008"]);
}

#[test]
fn for_is_rejected_but_its_body_still_checks() {
    // One diagnostic for the construct; the body's own mistake still
    // surfaces, because recovery keeps walking.
    let source = "fn f(xs: List<Int>) -> Int { for x in xs { let y: Bool = x; } 1 }";
    assert_eq!(codes_of(source), ["XN1008", "XN3001"]);
}

#[test]
fn fn_type_annotations_are_rejected() {
    assert_eq!(
        codes_of("fn apply(f: fn(Int) -> Int) -> Int { 1 }"),
        ["XN1008"]
    );
    assert_eq!(
        codes_of("fn f() -> Int { let g: fn() -> Int = ??; 1 }"),
        ["XN1008"]
    );
}

// ------------------------------------------------------------- exhaustiveness

#[test]
fn a_bool_match_missing_false_is_refused_with_the_witness() {
    let found = diagnostics_of("fn f(b: Bool) -> Int { match b { true => 1 } }");
    let (code, message) = &found[0];
    assert_eq!(code, "XN5001");
    assert!(message.contains("`false`"), "{message}");
}

#[test]
fn a_complete_bool_match_checks_cleanly() {
    expect_clean("fn f(b: Bool) -> Int { match b { true => 1, false => 0 } }");
}

#[test]
fn an_enum_match_missing_a_variant_names_it() {
    let source = "enum Rank { Bronze, Silver, Gold }\n\
                  fn f(r: Rank) -> Int { match r { Rank.Bronze => 1, Rank.Silver => 2 } }";
    let found = diagnostics_of(source);
    let (code, message) = &found[0];
    assert_eq!(code, "XN5001");
    assert!(message.contains("Rank.Gold"), "{message}");
}

#[test]
fn payload_patterns_recurse_for_coverage() {
    let source = "fn f(o: Option<Bool>) -> Int { match o { Some(true) => 1, None => 0 } }";
    let found = diagnostics_of(source);
    let (code, message) = &found[0];
    assert_eq!(code, "XN5001");
    assert!(message.contains("Some(false)"), "{message}");
    expect_clean(
        "fn f(o: Option<Bool>) -> Int { match o { Some(true) => 1, Some(false) => 2, None => 0 } }",
    );
}

#[test]
fn a_missing_payload_variant_renders_a_wildcard_payload() {
    let source = "enum Shape { Rect(Int, Int), Dot }\n\
                  fn f(s: Shape) -> Int { match s { Shape.Dot => 0 } }";
    let found = diagnostics_of(source);
    assert!(found[0].1.contains("Shape.Rect(_, _)"), "{}", found[0].1);
}

#[test]
fn or_patterns_cover_each_alternative() {
    expect_clean(
        "enum Rank { Bronze, Silver, Gold }\n\
         fn f(r: Rank) -> Int { match r { Rank.Bronze | Rank.Silver => 1, Rank.Gold => 2 } }",
    );
}

#[test]
fn a_guarded_arm_contributes_nothing_to_coverage() {
    // The guard can be false at runtime, so the value must land elsewhere.
    let source = "fn f(b: Bool, c: Bool) -> Int { match b { true => 1, false if c => 0 } }";
    let found = diagnostics_of(source);
    let (code, message) = &found[0];
    assert_eq!(code, "XN5001");
    assert!(message.contains("`false`"), "{message}");
    expect_clean(
        "fn f(b: Bool, c: Bool) -> Int { match b { true if c => 1, true => 2, false => 0 } }",
    );
}

#[test]
fn a_wildcard_or_binding_arm_covers_non_enumerable_scrutinees() {
    expect_clean("fn f(n: Int) -> Int { match n { 0 => 1, _ => 2 } }");
    expect_clean("fn f(s: String) -> Int { match s { text => text.len() } }");
}

#[test]
fn an_int_match_without_a_wildcard_is_refused() {
    // Int cannot be enumerated by literals; the witness is the catch-all.
    let found = diagnostics_of("fn f(n: Int) -> Int { match n { 0 => 1, 1 => 2 } }");
    let (code, message) = &found[0];
    assert_eq!(code, "XN5001");
    assert!(message.contains("`_`"), "{message}");
}

#[test]
fn option_and_result_misses_render_unqualified() {
    let found = diagnostics_of("fn f(o: Option<Int>) -> Int { match o { Some(v) => v } }");
    assert!(found[0].1.contains("`None`"), "{}", found[0].1);
    // The payload enum is enumerable, so the witness is concrete rather
    // than `_` — `Err(E.X)` names an exact value the arms miss.
    let found =
        diagnostics_of("enum E { X }\nfn f(r: Result<Int, E>) -> Int { match r { Ok(v) => v } }");
    assert!(found[0].1.contains("Err(E.X)"), "{}", found[0].1);
}

#[test]
fn struct_patterns_recurse_into_fields() {
    let source = "struct Flag { alive: Bool }\n\
                  fn f(p: Flag) -> Int { match p { Flag { alive: true } => 1 } }";
    let found = diagnostics_of(source);
    let (code, message) = &found[0];
    assert_eq!(code, "XN5001");
    assert!(message.contains("alive: false"), "{message}");
    expect_clean(
        "struct Flag { alive: Bool }\n\
         fn f(p: Flag) -> Int { match p { Flag { alive } => 1 } }",
    );
}

#[test]
fn exhaustiveness_still_runs_when_an_arm_body_is_rejected() {
    // A rejected construct in one arm's body must not mask the missing arm.
    let source = "fn f(b: Bool) -> Int { match b { true => { let g = |x: Int| x; 1 } } }";
    assert_eq!(codes_of(source), ["XN1008", "XN5001"]);
}

// ------------------------------------------------------------------- teaches

/// The diagnostics with their teaches, for asserting attachment structure.
fn taught_of(source: &str) -> Vec<xenith_diag::Diagnostic> {
    let parsed = parse(source);
    assert!(
        parsed.diagnostics.is_empty(),
        "test source must parse cleanly first: {:?}",
        parsed.diagnostics
    );
    analyze(&parsed.module).diagnostics
}

#[test]
fn xn2003_teaches_the_receivers_method_catalogue() {
    use xenith_diag::TeachKind;
    let found = taught_of("fn f(xs: List<Int>) -> Int { xs.size() }");
    assert_eq!(found[0].code.id(), "XN2003");
    let teach = &found[0].teaches[0];
    assert_eq!(teach.kind, TeachKind::AvailableMethods);
    assert_eq!(teach.type_name, "List<Int>");
    assert_eq!(teach.items.len(), 6, "the finite contract caps at six");
    assert_eq!(teach.total_items, 10);
    assert!(teach.truncated);
    assert_eq!(teach.items[0].signature, "len() -> Int");
    // Declaration order, not alphabetical.
    assert_eq!(teach.items[2].name, "push");
}

#[test]
fn a_taught_catalogue_shows_receiver_generics_bound() {
    let found = taught_of("fn f(m: Map<String, Int>) -> Int { m.set(key: \"a\", value: 1) }");
    let teach = &found[0].teaches[0];
    assert_eq!(teach.type_name, "Map<String, Int>");
    assert!(
        teach
            .items
            .iter()
            .any(|i| i.signature == "insert(key: String, value: Int) -> Option<Int>"),
        "{:?}",
        teach.items
    );
}

#[test]
fn xn3008_teaches_the_callees_signature_once_per_call_site() {
    use xenith_diag::TeachKind;
    let found = taught_of(
        "fn add(a: Int, b: Int) -> Int { a + b }\n\
         fn g() -> Int { add(1, 2) }",
    );
    assert_eq!(found.len(), 2, "one XN3008 per unnamed argument");
    let teach = &found[0].teaches[0];
    assert_eq!(teach.kind, TeachKind::CallSignature);
    assert_eq!(teach.items[0].signature, "add(a: Int, b: Int) -> Int");
    assert!(
        found[1].teaches.is_empty(),
        "the signature is not repeated within one call site"
    );
}

#[test]
fn a_method_call_signature_is_taught_concretely() {
    let found = taught_of("fn f() -> Option<Int> { var xs = [1]; xs.replace(0, 9) }");
    let teach = &found[0].teaches[0];
    assert_eq!(teach.type_name, "List<Int>");
    assert_eq!(
        teach.items[0].signature,
        "replace(index: Int, value: Int) -> Option<Int>"
    );
}

#[test]
fn a_taught_signature_carries_the_callees_effects() {
    let found = taught_of("fn f(io: Io) -> Result<Unit, Error> uses {Io.write} { io.write() }");
    assert_eq!(found[0].code.id(), "XN3002");
    assert_eq!(
        found[0].teaches[0].items[0].signature,
        "write(text: String) -> Result<Unit, Error> uses {Io.write}"
    );
}

#[test]
fn a_catalogue_appears_once_per_type_per_check() {
    let found = taught_of(
        "fn f(xs: List<Int>) -> Int { xs.first(); xs.last(); 1 }\n\
         fn g(ys: List<Int>) -> Int { ys.head(); 1 }",
    );
    let taught: Vec<bool> = found.iter().map(|d| !d.teaches.is_empty()).collect();
    // Three XN2003 on the same receiver type, across two functions: the
    // catalogue rides the first and only the first.
    assert_eq!(taught, [true, false, false], "{found:#?}");
}

#[test]
fn the_teaching_budget_caps_blocks_per_check() {
    // Six distinct receiver types, six catalogues asked for, five granted.
    let source = "fn f(a: List<Int>, b: List<String>, c: Map<String, Int>, s: String, n: Int, \
                  o: Option<Int>) -> Int {\n\
                  a.nope(); b.nope(); c.nope(); s.nope(); n.nope(); o.nope(); 1 }";
    let found = taught_of(source);
    assert_eq!(found.len(), 6);
    let taught = found.iter().filter(|d| !d.teaches.is_empty()).count();
    assert_eq!(taught, 5, "first come, first served, five per run");
    assert!(found[5].teaches.is_empty(), "the sixth arrives too late");
}

#[test]
fn an_overlong_taught_signature_is_cut_at_the_byte_budget() {
    let long = "a_parameter_name_that_goes_on_and_on_and_on_for_a_very_long_time";
    let source = format!(
        "fn f({long}_one: Int, {long}_two: Int, {long}_three: Int) -> Int {{ 1 }}\n\
         fn g() -> Int {{ f(1, 2, 3) }}"
    );
    let found = taught_of(&source);
    let signature = &found[0].teaches[0].items[0].signature;
    assert!(signature.ends_with('…'), "{signature}");
    assert!(signature.len() <= xenith_diag::MAX_SIGNATURE_BYTES + '…'.len_utf8());
}

#[test]
fn a_receiver_with_no_methods_teaches_nothing() {
    let found = taught_of(
        "struct Empty {}\n\
         fn f(e: Empty) -> Int { e.nope() }",
    );
    assert_eq!(found[0].code.id(), "XN2003");
    assert!(
        found[0].teaches.is_empty(),
        "an empty catalogue is silence, not an empty block"
    );
}

// -------------------------------------------------------------- did-you-mean

#[test]
fn a_one_edit_method_typo_gets_a_did_you_mean() {
    let found = diagnostics_of("fn f(xs: List<Int>) -> Unit { xs.pussh(item: 2) }");
    assert!(
        found[0].1.ends_with("; did you mean `push`?"),
        "{}",
        found[0].1
    );
    let found = diagnostics_of("fn f(xs: List<Int>) -> Int { xs.lenn() }");
    assert!(
        found[0].1.ends_with("; did you mean `len`?"),
        "{}",
        found[0].1
    );
    let found = diagnostics_of("fn f(m: Map<String, Int>) -> Bool { m.has_keys(key: \"a\") }");
    assert!(
        found[0].1.ends_with("; did you mean `has_key`?"),
        "{}",
        found[0].1
    );
}

#[test]
fn a_distant_method_name_suggests_nothing() {
    let found = diagnostics_of("fn f(xs: List<Int>) -> Int { xs.size() }");
    assert_eq!(found[0].1, "`List<Int>` has no method named `size`");
}

#[test]
fn a_method_tie_suggests_nothing() {
    // `gen` is one edit from both `get` and `len`; a coin toss is not a
    // suggestion.
    let found = diagnostics_of("fn f(xs: List<Int>) -> Int { xs.gen() }");
    assert_eq!(found[0].1, "`List<Int>` has no method named `gen`");
}

#[test]
fn a_transposed_binding_gets_a_did_you_mean() {
    // A transposition costs one — Damerau, not plain Levenshtein.
    let found = diagnostics_of("fn f() -> Int { let count = 1; cuont }");
    assert_eq!(found[0].0, "XN2002");
    assert!(
        found[0].1.ends_with("; did you mean `count`?"),
        "{}",
        found[0].1
    );
}

#[test]
fn a_misspelled_function_gets_a_did_you_mean() {
    let found = diagnostics_of(
        "fn double(n: Int) -> Int { n + n }\n\
         fn g() -> Int { doubel(3) }",
    );
    assert!(
        found[0].1.ends_with("; did you mean `double`?"),
        "{}",
        found[0].1
    );
}

#[test]
fn a_binding_tie_suggests_nothing() {
    let found = diagnostics_of("fn f(rate: Int, late: Int) -> Int { gate }");
    assert_eq!(found[0].1, "nothing named `gate` is in scope");
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

// The L0 gate (design/0007 §4): if `goals` cannot surface the List surface at
// a hole, the separation experiment's oracle is dead and the rest of the
// slice is not worth building on.

#[test]
fn l0_gate_goals_surface_list_methods() {
    let found = candidate_expressions("fn f(xs: List<Int>) -> Int { ??count }");
    assert!(
        found.iter().any(|c| c.contains("len")),
        "an Int hole with a List<Int> in scope must offer `len`: {found:?}"
    );
}

#[test]
fn l0_gate_goals_surface_list_producers() {
    let found = candidate_expressions("fn f(xs: List<Int>) -> List<Int> { ??result }");
    assert!(
        found.iter().any(|c| c.contains("sorted")),
        "a List<Int> hole must offer `sorted`: {found:?}"
    );
    assert!(
        found.iter().any(|c| c.contains("concat")),
        "a List<Int> hole must offer `concat`: {found:?}"
    );
}

#[test]
fn a_bound_blocked_method_is_reported_with_the_reason_not_suggested() {
    let goals = goals_of("fn f(xs: List<Float>) -> List<Float> { ??result }");
    let goal = &goals[0];
    assert!(
        goal.candidates
            .iter()
            .all(|c| !c.expression.contains("sorted")),
        "{:?}",
        goal.candidates
    );
    assert!(
        goal.blocked.iter().any(|b| b.contains("Ord")),
        "{:?}",
        goal.blocked
    );
}

// The M gate — the same claim as L0, for the Map surface.

#[test]
fn m_gate_goals_surface_map_methods() {
    // A parameter is immutable, so the reads are candidates and the
    // mutators are blocked with the reason, not silently dropped.
    let goals = goals_of("fn f(m: Map<String, Int>) -> Option<Int> { ??found }");
    let goal = &goals[0];
    assert!(
        goal.candidates
            .iter()
            .any(|c| c.expression.contains("m.get(key:")),
        "an Option<Int> hole with a Map<String, Int> in scope must offer `get`: {:?}",
        goal.candidates
    );
    assert!(
        goal.candidates
            .iter()
            .all(|c| !c.expression.contains("remove")),
        "{:?}",
        goal.candidates
    );
    assert!(
        goal.blocked
            .iter()
            .any(|b| b.contains("remove") && b.contains("var")),
        "{:?}",
        goal.blocked
    );
}

#[test]
fn map_mutators_surface_only_for_var_bindings() {
    let goals =
        goals_of("fn f() -> Option<Int> { var m: Map<String, Int> = empty_map(); ??found }");
    let found: Vec<&str> = goals[0]
        .candidates
        .iter()
        .map(|c| c.expression.as_str())
        .collect();
    assert!(
        found.iter().any(|c| c.contains("m.remove(key:")),
        "{found:?}"
    );
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
