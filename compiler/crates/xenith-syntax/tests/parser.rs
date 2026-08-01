use xenith_diag::DiagCode;
use xenith_syntax::ast::*;
use xenith_syntax::parse;

// ------------------------------------------------------------------ helpers

fn codes(source: &str) -> Vec<&'static str> {
    parse(source)
        .diagnostics
        .iter()
        .map(|d| d.code.id())
        .collect()
}

fn expect_clean(source: &str) -> Module {
    let parsed = parse(source);
    assert!(
        parsed.diagnostics.is_empty(),
        "expected a clean parse, got {:#?}",
        parsed
            .diagnostics
            .iter()
            .map(|d| (d.code.id(), d.message.clone()))
            .collect::<Vec<_>>()
    );
    parsed.module
}

/// Parse a single expression by wrapping it in a function body.
fn expr_of(source: &str) -> Expr {
    let wrapped = format!("fn f() {{ {source} }}");
    let module = expect_clean(&wrapped);
    let ItemKind::Fn(f) = &module.items[0].kind else {
        panic!("expected a function");
    };
    *f.body
        .as_ref()
        .expect("body")
        .tail
        .clone()
        .expect("tail expression")
}

/// Render an expression fully parenthesised, so precedence and associativity
/// are visible in a single string rather than inferred from tree shape.
fn render(expr: &Expr) -> String {
    match &expr.kind {
        ExprKind::Int(v) | ExprKind::Float(v) => v.clone(),
        ExprKind::Str(v) | ExprKind::Char(v) => v.clone(),
        ExprKind::Bool(v) => v.to_string(),
        ExprKind::Unit => "unit".to_string(),
        ExprKind::Path(p) => p
            .segments
            .iter()
            .map(|s| s.name.as_str())
            .collect::<Vec<_>>()
            .join("."),
        ExprKind::Hole { name } => match name {
            Some(n) => format!("??{n}"),
            None => "??".to_string(),
        },
        ExprKind::Unary { op, operand } => {
            let symbol = match op {
                UnaryOp::Neg => "-",
                UnaryOp::Not => "!",
            };
            format!("({symbol}{})", render(operand))
        }
        ExprKind::Binary { op, lhs, rhs } => {
            format!("({} {} {})", render(lhs), op.symbol(), render(rhs))
        }
        ExprKind::Assign { target, op, value } => {
            let symbol = op.map(|o| format!("{}=", o.symbol())).unwrap_or("=".into());
            format!("({} {symbol} {})", render(target), render(value))
        }
        ExprKind::Call { callee, args } => {
            format!("{}({})", render(callee), render_args(args))
        }
        ExprKind::MethodCall {
            receiver,
            method,
            args,
        } => format!(
            "{}.{}({})",
            render(receiver),
            method.name,
            render_args(args)
        ),
        ExprKind::Field { receiver, name } => format!("{}.{}", render(receiver), name.name),
        ExprKind::Await(inner) => format!("{}.await", render(inner)),
        ExprKind::Try(inner) => format!("{}?", render(inner)),
        ExprKind::If { .. } => "if".to_string(),
        ExprKind::Match { .. } => "match".to_string(),
        ExprKind::Block(_) => "block".to_string(),
        ExprKind::StructLit { path, .. } => format!(
            "{}{{}}",
            path.segments
                .iter()
                .map(|s| s.name.as_str())
                .collect::<Vec<_>>()
                .join(".")
        ),
        ExprKind::Lambda { .. } => "lambda".to_string(),
        ExprKind::Error => "<error>".to_string(),
    }
}

fn render_args(args: &[Arg]) -> String {
    args.iter()
        .map(|a| match &a.name {
            Some(n) => format!("{}: {}", n.name, render(&a.value)),
            None => render(&a.value),
        })
        .collect::<Vec<_>>()
        .join(", ")
}

// -------------------------------------------------------------- whole module

