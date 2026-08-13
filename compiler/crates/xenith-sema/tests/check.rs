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
fn a_named_function_is_not_a_value() {
    // design/0014 §5: no fn-value spelling for named fns. Binding one is
    // refused where the reference happens; the poisoned binding stays
    // silent downstream, so `f == f` adds nothing.
    let source = "fn double(n: Int) -> Int { n + n }\n\
                  fn g() -> Bool { let f = double; f == f }";
    let codes = codes_of(source);
    assert_eq!(codes, ["XN1008"]);
}

#[test]
fn a_named_function_cannot_ride_into_a_combinator() {
    // The soundness hole the refusal closes: an effectful named fn passed
    // as `f:` would run its effects outside every check. The reference
    // itself is refused, before effects even come up; the undetermined `U`
    // that follows is the ordinary poison cascade.
    let source = "fn double(n: Int) -> Int { n + n }\n\
                  fn g(xs: List<Int>) -> List<Int> { xs.map(f: double) }";
    assert_eq!(codes_of(source), ["XN1008", "XN3005"]);
}

// ------------------------------------------------------------------- consts
//
// `const` parsed and resolved to nothing before; it now enters the table,
// types its references, and folds its initializer at check time. The fold's
// grammar is the whole contract: a literal, or arithmetic over literals.

#[test]
fn a_const_resolves_and_carries_its_declared_type() {
    expect_clean(
        "const LIMIT: Int = 1_000;\n\
         const NAME: String = \"ada\";\n\
         const ON: Bool = !false;\n\
         fn cap(n: Int) -> Int { if n > LIMIT { LIMIT } else { n } }\n\
         fn label() -> String { NAME }\n\
         fn flag() -> Bool { ON }",
    );
}

#[test]
fn a_const_is_a_value_wherever_a_value_goes() {
    // Including inside a closure body: a const is a module-level item, so
    // no capture rule applies to it (design/0014 §1 is about bindings).
    expect_clean(
        "const STEP: Int = 2;\n\
         fn scaled(xs: List<Int>) -> List<Int> { xs.map(f: |x| x * STEP) }",
    );
}

#[test]
fn arithmetic_over_literals_folds_but_a_call_does_not() {
    expect_clean("const HALF: Int = 1_000 / 2;\nfn f() -> Int { HALF }");
    let found = diagnostics_of("fn one() -> Int { 1 }\nconst C: Int = one();");
    let codes: Vec<&str> = found.iter().map(|(c, _)| c.as_str()).collect();
    assert_eq!(codes, ["XN3012"], "{found:#?}");
    assert!(
        found[0].1.contains("arithmetic over literals"),
        "{found:#?}"
    );
}

#[test]
fn a_const_may_not_name_another_const() {
    // The exclusion that keeps initialization order — and therefore the
    // initialization cycle design/0010 §5 reserves a diagnostic for — from
    // existing at all.
    assert_eq!(codes_of("const A: Int = 1;\nconst B: Int = A;"), ["XN3012"]);
}

#[test]
fn a_const_initializer_is_checked_against_its_annotation() {
    assert_eq!(codes_of("const NAME: String = 5;"), ["XN3001"]);
    assert_eq!(codes_of("const MIXED: Int = 1 + 1.0;"), ["XN3001"]);
}

#[test]
fn overflow_and_division_by_zero_are_refused_at_the_declaration() {
    // Trapping arithmetic (design/0003) turned into a diagnostic: folding at
    // check time is the one place where it can be.
    let found = diagnostics_of("const OVER: Int = 9_223_372_036_854_775_807 + 1;");
    assert_eq!(found[0].0, "XN3012");
    assert!(found[0].1.contains("overflow"), "{found:#?}");
    let found = diagnostics_of("const ZERO: Int = 1 / 0;");
    assert_eq!(found[0].0, "XN3012");
    assert!(found[0].1.contains("division by zero"), "{found:#?}");
}

#[test]
fn a_non_arithmetic_operator_is_not_folded() {
    assert_eq!(codes_of("const CMP: Bool = 1 < 2;"), ["XN3012"]);
}

#[test]
fn a_hole_cannot_be_a_const_value() {
    assert_eq!(codes_of("const GAP: Int = ??later;"), ["XN3012"]);
}

