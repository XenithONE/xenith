use xenith_syntax::{FormatError, format};

/// Inputs used wherever a test needs breadth rather than a specific shape.
const SAMPLES: &[&str] = &[
    "fn f() {}",
    "fn add(a: Int, b: Int) -> Int { a + b }",
    "use std.io;",
    "const LIMIT: Int = 1_000;",
    "struct P { name: String, var score: Int }",
    "struct Empty {}",
    "enum E { A, B(Int), C(Int, String) }",
    "fn f(c: Bool) -> Int { if c { 1 } else { 2 } }",
    "fn f(c: Bool) -> Int { if c { 1 } else if c { 2 } else { 3 } }",
    "fn f(v: Int) -> Int { match v { 0 => 1, _ => 2, } }",
    "fn f(r: Result<Int, E>) -> Int { match r { Ok(v) if v > 0 => v, Ok(_) => 0, Err(_) => -1, } }",
    "fn f(xs: List<Int>) { for x in xs { continue; } }",
    "fn f() { while true { break; } }",
    "fn f() -> Int { let a = 1; var b = 2; b = a + b; b }",
    "async fn f(net: Net) -> Result<Unit, E> uses {Net.get, Net.send} { return unit; }",
    "fn f() { g(a: 1, b: 2); }",
    "fn f() -> Int { ?? }",
    "fn f(x: ??) -> ??ret { ??body }",
    "fn f(m: Map<String, List<Int>>) {}",
    "fn f(p: Player) { match p { Player { name, score: s } => unit, } }",
    "fn f() { let g = move || 1; }",
    "fn f() { let g = async move |x: Int| x; }",
    "fn f() -> Int { a.b().c?.d.await }",
    "/// Doc\nfn f() {}",
    "fn f() { // a comment\n let a = 1; }",
    "fn f() -> Int { (a + b) * c }",
    "fn f() -> Int { a + b * c }",
    "fn f() -> Bool { a & b == c }",
    "fn f() { let p = Player { name: \"ada\", score: 0 }; }",
    "fn get<K: Eq + Hash, V>(map: Map<K, V>, key: K) -> Option<V> { ?? }",
    "struct Cache<K: Hash, V> { key: K, value: V }",
    "fn f() -> List<Int> { [1, 2, 3] }",
    "fn f() -> List<Int> { [] }",
    "fn f() -> List<List<Int>> { [[1], [], [2, 3]] }",
    "fn f() -> Int { var xs = [1]; xs.push(item: 2); xs.len() }",
];

fn formatted(source: &str) -> String {
    format(source).unwrap_or_else(|e| panic!("failed to format {source:?}: {e}"))
}

// ---------------------------------------------------------------- guarantees

#[test]
fn formatting_is_idempotent() {
    // A formatter that is not a fixed point produces a diff on every save and
    // makes CI unstable.
    for source in SAMPLES {
        let once = formatted(source);
        let twice = formatted(&once);
        assert_eq!(once, twice, "not idempotent for {source:?}");
    }
}

#[test]
fn formatting_never_changes_meaning() {
    // `format` proves this internally by re-parsing its own output and
    // comparing trees with spans cleared. Reaching here at all means the check
    // passed for every sample; this test exists to make that visible.
    for source in SAMPLES {
        assert!(format(source).is_ok(), "self-check rejected {source:?}");
    }
}

#[test]
fn layout_differences_collapse_to_the_same_bytes() {
    // The central guarantee: same meaning, same bytes.
    let dense = "fn f(a:Int,b:Int)->Int{let c=a+b;c}";
    let sprawling =
        "fn   f( a : Int ,\n  b : Int )\n->\nInt\n{\n\n    let   c =  a  +  b ;\n\n\n    c\n}\n";
    assert_eq!(formatted(dense), formatted(sprawling));
}

#[test]
fn every_output_ends_with_exactly_one_newline() {
    for source in SAMPLES {
        let output = formatted(source);
        assert!(output.ends_with('\n'), "{source:?}");
        assert!(!output.ends_with("\n\n"), "{source:?}");
    }
}

#[test]
fn output_never_contains_tabs_or_carriage_returns() {
    for source in SAMPLES {
        let output = formatted(source);
        assert!(!output.contains('\t'), "{source:?}");
        assert!(!output.contains('\r'), "{source:?}");
    }
}