#[test]
fn a_representative_program_parses_cleanly() {
    let source = r#"
use std.io;

const LIMIT: Int = 1_000;

/// A player in the current match.
struct Player {
    name: String,
    var score: Int,
}

enum Rank {
    Bronze,
    Silver,
    Gold,
}

enum ScoreError {
    Overflow,
    NotFound(Int),
}

fn rank_of(score: Int) -> Rank {
    match score {
        s if s >= 1000 => Rank.Gold,
        s if s >= 100 => Rank.Silver,
        _ => Rank.Bronze,
    }
}

fn award(player: Player, points: Int) -> Result<Player, ScoreError> {
    let total = player.score.checked_add(other: points)
        .to_result(error: ScoreError.Overflow)?;

    var updated = player;
    updated.score = total;
    Ok(updated)
}

async fn main(io: Io) -> Result<Unit, Error> uses {Io.write} {
    let player = Player { name: "ada", score: 0 };
    let awarded = award(player: player, points: 120)?;
    io.write(text: awarded.name)?;
    return unit;
}
"#;
    let module = expect_clean(source);
    // use, const, struct, two enums, and three functions.
    assert_eq!(module.items.len(), 8);
}

#[test]
fn documentation_attaches_to_the_following_item() {
    let module = expect_clean("/// One\n/// Two\nfn f() {}\n");
    assert_eq!(module.items[0].docs.len(), 2);
}

#[test]
fn a_blank_line_detaches_documentation() {
    // Otherwise a file header would become documentation for whatever
    // declaration happens to come first.
    let module = expect_clean("/// A file header\n\nfn f() {}\n");
    assert!(module.items[0].docs.is_empty());
}

// -------------------------------------------------------------- declarations

#[test]
fn a_function_records_effects_generics_and_async() {
    let module = expect_clean(
        "async fn fetch<T>(net: Net, url: Url) -> Result<T, Error> uses {Net.get, Net.send} {}",
    );
    let ItemKind::Fn(f) = &module.items[0].kind else {
        panic!("expected a function");
    };
    assert!(f.is_async);
    assert_eq!(f.generics.len(), 1);
    assert_eq!(f.params.len(), 2);
    assert!(f.return_type.is_some());
    let effects = f.effects.as_ref().expect("effect set");
    assert_eq!(effects.effects.len(), 2);
    assert_eq!(effects.effects[0].segments[0].name, "Net");
    assert_eq!(effects.effects[0].segments[1].name, "get");
}

#[test]
fn an_absent_uses_clause_is_the_empty_effect_set() {
    // The strongest claim a signature can make: this function performs no
    // effects at all.
    let module = expect_clean("fn add(a: Int, b: Int) -> Int { a + b }");
    let ItemKind::Fn(f) = &module.items[0].kind else {
        panic!("expected a function");
    };
    assert!(f.effects.is_none());
}

#[test]
fn struct_fields_are_immutable_unless_marked_var() {
    let module = expect_clean("struct P { name: String, var score: Int }");
    let ItemKind::Struct(s) = &module.items[0].kind else {
        panic!("expected a struct");
    };
    assert!(!s.fields[0].mutable);
    assert!(s.fields[1].mutable);
}

#[test]
fn enum_variants_may_carry_a_payload() {
    let module = expect_clean("enum E { None, One(Int), Two(Int, String) }");
    let ItemKind::Enum(e) = &module.items[0].kind else {
        panic!("expected an enum");
    };
    assert_eq!(e.variants[0].payload.len(), 0);
    assert_eq!(e.variants[1].payload.len(), 1);
    assert_eq!(e.variants[2].payload.len(), 2);
}

// ---------------------------------------------------------------- precedence

#[test]
fn arithmetic_binds_tighter_than_comparison() {
    assert_eq!(render(&expr_of("a + b * c")), "(a + (b * c))");
    assert_eq!(render(&expr_of("a * b + c")), "((a * b) + c)");
    assert_eq!(render(&expr_of("a + b == c")), "((a + b) == c)");
}

