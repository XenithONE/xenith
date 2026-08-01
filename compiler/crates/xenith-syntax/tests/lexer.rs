use xenith_diag::DiagCode;
use xenith_syntax::{TokenKind, lex};

/// Kinds of the tokens the parser would see, without the trailing `Eof`.
fn kinds(source: &str) -> Vec<TokenKind> {
    let lexed = lex(source);
    lexed
        .significant()
        .map(|t| t.kind)
        .filter(|k| *k != TokenKind::Eof)
        .collect()
}

fn codes(source: &str) -> Vec<&'static str> {
    lex(source)
        .diagnostics
        .iter()
        .map(|d| d.code.id())
        .collect()
}

fn texts(source: &str) -> Vec<&str> {
    let lexed = lex(source);
    lexed
        .tokens
        .iter()
        .filter(|t| !t.kind.is_trivia() && t.kind != TokenKind::Eof)
        .map(|t| t.span.slice(source).unwrap_or("<bad span>"))
        .collect::<Vec<_>>()
}

// ---------------------------------------------------------------- invariants

#[test]
fn tokens_tile_the_source_exactly() {
    // The formatter reconstructs output from tokens, so the token stream must
    // cover every byte with no gaps and no overlaps.
    let source = r#"
/// Doc
fn main(io: Io) -> Result<Unit, Error> uses {Io.write} {
    let greeting = "hi\n";   // trailing comment
    io.write(text: greeting)?;
    return unit;
}
"#;
    let lexed = lex(source);
    let mut cursor = 0u32;
    for token in &lexed.tokens {
        assert_eq!(
            token.span.start, cursor,
            "gap or overlap before {:?} at {:?}",
            token.kind, token.span
        );
        cursor = token.span.end;
    }
    assert_eq!(cursor as usize, source.len(), "tokens must reach the end");
}

#[test]
fn every_input_ends_with_eof() {
    for source in ["", "   ", "fn", "\"unterminated", "@@@", "??", "'"] {
        let lexed = lex(source);
        assert_eq!(
            lexed.tokens.last().map(|t| t.kind),
            Some(TokenKind::Eof),
            "input {source:?} did not end with Eof"
        );
    }
}

#[test]
fn pathological_input_does_not_panic() {
    // Lexing must be total. A model mid-edit produces exactly this kind of text.
    let cases = [
        "\\",
        "'''",
        "\"\\",
        "1.",
        ".",
        "?",
        "???????",
        "あいうえお",
        "\u{0}\u{1}\u{2}",
        "0000000000000000000000000000000",
        "'\\",
        "\"\\\"",
    ];
    for source in cases {
        let lexed = lex(source);
        assert_eq!(lexed.tokens.last().map(|t| t.kind), Some(TokenKind::Eof));
    }
}

#[test]
fn empty_source_yields_only_eof() {
    let lexed = lex("");
    assert_eq!(lexed.tokens.len(), 1);
    assert_eq!(lexed.tokens[0].kind, TokenKind::Eof);
    assert!(lexed.diagnostics.is_empty());
}

// ------------------------------------------------------------------- basics

#[test]
fn a_small_function_lexes_without_diagnostics() {
    let source = "fn add(a: Int, b: Int) -> Int { a + b }";
    assert!(lex(source).diagnostics.is_empty());
    assert_eq!(
        kinds(source),
        [
            TokenKind::Fn,
            TokenKind::Ident,
            TokenKind::LParen,
            TokenKind::Ident,
            TokenKind::Colon,
            TokenKind::Ident,
            TokenKind::Comma,
            TokenKind::Ident,
            TokenKind::Colon,
            TokenKind::Ident,
            TokenKind::RParen,
            TokenKind::Arrow,
            TokenKind::Ident,
            TokenKind::LBrace,
            TokenKind::Ident,
            TokenKind::Plus,
            TokenKind::Ident,
            TokenKind::RBrace,
        ]
    );
}

#[test]
fn keywords_are_distinguished_from_identifiers_that_contain_them() {
    assert_eq!(kinds("use"), [TokenKind::Use]);
    assert_eq!(kinds("uses"), [TokenKind::Uses]);
    assert_eq!(kinds("user"), [TokenKind::Ident]);
    assert_eq!(kinds("if_valid"), [TokenKind::Ident]);
    assert_eq!(kinds("informal"), [TokenKind::Ident]);
}

#[test]
fn underscore_alone_is_a_wildcard_but_prefixed_names_are_identifiers() {
    assert_eq!(kinds("_"), [TokenKind::Underscore]);
    assert_eq!(kinds("_unused"), [TokenKind::Ident]);
    assert_eq!(kinds("__"), [TokenKind::Ident]);
}