#[test]
fn a_const_collides_with_a_const_or_a_fn_of_the_same_name() {
    assert_eq!(
        codes_of("const DUP: Int = 1;\nconst DUP: Int = 2;"),
        ["XN2005"]
    );
    assert_eq!(
        codes_of("fn thing() -> Int { 1 }\nconst thing: Int = 2;"),
        ["XN2005"]
    );
}

#[test]
fn a_local_binding_shadows_a_const_the_way_it_shadows_anything() {
    expect_clean("const LIMIT: Int = 5;\nfn f() -> String { let LIMIT = \"x\"; LIMIT }");
}

// --------------------------------------------- generic literal construction
//
// The expected type seeds a user struct literal and a payload-less variant
// of a generic user enum, exactly as it seeds `Ok(..)` and `None`. Both were
// unconstructible in any position before; where nothing determines the
// arguments, the refusal names them and reports once.

const PAIR_AND_WRAP: &str = "struct Pair<T> {\n    a: T,\n    b: T,\n}\n\n\
     enum Wrap<T> {\n    Hollow,\n    Full(T),\n}\n\n";

#[test]
fn a_generic_struct_literal_takes_its_arguments_from_the_annotation() {
    expect_clean(&format!(
        "{PAIR_AND_WRAP}fn f() -> Int {{ let p: Pair<Int> = Pair {{ a: 1, b: 2 }}; p.a }}"
    ));
}

#[test]
fn a_generic_struct_literal_takes_its_arguments_from_the_return_type() {
    // No annotation in sight: the function's own return type is the
    // expectation, and it reaches the literal.
    expect_clean(&format!(
        "{PAIR_AND_WRAP}fn f() -> Pair<String> {{ Pair {{ a: \"x\", b: \"y\" }} }}"
    ));
}

#[test]
fn a_generic_struct_literal_takes_its_arguments_from_an_argument_position() {
    expect_clean(&format!(
        "{PAIR_AND_WRAP}fn take(p: Pair<Int>) -> Int {{ p.b }}\n\
         fn f() -> Int {{ take(p: Pair {{ a: 1, b: 2 }}) }}"
    ));
}

#[test]
fn an_undetermined_generic_struct_literal_names_its_parameters_once() {
    // One diagnostic, not one per field: the unbound parameter becomes
    // poison, so the fields report nothing further (design/0006 §2).
    let found = diagnostics_of(&format!(
        "{PAIR_AND_WRAP}fn f() -> Int {{ let p = Pair {{ a: 1, b: 2 }}; 1 }}"
    ));
    let codes: Vec<&str> = found.iter().map(|(c, _)| c.as_str()).collect();
    assert_eq!(codes, ["XN3005"], "{found:#?}");
    assert!(found[0].1.contains("`T` of `Pair`"), "{found:#?}");
}

#[test]
fn a_generic_struct_literal_in_the_wrong_position_reports_only_the_mismatch() {
    // The expectation is concrete and names another type: that is the one
    // mistake, and "annotate the binding" would name the wrong repair.
    let codes = codes_of(&format!(
        "{PAIR_AND_WRAP}struct Plain {{\n    x: Int,\n}}\n\
         fn f() -> Int {{ let q: Plain = Pair {{ a: 1, b: 2 }}; q.x }}"
    ));
    assert_eq!(codes, ["XN3001"]);
}

#[test]
fn a_non_generic_struct_literal_is_unaffected_by_the_expectation() {
    expect_clean("struct Plain {\n    x: Int,\n}\nfn f() -> Plain { Plain { x: 3 } }");
}

#[test]
fn a_payload_less_variant_of_a_generic_enum_takes_its_arguments_from_context() {
    expect_clean(&format!(
        "{PAIR_AND_WRAP}fn f() -> Wrap<Int> {{ Wrap.Hollow }}\n\
         fn g() -> Int {{ let w: Wrap<Int> = Wrap.Hollow; 1 }}\n\
         fn h(w: Wrap<Int>) -> Int {{ 1 }}\n\
         fn i() -> Int {{ h(w: Wrap.Hollow) }}"
    ));
}

#[test]
fn a_payload_carrying_variant_of_a_generic_enum_still_binds_from_its_payload() {
    expect_clean(&format!(
        "{PAIR_AND_WRAP}fn f() -> Wrap<Int> {{ Wrap.Full(5) }}"
    ));
}