#[test]
fn bitwise_binds_tighter_than_comparison_as_in_rust() {
    // The C ordering, where `a & b == c` means `a & (b == c)`, is a well-known
    // trap. Rust's ordering is used instead -- deviating would also cost
    // transfer for no benefit.
    assert_eq!(render(&expr_of("a & b == c")), "((a & b) == c)");
    assert_eq!(render(&expr_of("a | b & c")), "(a | (b & c))");
    assert_eq!(render(&expr_of("a ^ b | c")), "((a ^ b) | c)");
}

#[test]
fn logical_and_binds_tighter_than_logical_or() {
    assert_eq!(render(&expr_of("a || b && c")), "(a || (b && c))");
}

#[test]
fn binary_operators_are_left_associative() {
    assert_eq!(render(&expr_of("a - b - c")), "((a - b) - c)");
    assert_eq!(render(&expr_of("a / b / c")), "((a / b) / c)");
}

#[test]
fn assignment_is_right_associative_and_lowest() {
    assert_eq!(render(&expr_of("a = b + c")), "(a = (b + c))");
    assert_eq!(render(&expr_of("a = b = c")), "(a = (b = c))");
    assert_eq!(render(&expr_of("a += b * c")), "(a += (b * c))");
}

#[test]
fn unary_binds_tighter_than_binary() {
    assert_eq!(render(&expr_of("-a + b")), "((-a) + b)");
    assert_eq!(render(&expr_of("!a && b")), "((!a) && b)");
}

#[test]
fn parentheses_override_precedence() {
    assert_eq!(render(&expr_of("(a + b) * c")), "((a + b) * c)");
}

#[test]
fn shift_is_recognised_from_two_adjacent_angle_brackets() {
    // The lexer never emits `>>`, so the parser reconstitutes a shift only
    // when the two tokens touch. That is what lets `List<Int>>` inside a
    // generic close two levels instead of becoming one shift operator.
    assert_eq!(render(&expr_of("a << b")), "(a << b)");
    assert_eq!(render(&expr_of("a >> b")), "(a >> b)");
}

#[test]
fn shift_binds_tighter_than_comparison() {
    assert_eq!(render(&expr_of("a << b == c")), "((a << b) == c)");
}

// ---------------------------------------------------------------- postfix

#[test]
fn method_calls_chain_left_to_right() {
    assert_eq!(render(&expr_of("a.first().second()")), "a.first().second()");
}

#[test]
fn try_and_await_are_postfix_and_compose() {
    assert_eq!(render(&expr_of("f().await?")), "f().await?");
    assert_eq!(render(&expr_of("a.b()?.c")), "a.b()?.c");
}

#[test]
fn field_access_is_distinct_from_a_method_call() {
    let ExprKind::Field { .. } = expr_of("player.score").kind else {
        panic!("expected a field access");
    };
    let ExprKind::MethodCall { .. } = expr_of("player.score()").kind else {
        panic!("expected a method call");
    };
}

// ------------------------------------------------------------------ arguments

#[test]
fn named_and_positional_arguments_both_parse() {
    // The rule requiring names once a call takes two or more is enforced later,
    // where the callee's parameter names are known and the fix can name them.
    assert_eq!(render(&expr_of("f(x)")), "f(x)");
    assert_eq!(
        render(&expr_of("rect(width: 100, height: 200)")),
        "rect(width: 100, height: 200)"
    );
}

#[test]
fn an_argument_name_is_recorded_separately_from_its_value() {
    let ExprKind::Call { args, .. } = expr_of("f(count: 3)").kind else {
        panic!("expected a call");
    };
    assert_eq!(
        args[0].name.as_ref().map(|n| n.name.as_str()),
        Some("count")
    );
}

// ---------------------------------------------------------------------- holes

#[test]
fn holes_parse_in_expression_position_without_a_diagnostic() {
    assert!(parse("fn f() -> Int { ?? }").diagnostics.is_empty());
    assert!(parse("fn f() -> Int { ??result }").diagnostics.is_empty());
}

#[test]
fn a_named_hole_keeps_its_name() {
    let ExprKind::Hole { name } = expr_of("??response").kind else {
        panic!("expected a hole");
    };
    assert_eq!(name.as_deref(), Some("response"));
}