#[test]
fn multi_character_operators_win_over_their_prefixes() {
    assert_eq!(
        kinds("== != <= >= -> => += -= *= /= %= && ||"),
        [
            TokenKind::EqEq,
            TokenKind::NotEq,
            TokenKind::LtEq,
            TokenKind::GtEq,
            TokenKind::Arrow,
            TokenKind::FatArrow,
            TokenKind::PlusAssign,
            TokenKind::MinusAssign,
            TokenKind::StarAssign,
            TokenKind::SlashAssign,
            TokenKind::PercentAssign,
            TokenKind::AndAnd,
            TokenKind::OrOr,
        ]
    );
}

// ------------------------------------------------------------------ generics

#[test]
fn nested_generics_do_not_collapse_into_a_shift_operator() {
    // The reason the lexer never produces `>>`. If it did, this closing pair
    // would be one token and every nested generic would fail to parse.
    let source = "Map<String, List<Int>>";
    let got = kinds(source);
    assert_eq!(got[got.len() - 2], TokenKind::Gt);
    assert_eq!(got[got.len() - 1], TokenKind::Gt);
    assert!(!got.contains(&TokenKind::Shr), "lexer must not emit `>>`");
}

#[test]
fn adjacent_angle_brackets_stay_adjacent_so_the_parser_can_join_them() {
    let lexed = lex("a >> b");
    let significant: Vec<_> = lexed.significant().collect();
    // `>` `>` with no gap: the parser can recognise a shift by span adjacency.
    assert_eq!(significant[1].kind, TokenKind::Gt);
    assert_eq!(significant[2].kind, TokenKind::Gt);
    assert_eq!(significant[1].span.end, significant[2].span.start);
}

// --------------------------------------------------------------------- holes

#[test]
fn an_anonymous_hole_is_a_single_token() {
    assert_eq!(kinds("??"), [TokenKind::Hole]);
}

#[test]
fn a_named_hole_includes_its_name() {
    assert_eq!(kinds("??response"), [TokenKind::Hole]);
    assert_eq!(texts("??response"), ["??response"]);
}

#[test]
fn holes_are_not_diagnostics() {
    // Partial programs are legal. A hole must not itself be an error.
    let source = "let x: Int = ??value;";
    assert!(
        lex(source).diagnostics.is_empty(),
        "a hole is a legal program element"
    );
}

#[test]
fn a_single_question_mark_is_still_error_propagation() {
    assert_eq!(
        kinds("try_read()?"),
        [
            TokenKind::Ident,
            TokenKind::LParen,
            TokenKind::RParen,
            TokenKind::Question,
        ]
    );
}

// ------------------------------------------------------------------- numbers

#[test]
fn integers_allow_underscore_separators() {
    assert_eq!(kinds("1_000_000"), [TokenKind::Int]);
    assert_eq!(texts("1_000_000"), ["1_000_000"]);
    assert!(lex("1_000_000").diagnostics.is_empty());
}

#[test]
fn a_float_needs_digits_on_both_sides() {
    assert_eq!(kinds("1.5"), [TokenKind::Float]);
    assert!(lex("1.5").diagnostics.is_empty());

    assert_eq!(codes(".5"), ["XN0004"]);
    assert_eq!(codes("1."), ["XN0004"]);
}

#[test]
fn malformed_floats_suggest_the_missing_digit() {
    let lexed = lex(".5");
    let fix = lexed.diagnostics[0].fix.as_ref().expect("fix expected");
    assert_eq!(fix.edits[0].replacement, "0");
    assert_eq!(fix.edits[0].span.start, 0, "inserts before the point");

    let lexed = lex("1.");
    let fix = lexed.diagnostics[0].fix.as_ref().expect("fix expected");
    assert_eq!(fix.edits[0].replacement, "0");
    assert_eq!(fix.edits[0].span.start, 2, "inserts after the point");
}

#[test]
fn a_point_followed_by_a_name_is_a_method_call_not_a_broken_float() {
    let source = "1.to_text()";
    assert!(
        lex(source).diagnostics.is_empty(),
        "`1.to_text()` is a method call on an integer"
    );
    assert_eq!(
        kinds(source),
        [
            TokenKind::Int,
            TokenKind::Dot,
            TokenKind::Ident,
            TokenKind::LParen,
            TokenKind::RParen,
        ]
    );
}

// ------------------------------------------------------------------- strings