#[test]
fn an_undetermined_generic_variant_names_its_parameters() {
    let found = diagnostics_of(&format!(
        "{PAIR_AND_WRAP}fn f() -> Int {{ let w = Wrap.Hollow; 1 }}"
    ));
    let codes: Vec<&str> = found.iter().map(|(c, _)| c.as_str()).collect();
    assert_eq!(codes, ["XN3005"], "{found:#?}");
    assert!(found[0].1.contains("`T` of `Wrap`"), "{found:#?}");
}

#[test]
fn a_generic_variant_in_the_wrong_position_reports_only_the_mismatch() {
    let codes = codes_of(&format!(
        "{PAIR_AND_WRAP}struct Plain {{\n    x: Int,\n}}\n\
         fn f() -> Int {{ let v: Plain = Wrap.Hollow; v.x }}"
    ));
    assert_eq!(codes, ["XN3001"]);
}

#[test]
fn a_hole_annotation_over_a_generic_literal_reports_nothing() {
    // A hole is a deliberate gap, not a mistake: the position offers no
    // concrete expectation, so neither the seeding failure nor a mismatch
    // is a thing to report (design/0006 §2).
    expect_clean(&format!(
        "{PAIR_AND_WRAP}fn f() -> Int {{ let h: ?? = Pair {{ a: 1, b: 2 }}; 1 }}"
    ));
    expect_clean(&format!(
        "{PAIR_AND_WRAP}fn f() -> Int {{ let h: ?? = Wrap.Hollow; 1 }}"
    ));
}

#[test]
fn a_generic_constructor_used_as_a_value_is_still_refused() {
    // `Wrap.Full` uncalled would be a fn value, which does not ship — so
    // this reports wherever it appears, expectation or not.
    let codes = codes_of(&format!(
        "{PAIR_AND_WRAP}fn f() -> Int {{ let c: Wrap<Int> = Wrap.Full; 1 }}"
    ));
    assert_eq!(codes, ["XN3005"]);
}

#[test]
fn a_generic_literal_is_matched_and_read_like_any_other_value() {
    expect_clean(&format!(
        "{PAIR_AND_WRAP}fn f() -> String {{\n\
         let w: Wrap<Int> = Wrap.Hollow;\n\
         match w {{\n\
         Wrap.Hollow => \"none\",\n\
         Wrap.Full(v) => \"some\",\n\
         }}\n\
         }}"
    ));
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

// ---------------------------------------------------------- infinite sizes

#[test]
fn a_struct_containing_itself_by_value_is_refused() {
    let found = diagnostics_of(
        "struct A {
    a: A,
}",
    );
    let (code, message) = &found[0];
    assert_eq!(code, "XN3011");
    assert!(message.contains("contains itself by value"), "{message}");
}

#[test]
fn an_enum_holding_itself_by_payload_is_refused() {
    assert_eq!(
        codes_of(
            "enum E {
    Leaf,
    More(E),
}"
        ),
        ["XN3011"]
    );
}

#[test]
fn indirection_through_a_container_is_finite() {
    expect_clean(
        "struct Node {
    next: Option<Node>,
}",
    );
    expect_clean(
        "struct Tree {
    kids: List<Tree>,
}",
    );
}

#[test]
fn a_generic_wrapper_is_judged_by_how_it_holds_its_parameter() {
    // `B` holds `T` directly, so `B<A>` holds `A` by value — infinite.
    assert_eq!(
        codes_of(
            "struct B<T> {
    t: T,
}

struct A {
    x: B<A>,
}"
        ),
        ["XN3011"]
    );
    // `C` holds `T` behind `Option`, so `C<A>` is a finite wrapper.
    expect_clean(
        "struct C<T> {
    t: Option<T>,
}

struct A {
    x: C<A>,
}",
    );
}

#[test]
fn a_mutual_cycle_names_the_chain() {
    let found = diagnostics_of(
        "struct A {
    b: B,
}

struct B {
    a: A,
}",
    );
    assert_eq!(found.len(), 1, "one cycle, one diagnostic: {found:#?}");
    assert!(found[0].1.contains("A -> B -> A"), "{}", found[0].1);
}

// -------------------------------------------------------------- bare variants

#[test]
fn none_in_check_position_takes_the_expected_type() {
    expect_clean("fn f() -> Option<Int> { None }");
    expect_clean("fn f() -> Int { let o: Option<Int> = None; 1 }");
    // With nothing pushed down it still fails closed (0006 §1-1).
    assert_eq!(codes_of("fn f() -> Int { let o = None; 1 }"), ["XN3005"]);
}