#[test]
fn an_anonymous_hole_has_no_name() {
    let ExprKind::Hole { name } = expr_of("??").kind else {
        panic!("expected a hole");
    };
    assert_eq!(name, None);
}

#[test]
fn holes_parse_in_type_position() {
    let module = expect_clean("fn f(x: ??) -> ??ret { ?? }");
    let ItemKind::Fn(f) = &module.items[0].kind else {
        panic!("expected a function");
    };
    assert!(matches!(f.params[0].ty.kind, TypeKind::Hole { name: None }));
    let TypeKind::Hole { name } = &f.return_type.as_ref().expect("return type").kind else {
        panic!("expected a hole type");
    };
    assert_eq!(name.as_deref(), Some("ret"));
}

// ------------------------------------------------------------------ generics

#[test]
fn generic_parameters_record_their_bounds() {
    let module = expect_clean("fn get<K: Eq + Hash, V>(map: Map<K, V>, key: K) -> Option<V> {}");
    let ItemKind::Fn(f) = &module.items[0].kind else {
        panic!("expected a function");
    };
    assert_eq!(f.generics.len(), 2);
    assert_eq!(f.generics[0].name.name, "K");
    assert_eq!(
        f.generics[0]
            .bounds
            .iter()
            .map(|b| b.name.as_str())
            .collect::<Vec<_>>(),
        ["Eq", "Hash"]
    );
    assert!(f.generics[1].bounds.is_empty());
}

#[test]
fn bounds_parse_on_structs_and_enums_too() {
    let module = expect_clean("struct Cache<K: Hash, V> { key: K, value: V }");
    let ItemKind::Struct(s) = &module.items[0].kind else {
        panic!("expected a struct");
    };
    assert_eq!(s.generics[0].bounds.len(), 1);

    let module = expect_clean("enum Tree<T: Ord> { Leaf, Node(T) }");
    let ItemKind::Enum(e) = &module.items[0].kind else {
        panic!("expected an enum");
    };
    assert_eq!(e.generics[0].bounds[0].name, "Ord");
}

#[test]
fn an_unknown_bound_name_is_not_a_parse_error() {
    // The sealed set is enforced by the checker, which can name the property.
    // The parser accepting it is what makes that diagnostic possible.
    assert!(parse("fn f<T: Sortable>(x: T) {}").diagnostics.is_empty());
}

#[test]
fn nested_generic_arguments_parse() {
    let module = expect_clean("fn f(m: Map<String, List<Int>>) {}");
    let ItemKind::Fn(f) = &module.items[0].kind else {
        panic!("expected a function");
    };
    let TypeKind::Named { args, .. } = &f.params[0].ty.kind else {
        panic!("expected a named type");
    };
    assert_eq!(args.len(), 2);
    let TypeKind::Named { args: inner, .. } = &args[1].kind else {
        panic!("expected List<Int>");
    };
    assert_eq!(inner.len(), 1);
}

// ------------------------------------------------- struct literal ambiguity

#[test]
fn a_condition_followed_by_a_block_is_not_a_struct_literal() {
    // Without suppression, `ready { .. }` parses as a struct literal and the
    // error lands nowhere near the actual mistake.
    for source in [
        "fn f(ready: Bool) { if ready { } }",
        "fn f(ready: Bool) { while ready { } }",
        "fn f(v: Int) { match v { _ => unit, } }",
    ] {
        assert!(
            parse(source).diagnostics.is_empty(),
            "failed to parse: {source}"
        );
    }
}

#[test]
fn struct_literals_still_parse_where_they_are_unambiguous() {
    let ExprKind::StructLit { fields, .. } = expr_of("Player { name: \"ada\", score: 0 }").kind
    else {
        panic!("expected a struct literal");
    };
    assert_eq!(fields.len(), 2);
}

#[test]
fn a_struct_literal_inside_a_conditions_parentheses_is_allowed() {
    // Suppression applies to the condition itself, not to bracketed
    // subexpressions within it.
    assert!(
        parse("fn f() { if is_ready(p: Player { score: 0 }) { } }")
            .diagnostics
            .is_empty()
    );
}

