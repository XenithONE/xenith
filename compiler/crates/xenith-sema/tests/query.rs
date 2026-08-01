use xenith_sema::{producers, type_at};
use xenith_syntax::parse;

/// Probe at the byte offset of `marker`'s first occurrence in `source`.
fn probe_at(source: &str, marker: &str) -> Option<xenith_sema::Probe> {
    let offset = source
        .find(marker)
        .unwrap_or_else(|| panic!("marker {marker:?} not in source")) as u32;
    let parsed = parse(source);
    type_at(&parsed.module, offset)
}

// ------------------------------------------------------------------ type-at

#[test]
fn a_literal_answers_with_its_own_type() {
    let source = "fn f() -> Int { 42 }";
    let probe = probe_at(source, "42").expect("probe");
    assert_eq!(probe.ty, "Int");
    assert_eq!(probe.enclosing_function, "f");
}

#[test]
fn a_name_answers_with_the_bindings_type() {
    let source = "struct Player { name: String }\n\
                  fn f(player: Player) -> String { player.name }";
    let probe = probe_at(source, "player.name").expect("probe");
    // Innermost expression at the start of `player.name` is `player` itself.
    assert_eq!(probe.ty, "Player");
}

#[test]
fn the_innermost_expression_wins() {
    let source = "fn f(a: Int, b: Int) -> Int { a + b }";
    let probe = probe_at(source, "b }").expect("probe");
    assert_eq!(probe.ty, "Int", "the operand, not the whole addition");
}

#[test]
fn a_hole_answers_with_the_required_type() {
    let source = "enum ApiError { Down }\n\
                  fn f() -> Result<Int, ApiError> { ??body }";
    let probe = probe_at(source, "??body").expect("probe");
    assert_eq!(probe.ty, "Result<Int, ApiError>");
}

#[test]
fn the_scope_reflects_the_position_not_the_whole_function() {
    let source = "fn f() -> Int { let early = 1; let late = early + 1; late }";
    let probe = probe_at(source, "early + 1").expect("probe");
    let names: Vec<&str> = probe.in_scope.iter().map(|(n, _)| n.as_str()).collect();
    assert!(names.contains(&"early"), "{names:?}");
    assert!(
        !names.contains(&"late"),
        "a binding must not be in scope inside its own initialiser: {names:?}"
    );
}

#[test]
fn the_probe_carries_the_effect_budget() {
    let source = "fn f(io: Io) -> Result<Unit, Error> uses {Io.write} { io.write(text: \"hi\") }";
    let probe = probe_at(source, "io.write").expect("probe");
    assert_eq!(probe.allowed_effects, ["Io.write"]);
}

#[test]
fn a_let_binding_name_answers_with_the_bound_type() {
    // The most natural query of all: "what is this variable?"
    let source = "fn f() -> Int { let total = 1 + 2; total }";
    let probe = probe_at(source, "total").expect("probe");
    assert_eq!(probe.ty, "Int");
}

#[test]
fn a_position_outside_any_expression_answers_nothing() {
    let source = "fn f() -> Int { 42 }";
    // Offset 0 is the `fn` keyword.
    let parsed = parse(source);
    assert!(type_at(&parsed.module, 0).is_none());
}

#[test]
fn partial_programs_still_answer() {
    // The half-written call parses with recovery; the probe on the argument
    // `count` must still know its type. The last occurrence is the argument
    // (the first is the parameter declaration).
    let source = "fn f(count: Int) -> Int { g(count";
    let offset = source.rfind("count").expect("marker") as u32;
    let parsed = parse(source);
    let probe = type_at(&parsed.module, offset).expect("a broken program still answers");
    assert_eq!(probe.ty, "Int");
}

// ---------------------------------------------------------------- producers

fn producer_signatures(source: &str, ty: &str) -> Vec<String> {
    let parsed = parse(source);
    producers(&parsed.module, ty)
        .expect("type must parse")
        .into_iter()
        .map(|p| p.signature)
        .collect()
}

#[test]
fn functions_variants_and_the_expected_shape_are_listed() {
    let source = "struct Player { name: String }\n\
                  enum ScoreError { Overflow }\n\
                  fn try_award(player: Player, points: Int) -> Result<Player, ScoreError> { ??x }\n\
                  fn unrelated() -> Int { 1 }";
    let found = producer_signatures(source, "Result<Player, ScoreError>");
    assert!(
        found.contains(
            &"try_award(player: Player, points: Int) -> Result<Player, ScoreError>".to_string()
        ),
        "{found:?}"
    );
    assert!(found.contains(&"Ok(Player)".to_string()), "{found:?}");
    assert!(found.contains(&"Err(ScoreError)".to_string()), "{found:?}");
    assert!(
        found.iter().all(|s| !s.starts_with("unrelated")),
        "{found:?}"
    );
}

#[test]
fn generic_functions_are_instantiated_in_the_answer() {
    let source = "fn first<T>(items: List<T>) -> Option<T> { ??x }";
    let found = producer_signatures(source, "Option<Int>");
    assert!(
        found.contains(&"first(items: List<Int>) -> Option<Int>".to_string()),
        "{found:?}"
    );
}

#[test]
fn effects_appear_in_the_signature() {
    let source = "fn read_all(io: Io) -> String uses {Io.read} { \"\" }";
    let found = producer_signatures(source, "String");
    assert!(
        found
            .iter()
            .any(|s| s.starts_with("read_all") && s.contains("uses {Io.read}")),
        "{found:?}"
    );
}

#[test]
fn an_async_function_produces_a_task_not_its_inner_type() {
    let source = "async fn fetch() -> Int { 1 }";
    assert!(
        producer_signatures(source, "Int")
            .iter()
            .all(|s| !s.starts_with("fetch")),
        "calling an async fn yields Task<Int>, not Int"
    );
    let found = producer_signatures(source, "Task<Int>");
    assert!(
        found.contains(&"fetch() -> Task<Int>".to_string()),
        "{found:?}"
    );
}

#[test]
fn a_struct_type_lists_its_literal_shape() {
    let source = "struct Player { name: String, var score: Int }";
    let found = producer_signatures(source, "Player");
    assert!(
        found.contains(&"Player { name: String, score: Int }".to_string()),
        "{found:?}"
    );
}

#[test]
fn an_unknown_type_is_an_error_not_an_empty_list() {
    // Empty would read as "nothing produces this", which is a different and
    // wrong claim.
    let parsed = parse("fn f() -> Int { 1 }");
    let result = producers(&parsed.module, "Mystery");
    assert!(result.is_err());
}

#[test]
fn malformed_type_text_is_an_error() {
    let parsed = parse("fn f() -> Int { 1 }");
    assert!(producers(&parsed.module, "Result<").is_err());
}