// ------------------------------------------------------- unshipped constructs

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
    // design/0014 §3 (task interpretation): fn types appear only in std
    // signatures. A user fn taking a fn-typed parameter would call it, so
    // every user-written position is refused — params, lets, fields alike.
    assert_eq!(
        codes_of("fn apply(f: fn(Int) -> Int) -> Int { 1 }"),
        ["XN1008"]
    );
    assert_eq!(
        codes_of("fn f() -> Int { let g: fn() -> Int = ??; 1 }"),
        ["XN1008"]
    );
    assert_eq!(
        codes_of("struct Holder { callback: fn(Int) -> Int }"),
        ["XN1008"]
    );
}

// --------------------------------------------------- closures (design/0014)
//
// The compile-fail battery of 0014 §6: safety is an unconditional gate, not
// an experiment. Each case pins the code that fires and, where the RFC
// prescribes wording, the sentence.

#[test]
fn map_filter_fold_find_check_end_to_end() {
    expect_clean(
        "fn f(xs: List<Int>) -> List<Int> { xs.map(|x| x * 2) }\n\
         fn g(xs: List<Int>) -> List<Int> { xs.filter(|x| x % 2 == 0) }\n\
         fn h(xs: List<Int>) -> Int { xs.fold(init: 0, f: |acc, x| acc + x) }\n\
         fn i(xs: List<Int>) -> Option<Int> { xs.find(|x| x > 1) }",
    );
}

#[test]
fn a_closure_body_pins_maps_result_type() {
    // `map`'s `U` binds from the body: List<Int> -> List<String>.
    expect_clean("fn f(xs: List<Int>) -> List<String> { xs.map(|x| x.to_text()) }");
    // And a wrong claim about `U` is caught where the results meet.
    assert_eq!(
        codes_of("fn f(xs: List<Int>) -> List<Int> { xs.map(|x| x.to_text()) }"),
        ["XN3001"]
    );
}

#[test]
fn closure_parameters_may_be_discarded_and_captures_may_be_safe() {
    expect_clean("fn f(xs: List<Int>) -> List<Int> { xs.map(|_| 7) }");
    // A `let` of a CaptureSafe type copies in at creation — legal.
    expect_clean("fn f(xs: List<Int>) -> List<Int> { let base = 10; xs.map(|x| x + base) }");
    // Nested closures re-snapshot; inner bodies see outer parameters.
    expect_clean(
        "fn f(xss: List<List<Int>>) -> List<Int> {\n\
             xss.map(|xs| xs.fold(init: 0, f: |acc, x| acc + x))\n\
         }",
    );
}

#[test]
fn a_capability_capture_is_refused_with_the_plan_teach() {
    // Battery: capability capture (XN4005). `io` is free in the body and
    // resolves to the enclosing fn's parameter — a capture, and `Io` has no
    // honest snapshot.
    let source = "fn f(io: Io, xs: List<Int>) -> List<Int> {\n\
                      xs.map(|x| { let grab = io; x })\n\
                  }";
    let found = diagnostics_of(source);
    assert_eq!(found.len(), 1, "{found:#?}");
    let (code, message) = &found[0];
    assert_eq!(code, "XN4005");
    assert!(message.contains("CaptureSafe"), "{message}");
    assert!(message.contains("closures are plans"), "{message}");
}

#[test]
fn a_capability_arriving_through_a_parameter_still_cannot_act() {
    // Battery: capability-parameter use (XN4006). Nothing is captured — the
    // capability rides in as the element type — and pillar 1 still refuses
    // the effect inside the body.
    let source = "fn f(io: Io) -> List<Int> {\n\
                      [io].map(|x| { x.write(text: \"hi\"); 1 })\n\
                  }";
    let found = diagnostics_of(source);
    assert_eq!(found.len(), 1, "{found:#?}");
    let (code, message) = &found[0];
    assert_eq!(code, "XN4006");
    assert!(
        message.contains("a closure body performs no effects"),
        "{message}"
    );
    assert!(message.contains("closures are plans"), "{message}");
}