// ---------------------------------------------------------------- statements

#[test]
fn let_and_var_differ_only_in_mutability() {
    let module = expect_clean("fn f() { let a = 1; var b = 2; }");
    let ItemKind::Fn(f) = &module.items[0].kind else {
        panic!("expected a function");
    };
    let stmts = &f.body.as_ref().expect("body").stmts;
    let StmtKind::Let { mutable: a, .. } = &stmts[0].kind else {
        panic!("expected a let");
    };
    let StmtKind::Let { mutable: b, .. } = &stmts[1].kind else {
        panic!("expected a var");
    };
    assert!(!a);
    assert!(b);
}

#[test]
fn a_trailing_expression_becomes_the_blocks_value() {
    let module = expect_clean("fn f() -> Int { let a = 1; a + 1 }");
    let ItemKind::Fn(f) = &module.items[0].kind else {
        panic!("expected a function");
    };
    let body = f.body.as_ref().expect("body");
    assert_eq!(body.stmts.len(), 1);
    assert!(body.tail.is_some(), "the final expression is the value");
}

#[test]
fn a_semicolon_after_the_final_expression_discards_it() {
    let module = expect_clean("fn f() { let a = 1; a + 1; }");
    let ItemKind::Fn(f) = &module.items[0].kind else {
        panic!("expected a function");
    };
    let body = f.body.as_ref().expect("body");
    assert_eq!(body.stmts.len(), 2);
    assert!(body.tail.is_none(), "the block evaluates to unit");
}

#[test]
fn block_shaped_expressions_need_no_semicolon_as_statements() {
    assert!(
        parse("fn f(c: Bool) { if c { } if c { } }")
            .diagnostics
            .is_empty()
    );
}

#[test]
fn loops_and_jumps_parse() {
    assert!(
        parse("fn f(xs: List<Int>) { for x in xs { continue; } while true { break; } }")
            .diagnostics
            .is_empty()
    );
}

// ------------------------------------------------------------------ patterns

#[test]
fn match_arms_support_guards_variants_and_alternatives() {
    let source = r#"
fn f(r: Result<Int, Error>) -> Int {
    match r {
        Ok(v) if v > 0 => v,
        Ok(_) => 0,
        Err(Error.NotFound) | Err(Error.Denied) => -1,
        _ => -2,
    }
}
"#;
    let module = expect_clean(source);
    let ItemKind::Fn(f) = &module.items[0].kind else {
        panic!("expected a function");
    };
    let ExprKind::Match { arms, .. } = &f.body.as_ref().expect("body").tail.as_ref().unwrap().kind
    else {
        panic!("expected a match");
    };
    assert_eq!(arms.len(), 4);
    assert!(arms[0].guard.is_some());
    assert!(matches!(arms[2].pattern.kind, PatternKind::Or(_)));
    assert!(matches!(arms[3].pattern.kind, PatternKind::Wildcard));
}

/// Arms of a `match` written as the sole expression of a function body.
fn match_arms_of(source: &str) -> Vec<MatchArm> {
    let ExprKind::Match { arms, .. } = expr_of(source).kind else {
        panic!("expected a match expression");
    };
    arms
}

#[test]
fn a_single_segment_pattern_binds_but_a_dotted_one_names_a_variant() {
    let arms = match_arms_of("match r { total => unit, }");
    assert!(matches!(arms[0].pattern.kind, PatternKind::Binding(_)));

    let arms = match_arms_of("match r { Rank.Gold => unit, }");
    assert!(matches!(arms[0].pattern.kind, PatternKind::Path(_)));
}

#[test]
fn struct_patterns_support_shorthand_fields() {
    let arms = match_arms_of("match p { Player { name, score: s } => unit, }");
    let PatternKind::Struct { fields, .. } = &arms[0].pattern.kind else {
        panic!("expected a struct pattern");
    };
    assert!(fields[0].pattern.is_none(), "shorthand binds to itself");
    assert!(fields[1].pattern.is_some());
}

