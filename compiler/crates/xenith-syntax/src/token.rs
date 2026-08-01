//! The Xenith token set.
//!
//! The operator set is closed — Xenith has no user-defined operators — so any
//! symbol not listed here is an error rather than something that might be
//! defined elsewhere. That is what makes [`DiagCode::UnexpectedCharacter`]
//! actionable.
//!
//! [`DiagCode::UnexpectedCharacter`]: xenith_diag::DiagCode::UnexpectedCharacter

use xenith_diag::Span;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TokenKind {
    // ----- literals -----
    /// Decimal integer, `_` permitted as a separator: `1_000`.
    Int,
    /// Decimal float, digits required on both sides: `1.0`.
    Float,
    Str,
    Char,

    // ----- names -----
    Ident,

    // ----- keywords -----
    Fn,
    Let,
    Var,
    Const,
    Struct,
    Enum,
    Match,
    If,
    Else,
    While,
    For,
    In,
    Return,
    Break,
    Continue,
    Use,
    /// `uses` — introduces a function's closed effect set. Distinct from `use`.
    Uses,
    Async,
    Await,
    Move,
    /// `is` — identity comparison, valid only on `Shared` and handles.
    Is,
    As,
    True,
    False,
    /// `unit` — the sole value of the unit type. `return unit;` rather than a
    /// bare `return`, so that the Go-style "return with no operand silently
    /// yields a zero value" class cannot exist. See design/0003.
    Unit,
    SelfValue,
    Unsafe,

    // ----- typed holes -----
    /// `??` or `??name`. A hole is a legal program element, not an error:
    /// partial programs compile. See design/0002.
    Hole,

    // ----- operators -----
    Plus,
    Minus,
    Star,
    Slash,
    Percent,
    Bang,
    Assign,
    PlusAssign,
    MinusAssign,
    StarAssign,
    SlashAssign,
    PercentAssign,
    EqEq,
    NotEq,
    Lt,
    Gt,
    LtEq,
    GtEq,
    AndAnd,
    OrOr,
    Amp,
    Pipe,
    Caret,
    /// `<<` — **never produced by the lexer.** The parser synthesises it by
    /// joining two adjacent [`TokenKind::Lt`] tokens. Lexing `<<` and `>>`
    /// directly would make the trailing `>>` of `Map<String, List<Int>>` a
    /// single token and break every nested generic.
    Shl,
    /// `>>` — see [`TokenKind::Shl`].
    Shr,
    /// Postfix `?` — early return on `Result` or `Option`.
    Question,

    // ----- punctuation -----
    Dot,
    Comma,
    Colon,
    Semi,
    /// `->` return type
    Arrow,
    /// `=>` match arm
    FatArrow,
    Underscore,
    LParen,
    RParen,
    LBrace,
    RBrace,
    LBracket,
    RBracket,

    // ----- trivia -----
    //
    // Trivia is part of the token stream rather than discarded, because the
    // canonical formatter has to place comments and cannot recover them later.
    Whitespace,
    /// `// ...`
    LineComment,
    /// `/// ...`
    DocComment,

    // ----- ends -----
    /// A character that begins no token. Recovery continues after it.
    Unknown,
    Eof,
}

impl TokenKind {
    /// Whitespace and comments — skipped by the parser, retained by the formatter.
    pub fn is_trivia(self) -> bool {
        matches!(
            self,
            TokenKind::Whitespace | TokenKind::LineComment | TokenKind::DocComment
        )
    }

    pub fn is_keyword(self) -> bool {
        matches!(
            self,
            TokenKind::Fn
                | TokenKind::Let
                | TokenKind::Var
                | TokenKind::Const
                | TokenKind::Struct
                | TokenKind::Enum
                | TokenKind::Match
                | TokenKind::If
                | TokenKind::Else
                | TokenKind::While
                | TokenKind::For
                | TokenKind::In
                | TokenKind::Return
                | TokenKind::Break
                | TokenKind::Continue
                | TokenKind::Use
                | TokenKind::Uses
                | TokenKind::Async
                | TokenKind::Await
                | TokenKind::Move
                | TokenKind::Is
                | TokenKind::As
                | TokenKind::True
                | TokenKind::False
                | TokenKind::Unit
                | TokenKind::SelfValue
                | TokenKind::Unsafe
        )
    }