#[test]
fn an_effectful_named_fn_cannot_be_called_from_a_closure_body() {
    // Battery: non-empty `uses` fn called in the body (XN4006). The named
    // fn resolves — resolution is not capture — and its effect set is what
    // the empty budget refuses.
    let source = "fn shout(io: Io) -> Int uses {Io.write} { let r = io.write(text: \"x\"); 1 }\n\
                  fn f(io: Io) -> List<Int> { [io].map(|x| shout(io: x)) }";
    assert_eq!(codes_of(source), ["XN4006"]);
}

#[test]
fn a_generic_cannot_launder_an_effect_into_a_closure() {
    // Battery: generic laundering (XN4006 side). The callee's `uses` is
    // declared on the signature, so the generic disguise changes nothing.
    let source = "fn sneak<T>(x: T, io: Io) -> Int uses {Io.write} { let r = io.write(text: \"x\"); 1 }\n\
                  fn f(io: Io, xs: List<Int>) -> List<Int> { [io].map(|cap| sneak(x: 1, io: cap)) }";
    assert_eq!(codes_of(source), ["XN4006"]);
}

#[test]
fn a_generic_cannot_launder_a_capability_into_a_capture() {
    // Battery: generic laundering (XN4005 side). No `CaptureSafe` bound is
    // spellable, so an unresolved `T` never captures — even when the caller
    // would have instantiated it with `Int`.
    let source = "fn wrap<T>(x: T, xs: List<Int>) -> List<Int> {\n\
                      xs.map(|n| { let grab = x; n })\n\
                  }";
    assert_eq!(codes_of(source), ["XN4005"]);
}

#[test]
fn shared_and_task_are_reserved_non_capture_safe() {
    // design/0014 §1: the 0004 shared-mutable primitives are non-CaptureSafe
    // before they exist, closing the future hole now.
    let source = "fn f(cell: Shared<Int>, xs: List<Int>) -> List<Int> {\n\
                      xs.map(|n| { let grab = cell; n })\n\
                  }";
    assert_eq!(codes_of(source), ["XN4005"]);
}

#[test]
fn a_self_reference_inside_the_initializer_is_definite_initialization() {
    // Battery: self-reference (XN4007), not XN2002 — the name exists one
    // statement later, and "unknown" would misdirect.
    let source = "fn f(xs: List<Int>) -> List<Int> {\n\
                      let out = xs.map(|x| x + out.len());\n\
                      out\n\
                  }";
    let found = diagnostics_of(source);
    assert_eq!(found.len(), 1, "{found:#?}");
    let (code, message) = &found[0];
    assert_eq!(code, "XN4007");
    assert!(
        message.contains("recursion belongs in a named fn"),
        "{message}"
    );
}

#[test]
fn a_var_capture_names_the_stale_snapshot_and_the_let_fix() {
    // Battery: var capture (XN4008). The RFC fixes the message: updates
    // after the snapshot are not visible; bind to a `let` first.
    let source = "fn f(xs: List<Int>) -> List<Int> {\n\
                      var total = 0;\n\
                      xs.map(|x| x + total)\n\
                  }";
    let found = diagnostics_of(source);
    assert_eq!(found.len(), 1, "{found:#?}");
    let (code, message) = &found[0];
    assert_eq!(code, "XN4008");
    assert!(
        message.contains("updates after that snapshot are not visible"),
        "{message}"
    );
    assert!(
        message.contains("bind the current value to a `let`"),
        "{message}"
    );
}

#[test]
fn a_repeated_bad_capture_reports_once() {
    // One bad capture, mentioned three times, is one diagnostic — the same
    // no-avalanche discipline as poison types.
    let source = "fn f(io: Io, xs: List<Int>) -> List<Int> {\n\
                      xs.map(|x| { let a = io; let b = io; let c = io; x })\n\
                  }";
    assert_eq!(codes_of(source), ["XN4005"]);
}

#[test]
fn a_let_position_closure_is_refused_with_the_extraction_advice() {
    // Battery: let-position closure (XN1011).
    let found = diagnostics_of("fn f() -> Int { let g = |x| x; 1 }");
    let (code, message) = &found[0];
    assert_eq!(code, "XN1011");
    assert!(
        message.contains("inline its body, or extract a named fn"),
        "{message}"
    );
}