// ------------------------------------------------------------------ lambdas

#[test]
fn lambdas_parse_with_and_without_parameters() {
    assert!(matches!(
        expr_of("move || 1").kind,
        ExprKind::Lambda {
            is_move: true,
            is_async: false,
            ..
        }
    ));
    assert!(matches!(
        expr_of("async move |x: Int| x").kind,
        ExprKind::Lambda {
            is_move: true,
            is_async: true,
            ..
        }
    ));

    let ExprKind::Lambda { params, .. } = expr_of("|a: Int, b: Int| a").kind else {
        panic!("expected a lambda");
    };
    assert_eq!(params.len(), 2);
}

// ------------------------------------------------------------------ recovery

#[test]
fn a_missing_semicolon_is_reported_with_an_insertion_fix() {
    let parsed = parse("fn f() { let a = 1 let b = 2; }");
    let diagnostic = &parsed.diagnostics[0];
    assert_eq!(diagnostic.code, DiagCode::MissingSemicolon);
    let fix = diagnostic.fix.as_ref().expect("fix expected");
    assert_eq!(fix.edits[0].replacement, ";");
    // Inserted where the punctuation belongs, not at the token that exposed it.
    assert_eq!(fix.edits[0].span.start, 18);
}

#[test]
fn a_caret_for_omitted_punctuation_points_where_the_fix_inserts() {
    // A caret and a fix that disagree send the reader -- or a model applying
    // the fix -- to two different places.
    for source in [
        "fn f() { let a = 1 let b = 2; }",
        "fn f() { g(a: 1 b: 2); }",
        "use std.io",
    ] {
        for diagnostic in &parse(source).diagnostics {
            let Some(fix) = &diagnostic.fix else { continue };
            assert_eq!(
                diagnostic.span.start, fix.edits[0].span.start,
                "caret and fix disagree for {source:?}"
            );
        }
    }
}

#[test]
fn parsing_continues_after_a_broken_statement() {
    let parsed = parse("fn f() { let a = ; let b = 2; }\nfn g() -> Int { 1 }");
    assert!(!parsed.diagnostics.is_empty());
    assert_eq!(parsed.module.items.len(), 2, "the second function survives");
    let ItemKind::Fn(g) = &parsed.module.items[1].kind else {
        panic!("expected the second function to parse");
    };
    assert_eq!(g.name.name, "g");
}

#[test]
fn a_top_level_statement_is_reported_as_a_missing_declaration() {
    assert_eq!(codes("let x = 1;")[0], "XN1007");
}

#[test]
fn a_bare_return_reports_and_suggests_unit() {
    let parsed = parse("fn f() { return; }");
    assert_eq!(parsed.diagnostics[0].code, DiagCode::ExpectedExpression);
    let fix = parsed.diagnostics[0].fix.as_ref().expect("fix expected");
    assert_eq!(fix.edits[0].replacement, " unit");
}

#[test]
fn every_input_produces_a_module() {
    // Totality: a model mid-edit produces exactly this kind of text, and a
    // parser that gives up returns nothing to repair from.
    let cases = [
        "",
        "fn",
        "fn f(",
        "fn f() {",
        "fn f() { let",
        "fn f() { match x {",
        "}}}}",
        "struct",
        "enum E {",
        "use",
        "fn f() { a.. }",
        "?? ?? ??",
        "fn f() -> { }",
    ];
    for source in cases {
        let parsed = parse(source);
        // The assertion is simply that this returns rather than hanging or
        // panicking; the module may be full of Error nodes.
        let _ = parsed.module.items.len();
    }
}

#[test]
fn an_unterminated_function_body_still_yields_the_function() {
    let parsed = parse("fn f() { let a = 1;");
    assert_eq!(parsed.module.items.len(), 1);
    let ItemKind::Fn(f) = &parsed.module.items[0].kind else {
        panic!("expected a function");
    };
    assert_eq!(f.name.name, "f");
    assert!(!parsed.diagnostics.is_empty());
}