#[test]
fn strings_carry_their_quotes_in_the_span() {
    assert_eq!(kinds(r#""hello""#), [TokenKind::Str]);
    assert_eq!(texts(r#""hello""#), [r#""hello""#]);
}

#[test]
fn recognised_escapes_are_accepted() {
    let source = r#""a\nb\tc\\d\"e\'f\0g""#;
    assert!(lex(source).diagnostics.is_empty());
}

#[test]
fn an_unrecognised_escape_is_reported_once() {
    assert_eq!(codes(r#""a\qb""#), ["XN0003"]);
}

#[test]
fn an_unterminated_string_stops_at_the_newline_and_offers_a_closing_quote() {
    let source = "\"oops\nlet x = 1;";
    let lexed = lex(source);
    assert_eq!(lexed.diagnostics[0].code, DiagCode::UnterminatedString);
    let fix = lexed.diagnostics[0].fix.as_ref().expect("fix expected");
    assert_eq!(fix.edits[0].replacement, "\"");

    // Recovery matters: the rest of the line must still lex.
    assert!(
        kinds(source).contains(&TokenKind::Let),
        "lexing continues after an unterminated string"
    );
}

// ---------------------------------------------------------------- characters

#[test]
fn character_literals_hold_exactly_one_character() {
    assert!(lex("'a'").diagnostics.is_empty());
    assert!(lex(r"'\n'").diagnostics.is_empty());
    assert!(
        lex("'あ'").diagnostics.is_empty(),
        "a multi-byte scalar is still one character"
    );

    assert_eq!(codes("''"), ["XN0005"]);
    assert_eq!(codes("'ab'"), ["XN0005"]);
}

#[test]
fn a_multi_character_literal_suggests_double_quotes() {
    let lexed = lex("'hello'");
    let fix = lexed.diagnostics[0].fix.as_ref().expect("fix expected");
    assert_eq!(fix.edits[0].replacement, "\"hello\"");
}

// ------------------------------------------------------------------ comments

#[test]
fn comments_are_retained_as_trivia() {
    let lexed = lex("let x = 1; // note\n");
    assert!(
        lexed
            .tokens
            .iter()
            .any(|t| t.kind == TokenKind::LineComment),
        "the formatter needs comments to survive lexing"
    );
    assert!(
        !lexed
            .significant()
            .any(|t| t.kind == TokenKind::LineComment),
        "the parser should not see comments"
    );
}

#[test]
fn three_slashes_document_but_four_do_not() {
    let doc = lex("/// documentation\n");
    assert!(doc.tokens.iter().any(|t| t.kind == TokenKind::DocComment));

    // A decorative rule should not attach itself to the next declaration.
    let rule = lex("//////////\n");
    assert!(rule.tokens.iter().any(|t| t.kind == TokenKind::LineComment));
    assert!(!rule.tokens.iter().any(|t| t.kind == TokenKind::DocComment));
}

#[test]
fn a_comment_runs_to_the_end_of_the_line_only() {
    assert_eq!(
        kinds("// gone\nlet x = 1;"),
        [
            TokenKind::Let,
            TokenKind::Ident,
            TokenKind::Assign,
            TokenKind::Int,
            TokenKind::Semi,
        ]
    );
}

// ------------------------------------------------------------ reserved words

#[test]
fn a_reserved_word_reports_once_and_recovers_as_an_identifier() {
    let lexed = lex("let loop = 1;");
    assert_eq!(
        lexed.diagnostics.iter().map(|d| d.code).collect::<Vec<_>>(),
        [DiagCode::ReservedKeyword],
        "exactly one diagnostic, not a cascade"
    );
    assert!(
        kinds("let loop = 1;").contains(&TokenKind::Ident),
        "recovers as an identifier so parsing continues"
    );
}

// -------------------------------------------------------- confusable characters

#[test]
fn full_width_quotes_are_reported_with_the_ascii_replacement() {
    // Pasting from a document or chat window is how this arrives, and the
    // characters are indistinguishable in most terminals.
    let lexed = lex("let s = “hi”;");
    assert_eq!(lexed.diagnostics[0].code, DiagCode::UnexpectedCharacter);
    let fix = lexed.diagnostics[0].fix.as_ref().expect("fix expected");
    assert_eq!(fix.edits[0].replacement, "\"");
}

#[test]
fn unicode_space_characters_are_accepted_as_whitespace() {
    // An ideographic space and a non-breaking space are genuinely whitespace,
    // and whitespace carries no meaning in Xenith (design/0003) — so pasting
    // one is harmless rather than an error. The canonical formatter rewrites
    // it to ASCII on the next pass.
    for space in ['\u{3000}', '\u{a0}'] {
        let source = format!("let{space}x = 1;");
        let lexed = lex(&source);
        assert!(
            lexed.diagnostics.is_empty(),
            "{space:?} should lex as whitespace, got {:?}",
            lexed.diagnostics
        );
        assert_eq!(
            kinds(&source),
            [
                TokenKind::Let,
                TokenKind::Ident,
                TokenKind::Assign,
                TokenKind::Int,
                TokenKind::Semi,
            ],
            "{space:?} must separate tokens like a plain space"
        );
    }
}

#[test]
fn an_unrecognised_character_without_a_lookalike_has_no_fix() {
    let lexed = lex("let x = §;");
    assert_eq!(lexed.diagnostics[0].code, DiagCode::UnexpectedCharacter);
    assert!(
        lexed.diagnostics[0].fix.is_none(),
        "a guess would teach models to apply fixes blindly"
    );
}

// ------------------------------------------------------------------- unicode

#[test]
fn spans_of_multibyte_content_slice_correctly() {
    let source = r#"let greeting = "こんにちは";"#;
    assert!(lex(source).diagnostics.is_empty());
    assert_eq!(texts(source).last(), Some(&";"));
    assert!(texts(source).contains(&r#""こんにちは""#));
}