#[test]
fn crlf_input_produces_lf_output() {
    let output = formatted("fn f() {\r\n    let a = 1;\r\n}\r\n");
    assert!(!output.contains('\r'));
}

// ------------------------------------------------------------------ comments

#[test]
fn comments_survive_formatting() {
    let source = "fn f() {\n// first\nlet a = 1;\n// second\nlet b = 2;\n}";
    let output = formatted(source);
    assert!(output.contains("// first"), "{output}");
    assert!(output.contains("// second"), "{output}");
}

#[test]
fn documentation_survives_formatting() {
    let output = formatted("/// Describes f.\nfn f() {}");
    assert!(output.contains("/// Describes f."), "{output}");
}

#[test]
fn a_comment_after_the_last_declaration_is_kept() {
    let output = formatted("fn f() {}\n// trailing note\n");
    assert!(output.contains("// trailing note"), "{output}");
}

#[test]
fn a_file_header_is_separated_from_the_first_declaration() {
    // Without this the header runs straight into the declaration and reads as
    // a comment about it. The rule keys off position in the file, not the
    // input's blank lines, so it stays layout-independent.
    let output = formatted("// What this file is for.\n// Second header line.\nuse std.io;");
    assert_eq!(
        output,
        "// What this file is for.\n// Second header line.\n\nuse std.io;\n"
    );
}

#[test]
fn a_file_header_is_separated_from_the_first_documentation_comment() {
    // The case that made this rule necessary: a header immediately above a
    // documented declaration produced one confused block of comment lines.
    let output = formatted("// Header.\n/// Documents f.\nfn f() {}");
    assert_eq!(output, "// Header.\n\n/// Documents f.\nfn f() {}\n");
}

#[test]
fn documentation_alone_is_not_treated_as_a_header() {
    assert_eq!(
        formatted("/// Documents f.\nfn f() {}"),
        "/// Documents f.\nfn f() {}\n"
    );
}

#[test]
fn a_comment_before_a_later_declaration_attaches_to_it() {
    let output = formatted("fn a() {}\n// about b\nfn b() {}");
    assert_eq!(output, "fn a() {}\n\n// about b\nfn b() {}\n");
}

#[test]
fn comments_inside_a_struct_are_kept() {
    let output = formatted("struct P {\n// the player's name\nname: String,\n}");
    assert!(output.contains("// the player's name"), "{output}");
}

// -------------------------------------------------------------- blank lines

#[test]
fn blank_lines_inside_a_block_are_removed() {
    // Deliberately stricter than gofmt or rustfmt. Blank lines carry no
    // meaning, so leaving them in would break "same meaning, same bytes" --
    // a model that writes one and a model that does not would disagree.
    // Recorded, with the tradeoff, in design/0005.
    let output = formatted("fn f() -> Int {\n    let a = 1;\n\n\n    let b = 2;\n\n    a + b\n}");
    assert_eq!(
        output,
        "fn f() -> Int {\n    let a = 1;\n    let b = 2;\n    a + b\n}\n"
    );
}

#[test]
fn declarations_are_separated_by_exactly_one_blank_line() {
    let output = formatted("fn a() {}\nfn b() {}\n\n\n\nfn c() {}");
    assert_eq!(output, "fn a() {}\n\nfn b() {}\n\nfn c() {}\n");
}

// ------------------------------------------------------------- parentheses

#[test]
fn necessary_parentheses_are_preserved() {
    assert!(formatted("fn f() -> Int { (a + b) * c }").contains("(a + b) * c"));
    assert!(formatted("fn f() -> Int { a * (b + c) }").contains("a * (b + c)"));
    assert!(formatted("fn f() -> Int { (a - b) - c }").contains("a - b - c"));
    assert!(formatted("fn f() -> Int { a - (b - c) }").contains("a - (b - c)"));
}

#[test]
fn redundant_parentheses_are_removed() {
    assert!(formatted("fn f() -> Int { (a) }").contains("    a\n"));
    assert!(formatted("fn f() -> Int { a + (b * c) }").contains("a + b * c"));
    assert!(formatted("fn f() -> Bool { (a & b) == c }").contains("a & b == c"));
}

#[test]
fn unary_and_postfix_parenthesise_correctly() {
    assert!(formatted("fn f() -> Int { -(a + b) }").contains("-(a + b)"));
    assert!(formatted("fn f() -> Int { (-a) + b }").contains("-a + b"));
    assert!(formatted("fn f() -> Int { (a + b).c }").contains("(a + b).c"));
    assert!(formatted("fn f() -> Int { (a?).b }").contains("a?.b"));
}