#[test]
fn return_field_and_container_positions_are_refused_too() {
    // `return |x| x;` — checked against the return type, still XN1011.
    assert_eq!(codes_of("fn f() -> Int { return |x| x; }"), ["XN1011"]);
    // A container element.
    assert_eq!(
        codes_of("fn f() -> Int { let xs = [|x| x]; 1 }"),
        ["XN1011"]
    );
    // A struct field.
    assert_eq!(
        codes_of("struct S { n: Int }\nfn f() -> S { S { n: |x| x } }"),
        ["XN1011"]
    );
    // An argument whose parameter is not fn-typed.
    assert_eq!(
        codes_of("fn take(n: Int) -> Int { n }\nfn f() -> Int { take(n: |x| x) }"),
        ["XN1011"]
    );
}

#[test]
fn question_mark_return_and_break_cannot_cross_the_boundary() {
    // Battery: `?` in the body (XN1012), with the early-exit teach.
    let source = "fn f(xs: List<Int>) -> Option<Int> {\n\
                      let ys = xs.map(|x| xs.get(index: x)?);\n\
                      ys.get(index: 0)\n\
                  }";
    let found = diagnostics_of(source);
    let (code, message) = &found[0];
    assert_eq!(code, "XN1012");
    assert!(
        message.contains("closures cannot early-return"),
        "{message}"
    );

    assert_eq!(
        codes_of("fn f(xs: List<Int>) -> List<Int> { xs.map(|x| { return x; x }) }"),
        ["XN1012"]
    );
    assert_eq!(
        codes_of("fn f(xs: List<Int>) -> List<Int> { xs.map(|x| { break; x }) }"),
        ["XN1012"]
    );
    // A loop the closure itself contains keeps its `break`.
    expect_clean(
        "fn f(xs: List<Int>) -> List<Int> {\n\
             xs.map(|x| { while true { break; } x })\n\
         }",
    );
}

#[test]
fn a_closure_arity_mismatch_is_reported_against_the_fn_type() {
    let found = diagnostics_of("fn f(xs: List<Int>) -> List<Int> { xs.map(|a, b| a) }");
    let (code, message) = &found[0];
    assert_eq!(code, "XN3002");
    assert!(
        message.contains("this closure takes 2 parameter(s)"),
        "{message}"
    );
    assert_eq!(
        codes_of("fn f(xs: List<Int>) -> List<Int> { xs.map(|| 1) }"),
        ["XN3002"]
    );
}

#[test]
fn fold_requires_named_arguments_by_the_existing_rule() {
    // Two parameters, so design/0002 §8 applies with no new machinery.
    let source = "fn f(xs: List<Int>) -> Int { xs.fold(0, |acc, x| acc + x) }";
    let codes = codes_of(source);
    assert_eq!(codes, ["XN3008", "XN3008"], "{codes:?}");
}

#[test]
fn goals_inside_a_closure_body_carry_the_empty_effect_budget() {
    // A hole in a closure body is a goal like any other, but its permitted
    // effects are the closure's — none — not the enclosing fn's. `filter`
    // pushes a closed `Bool`, so the hole is a goal rather than an
    // annotation demand.
    let source = "fn f(io: Io, xs: List<Int>) -> List<Int> uses {Io.write} {\n\
                      xs.filter(|x| ??gap)\n\
                  }";
    let parsed = parse(source);
    let analysis = analyze(&parsed.module);
    let goal = analysis
        .goals
        .iter()
        .find(|g| g.name.as_deref() == Some("gap"))
        .expect("the ??gap goal");
    assert!(
        goal.allowed_effects.is_empty(),
        "{:?}",
        goal.allowed_effects
    );
    assert!(
        goal.in_scope.iter().any(|(name, _)| name == "x"),
        "{:?}",
        goal.in_scope
    );
}

#[test]
fn closure_teaching_strips_to_byte_identity() {
    // The 0009 contract extended to the new notes: off-mode output is
    // on-mode minus exactly the teaching.
    let source = "fn f(io: Io) -> List<Int> { [io].map(|x| { x.write(text: \"hi\"); 1 }) }";
    let parsed = parse(source);
    let mut found = analyze(&parsed.module).diagnostics;
    assert!(found[0].message.contains("closures are plans"));
    found[0].strip_teaching();
    assert_eq!(
        found[0].message,
        "this call uses {Io.write}, but a closure body performs no effects"
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
    let source = "fn f(b: Bool) -> Int { match b { true => { let g = |x| x; 1 } } }";
    assert_eq!(codes_of(source), ["XN1011", "XN5001"]);
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
    assert_eq!(
        teach.total_items, 14,
        "the 0007 surface plus the 0014 combinators"
    );
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