    pub fn is_literal(self) -> bool {
        matches!(
            self,
            TokenKind::Int
                | TokenKind::Float
                | TokenKind::Str
                | TokenKind::Char
                | TokenKind::True
                | TokenKind::False
                | TokenKind::Unit
        )
    }

    /// How this token is named in diagnostics. Punctuation is quoted so that
    /// "expected `;`" reads correctly.
    pub fn describe(self) -> &'static str {
        match self {
            TokenKind::Int => "integer literal",
            TokenKind::Float => "float literal",
            TokenKind::Str => "string literal",
            TokenKind::Char => "character literal",
            TokenKind::Ident => "identifier",
            TokenKind::Fn => "`fn`",
            TokenKind::Let => "`let`",
            TokenKind::Var => "`var`",
            TokenKind::Const => "`const`",
            TokenKind::Struct => "`struct`",
            TokenKind::Enum => "`enum`",
            TokenKind::Match => "`match`",
            TokenKind::If => "`if`",
            TokenKind::Else => "`else`",
            TokenKind::While => "`while`",
            TokenKind::For => "`for`",
            TokenKind::In => "`in`",
            TokenKind::Return => "`return`",
            TokenKind::Break => "`break`",
            TokenKind::Continue => "`continue`",
            TokenKind::Use => "`use`",
            TokenKind::Uses => "`uses`",
            TokenKind::Async => "`async`",
            TokenKind::Await => "`await`",
            TokenKind::Move => "`move`",
            TokenKind::Is => "`is`",
            TokenKind::As => "`as`",
            TokenKind::True => "`true`",
            TokenKind::False => "`false`",
            TokenKind::Unit => "`unit`",
            TokenKind::SelfValue => "`self`",
            TokenKind::Unsafe => "`unsafe`",
            TokenKind::Hole => "hole",
            TokenKind::Plus => "`+`",
            TokenKind::Minus => "`-`",
            TokenKind::Star => "`*`",
            TokenKind::Slash => "`/`",
            TokenKind::Percent => "`%`",
            TokenKind::Bang => "`!`",
            TokenKind::Assign => "`=`",
            TokenKind::PlusAssign => "`+=`",
            TokenKind::MinusAssign => "`-=`",
            TokenKind::StarAssign => "`*=`",
            TokenKind::SlashAssign => "`/=`",
            TokenKind::PercentAssign => "`%=`",
            TokenKind::EqEq => "`==`",
            TokenKind::NotEq => "`!=`",
            TokenKind::Lt => "`<`",
            TokenKind::Gt => "`>`",
            TokenKind::LtEq => "`<=`",
            TokenKind::GtEq => "`>=`",
            TokenKind::AndAnd => "`&&`",
            TokenKind::OrOr => "`||`",
            TokenKind::Amp => "`&`",
            TokenKind::Pipe => "`|`",
            TokenKind::Caret => "`^`",
            TokenKind::Shl => "`<<`",
            TokenKind::Shr => "`>>`",
            TokenKind::Question => "`?`",
            TokenKind::Dot => "`.`",
            TokenKind::Comma => "`,`",
            TokenKind::Colon => "`:`",
            TokenKind::Semi => "`;`",
            TokenKind::Arrow => "`->`",
            TokenKind::FatArrow => "`=>`",
            TokenKind::Underscore => "`_`",
            TokenKind::LParen => "`(`",
            TokenKind::RParen => "`)`",
            TokenKind::LBrace => "`{`",
            TokenKind::RBrace => "`}`",
            TokenKind::LBracket => "`[`",
            TokenKind::RBracket => "`]`",
            TokenKind::Whitespace => "whitespace",
            TokenKind::LineComment => "comment",
            TokenKind::DocComment => "doc comment",
            TokenKind::Unknown => "unrecognised character",
            TokenKind::Eof => "end of file",
        }
    }
}