// ------------------------------------------------------------------- shape

#[test]
fn a_function_is_laid_out_canonically() {
    let output = formatted("fn add(a:Int,b:Int)->Int{a+b}");
    assert_eq!(output, "fn add(a: Int, b: Int) -> Int {\n    a + b\n}\n");
}

#[test]
fn indentation_is_four_spaces_per_level() {
    let output = formatted("fn f(c: Bool) { if c { if c { g(); } } }");
    assert!(output.contains("\n        if c {"), "{output}");
    assert!(output.contains("\n            g();"), "{output}");
}

#[test]
fn generic_bounds_format_canonically() {
    let output = formatted("fn get<K:Eq+Hash,V>(map:Map<K,V>,key:K)->Option<V>{ ?? }");
    assert!(
        output.starts_with("fn get<K: Eq + Hash, V>(map: Map<K, V>, key: K) -> Option<V> {"),
        "{output}"
    );
}

#[test]
fn effects_are_rendered_after_the_return_type() {
    let output = formatted("fn f(fs:Fs)->Result<Unit,E>uses{Fs.read,Fs.write}{return unit;}");
    assert!(
        output.starts_with("fn f(fs: Fs) -> Result<Unit, E> uses {Fs.read, Fs.write} {"),
        "{output}"
    );
}

#[test]
fn struct_fields_are_one_per_line_with_trailing_commas() {
    let output = formatted("struct P{name:String,var score:Int}");
    assert_eq!(
        output,
        "struct P {\n    name: String,\n    var score: Int,\n}\n"
    );
}

#[test]
fn match_arms_are_one_per_line() {
    let output = formatted("fn f(v:Int)->Int{match v{0=>1,_=>2,}}");
    assert_eq!(
        output,
        "fn f(v: Int) -> Int {\n    match v {\n        0 => 1,\n        _ => 2,\n    }\n}\n"
    );
}

#[test]
fn else_if_chains_stay_on_the_closing_brace() {
    let output = formatted("fn f(c: Bool) -> Int { if c { 1 } else if c { 2 } else { 3 } }");
    assert!(output.contains("} else if c {"), "{output}");
    assert!(output.contains("} else {"), "{output}");
}

#[test]
fn list_literals_format_canonically() {
    // A trailing comma is accepted on input and dropped on output.
    let output = formatted("fn f()->List<Int>{[1,2,3,]}");
    assert_eq!(output, "fn f() -> List<Int> {\n    [1, 2, 3]\n}\n");
    let output = formatted("fn f()->List<Int>{[ ]}");
    assert_eq!(output, "fn f() -> List<Int> {\n    []\n}\n");
}

#[test]
fn an_over_long_list_breaks_one_element_per_line() {
    let long = "fn f() { let xs = [alpha_element_with_a_long_name, beta_element_with_a_long_name, gamma_element_with_a_long_name]; }";
    let output = formatted(long);
    assert!(
        output.contains("        alpha_element_with_a_long_name,\n"),
        "{output}"
    );
    assert!(output.contains("    ];\n"), "{output}");
    assert!(
        output.lines().all(|l| l.chars().count() <= 100),
        "a line still exceeds the width:\n{output}"
    );
}

#[test]
fn an_over_long_call_breaks_one_argument_per_line() {
    let long = "fn f() { some_function_with_a_long_name(first_argument_name: first_value_here, second_argument_name: second_value_here); }";
    let output = formatted(long);
    assert!(
        output.contains("first_argument_name: first_value_here,\n"),
        "{output}"
    );
    assert!(
        output.lines().all(|l| l.chars().count() <= 100),
        "a line still exceeds the width:\n{output}"
    );
}

// ------------------------------------------------------------------- refusal

#[test]
fn source_that_does_not_parse_is_not_formatted() {
    // Formatting something the compiler cannot read would be guessing.
    let result = format("fn f( {");
    assert!(matches!(result, Err(FormatError::Unparsable(_))));
}

#[test]
fn the_error_explains_itself() {
    let Err(error) = format("fn f( {") else {
        panic!("expected a refusal");
    };
    assert!(error.to_string().contains("parse error"), "{error}");
}

#[test]
fn an_empty_file_formats_to_nothing() {
    assert_eq!(formatted(""), "");
    assert_eq!(formatted("\n\n\n"), "");
}