/// Maps an identifier to its keyword kind, or `None` if it is an ordinary name.
pub fn keyword_kind(text: &str) -> Option<TokenKind> {
    let kind = match text {
        "fn" => TokenKind::Fn,
        "let" => TokenKind::Let,
        "var" => TokenKind::Var,
        "const" => TokenKind::Const,
        "struct" => TokenKind::Struct,
        "enum" => TokenKind::Enum,
        "match" => TokenKind::Match,
        "if" => TokenKind::If,
        "else" => TokenKind::Else,
        "while" => TokenKind::While,
        "for" => TokenKind::For,
        "in" => TokenKind::In,
        "return" => TokenKind::Return,
        "break" => TokenKind::Break,
        "continue" => TokenKind::Continue,
        "use" => TokenKind::Use,
        "uses" => TokenKind::Uses,
        "async" => TokenKind::Async,
        "await" => TokenKind::Await,
        "move" => TokenKind::Move,
        "is" => TokenKind::Is,
        "as" => TokenKind::As,
        "true" => TokenKind::True,
        "false" => TokenKind::False,
        "unit" => TokenKind::Unit,
        "self" => TokenKind::SelfValue,
        "unsafe" => TokenKind::Unsafe,
        _ => return None,
    };
    Some(kind)
}

/// Words held back for features Xenith intends to add.
///
/// Reserving them now means introducing the feature later cannot break code
/// that already exists. Using one is [`DiagCode::ReservedKeyword`].
///
/// [`DiagCode::ReservedKeyword`]: xenith_diag::DiagCode::ReservedKeyword
pub const RESERVED_WORDS: &[&str] = &[
    "trait",
    "impl",
    "where",
    "pub",
    "mod",
    "loop",
    "defer",
    "yield",
    "capability",
    "effect",
    "extern",
    "static",
    "macro",
];

pub fn is_reserved(text: &str) -> bool {
    RESERVED_WORDS.contains(&text)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Token {
    pub kind: TokenKind,
    pub span: Span,
}

impl Token {
    pub fn new(kind: TokenKind, span: Span) -> Token {
        Token { kind, span }
    }

    pub fn text<'a>(&self, source: &'a str) -> &'a str {
        self.span.slice(source).unwrap_or("")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keywords_and_reserved_words_do_not_overlap() {
        for word in RESERVED_WORDS {
            assert!(
                keyword_kind(word).is_none(),
                "`{word}` is both an active keyword and reserved"
            );
        }
    }

    #[test]
    fn uses_is_distinct_from_use() {
        assert_eq!(keyword_kind("use"), Some(TokenKind::Use));
        assert_eq!(keyword_kind("uses"), Some(TokenKind::Uses));
    }

    #[test]
    fn ordinary_names_are_not_keywords() {
        for name in ["user", "usest", "iffy", "format", "matcher", "inner"] {
            assert_eq!(keyword_kind(name), None, "`{name}` should be an identifier");
        }
    }

    #[test]
    fn trivia_classification_matches_the_formatter_contract() {
        assert!(TokenKind::Whitespace.is_trivia());
        assert!(TokenKind::LineComment.is_trivia());
        assert!(TokenKind::DocComment.is_trivia());
        assert!(!TokenKind::Ident.is_trivia());
        assert!(!TokenKind::Eof.is_trivia());
    }

    #[test]
    fn unit_true_and_false_count_as_literals() {
        assert!(TokenKind::Unit.is_literal());
        assert!(TokenKind::True.is_literal());
        assert!(TokenKind::False.is_literal());
        assert!(!TokenKind::Ident.is_literal());
    }
}
