//! The Xenith lexer.
//!
//! Two properties matter more than throughput here.
//!
//! **It never fails.** Every input produces a token stream ending in
//! [`TokenKind::Eof`], with problems reported as diagnostics alongside. Partial
//! and broken programs are a normal state in Xenith, not an exceptional one —
//! typed holes depend on the whole pipeline tolerating incomplete input, so the
//! tolerance starts at the first stage.
//!
//! **It keeps everything.** Whitespace and comments are tokens. The canonical
//! formatter has to place comments and cannot invent them later.

use crate::token::{Token, TokenKind, is_reserved, keyword_kind};
use xenith_diag::{DiagCode, Diagnostic, Edit, Fix, Span};

#[derive(Clone, Debug)]
pub struct Lexed {
    pub tokens: Vec<Token>,
    pub diagnostics: Vec<Diagnostic>,
}

impl Lexed {
    /// Tokens the parser sees: everything except whitespace and comments.
    pub fn significant(&self) -> impl Iterator<Item = &Token> {
        self.tokens.iter().filter(|t| !t.kind.is_trivia())
    }

    pub fn has_errors(&self) -> bool {
        self.diagnostics.iter().any(Diagnostic::is_error)
    }
}

pub fn lex(source: &str) -> Lexed {
    Lexer::new(source).run()
}

struct Lexer<'a> {
    source: &'a str,
    /// Byte offset of the next character to read.
    pos: usize,
    tokens: Vec<Token>,
    diagnostics: Vec<Diagnostic>,
}

impl<'a> Lexer<'a> {
    fn new(source: &'a str) -> Lexer<'a> {
        Lexer {
            source,
            pos: 0,
            tokens: Vec::new(),
            diagnostics: Vec::new(),
        }
    }

    // ----- cursor -----

    fn peek(&self) -> Option<char> {
        self.source[self.pos..].chars().next()
    }

    fn peek_at(&self, nth: usize) -> Option<char> {
        self.source[self.pos..].chars().nth(nth)
    }

    fn bump(&mut self) -> Option<char> {
        let ch = self.peek()?;
        self.pos += ch.len_utf8();
        Some(ch)
    }

    /// Consume one character if it equals `expected`.
    fn eat(&mut self, expected: char) -> bool {
        if self.peek() == Some(expected) {
            self.pos += expected.len_utf8();
            true
        } else {
            false
        }
    }

    fn eat_while(&mut self, mut predicate: impl FnMut(char) -> bool) {
        while let Some(ch) = self.peek() {
            if !predicate(ch) {
                break;
            }
            self.pos += ch.len_utf8();
        }
    }

    fn offset(&self) -> u32 {
        self.pos as u32
    }

    fn push(&mut self, kind: TokenKind, start: u32) {
        self.tokens
            .push(Token::new(kind, Span::new(start, self.offset())));
    }

    fn error(&mut self, code: DiagCode, span: Span, message: impl Into<String>) {
        self.diagnostics
            .push(Diagnostic::error(code, span, message));
    }

    fn error_with_fix(&mut self, code: DiagCode, span: Span, message: impl Into<String>, fix: Fix) {
        self.diagnostics
            .push(Diagnostic::error(code, span, message).with_fix(fix));
    }

    // ----- driver -----

    fn run(mut self) -> Lexed {
        while self.peek().is_some() {
            self.token();
        }
        let end = self.offset();
        self.tokens.push(Token::new(TokenKind::Eof, Span::at(end)));
        Lexed {
            tokens: self.tokens,
            diagnostics: self.diagnostics,
        }
    }

    fn token(&mut self) {
        let start = self.offset();
        let Some(ch) = self.bump() else { return };

        match ch {
            c if c.is_whitespace() => {
                self.eat_while(char::is_whitespace);
                self.push(TokenKind::Whitespace, start);
            }

            '/' => {
                if self.peek() == Some('/') {
                    self.line_comment(start);
                } else if self.eat('=') {
                    self.push(TokenKind::SlashAssign, start);
                } else {
                    self.push(TokenKind::Slash, start);
                }
            }

            '"' => self.string(start),
            '\'' => self.character(start),

            c if c.is_ascii_digit() => self.number(start),

            c if is_ident_start(c) => self.ident(start),

            '+' => {
                let kind = if self.eat('=') {
                    TokenKind::PlusAssign
                } else {
                    TokenKind::Plus
                };
                self.push(kind, start);
            }
            '-' => {
                let kind = if self.eat('=') {
                    TokenKind::MinusAssign
                } else if self.eat('>') {
                    TokenKind::Arrow
                } else {
                    TokenKind::Minus
                };
                self.push(kind, start);
            }
            '*' => {
                let kind = if self.eat('=') {
                    TokenKind::StarAssign
                } else {
                    TokenKind::Star
                };
                self.push(kind, start);
            }
            '%' => {
                let kind = if self.eat('=') {
                    TokenKind::PercentAssign
                } else {
                    TokenKind::Percent
                };
                self.push(kind, start);
            }
            '=' => {
                let kind = if self.eat('=') {
                    TokenKind::EqEq
                } else if self.eat('>') {
                    TokenKind::FatArrow
                } else {
                    TokenKind::Assign
                };
                self.push(kind, start);
            }
            '!' => {
                let kind = if self.eat('=') {
                    TokenKind::NotEq
                } else {
                    TokenKind::Bang
                };
                self.push(kind, start);
            }
            // `<` and `>` are never joined into shift operators here. Doing so
            // would make `Map<String, List<Int>>` lex its final `>>` as one
            // token and break every nested generic. The parser joins adjacent
            // pairs when it wants a shift; spans make adjacency checkable.
            '<' => {
                let kind = if self.eat('=') {
                    TokenKind::LtEq
                } else {
                    TokenKind::Lt
                };
                self.push(kind, start);
            }
            '>' => {
                let kind = if self.eat('=') {
                    TokenKind::GtEq
                } else {
                    TokenKind::Gt
                };
                self.push(kind, start);
            }
            '&' => {
                let kind = if self.eat('&') {
                    TokenKind::AndAnd
                } else {
                    TokenKind::Amp
                };
                self.push(kind, start);
            }
            '|' => {
                let kind = if self.eat('|') {
                    TokenKind::OrOr
                } else {
                    TokenKind::Pipe
                };
                self.push(kind, start);
            }
            '^' => self.push(TokenKind::Caret, start),

            '?' => {
                if self.eat('?') {
                    // `??name` binds the name to the hole; `??` is anonymous.
                    self.eat_while(is_ident_continue);
                    self.push(TokenKind::Hole, start);
                } else {
                    self.push(TokenKind::Question, start);
                }
            }

            '.' => {
                if self.peek().is_some_and(|c| c.is_ascii_digit()) {
                    // `.5` — a float is required to have digits on both sides.
                    self.eat_while(|c| c.is_ascii_digit() || c == '_');
                    let span = Span::new(start, self.offset());
                    self.error_with_fix(
                        DiagCode::MalformedNumber,
                        span,
                        "a float literal needs a digit before the point",
                        Fix::single("write the leading zero", Edit::insert(start, "0")),
                    );
                    self.push(TokenKind::Float, start);
                } else {
                    self.push(TokenKind::Dot, start);
                }
            }

            ',' => self.push(TokenKind::Comma, start),
            ':' => self.push(TokenKind::Colon, start),
            ';' => self.push(TokenKind::Semi, start),
            '(' => self.push(TokenKind::LParen, start),
            ')' => self.push(TokenKind::RParen, start),
            '{' => self.push(TokenKind::LBrace, start),
            '}' => self.push(TokenKind::RBrace, start),
            '[' => self.push(TokenKind::LBracket, start),
            ']' => self.push(TokenKind::RBracket, start),

            other => {
                let span = Span::new(start, self.offset());
                let mut diagnostic = Diagnostic::error(
                    DiagCode::UnexpectedCharacter,
                    span,
                    format!("`{other}` cannot start a token"),
                );
                if let Some(ascii) = ascii_lookalike(other) {
                    diagnostic = diagnostic.with_fix(Fix::single(
                        format!("replace with `{ascii}`"),
                        Edit::replace(span, ascii.to_string()),
                    ));
                }
                self.diagnostics.push(diagnostic);
                self.push(TokenKind::Unknown, start);
            }
        }
    }

    // ----- pieces -----

    fn line_comment(&mut self, start: u32) {
        self.bump(); // the second '/'
        // A third slash makes it a doc comment, but `////` is an ordinary
        // comment again — otherwise a decorative rule of slashes would attach
        // itself to the next declaration as documentation.
        let is_doc = self.peek() == Some('/') && self.peek_at(1) != Some('/');
        if is_doc {
            self.bump();
        }
        self.eat_while(|c| c != '\n');
        let kind = if is_doc {
            TokenKind::DocComment
        } else {
            TokenKind::LineComment
        };
        self.push(kind, start);
    }

    fn ident(&mut self, start: u32) {
        self.eat_while(is_ident_continue);
        let span = Span::new(start, self.offset());
        let text = span.slice(self.source).unwrap_or("");

        if text == "_" {
            self.push(TokenKind::Underscore, start);
            return;
        }

        if let Some(kind) = keyword_kind(text) {
            self.push(kind, start);
            return;
        }

        if is_reserved(text) {
            self.error(
                DiagCode::ReservedKeyword,
                span,
                format!("`{text}` is reserved for a future version of Xenith"),
            );
            // Recover as an ordinary identifier so that parsing continues and
            // the reader gets this one error rather than a cascade.
        }

        self.push(TokenKind::Ident, start);
    }

    fn number(&mut self, start: u32) {
        self.eat_while(|c| c.is_ascii_digit() || c == '_');

        // A point is only part of the literal when a digit follows it.
        // `1.to_text()` is a method call on an integer, not a malformed float.
        if self.peek() == Some('.') {
            match self.peek_at(1) {
                Some(next) if next.is_ascii_digit() => {
                    self.bump();
                    self.eat_while(|c| c.is_ascii_digit() || c == '_');
                    self.push(TokenKind::Float, start);
                    return;
                }
                Some(next) if is_ident_start(next) => {
                    // leave the '.' for the parser: method call
                }
                _ => {
                    // `1.` with nothing usable after it.
                    self.bump();
                    let span = Span::new(start, self.offset());
                    self.error_with_fix(
                        DiagCode::MalformedNumber,
                        span,
                        "a float literal needs a digit after the point",
                        Fix::single("write the trailing zero", Edit::insert(self.offset(), "0")),
                    );
                    self.push(TokenKind::Float, start);
                    return;
                }
            }
        }

        self.push(TokenKind::Int, start);
    }

    fn string(&mut self, start: u32) {
        loop {
            match self.peek() {
                None | Some('\n') => {
                    let span = Span::new(start, self.offset());
                    self.error_with_fix(
                        DiagCode::UnterminatedString,
                        span,
                        "unterminated string literal",
                        Fix::single("close the string", Edit::insert(self.offset(), "\"")),
                    );
                    self.push(TokenKind::Str, start);
                    return;
                }
                Some('"') => {
                    self.bump();
                    self.push(TokenKind::Str, start);
                    return;
                }
                Some('\\') => {
                    self.bump();
                    self.escape();
                }
                Some(_) => {
                    self.bump();
                }
            }
        }
    }

    fn character(&mut self, start: u32) {
        let mut count = 0usize;
        loop {
            match self.peek() {
                None | Some('\n') => {
                    let span = Span::new(start, self.offset());
                    self.error(
                        DiagCode::MalformedChar,
                        span,
                        "unterminated character literal",
                    );
                    self.push(TokenKind::Char, start);
                    return;
                }
                Some('\'') => {
                    self.bump();
                    let span = Span::new(start, self.offset());
                    if count != 1 {
                        let message = if count == 0 {
                            "a character literal cannot be empty"
                        } else {
                            "a character literal must hold exactly one character"
                        };
                        let mut diagnostic =
                            Diagnostic::error(DiagCode::MalformedChar, span, message);
                        if count > 1 {
                            // The contents are almost always a string typed with
                            // the wrong quotes.
                            if let Some(inner) =
                                Span::new(start + 1, self.offset() - 1).slice(self.source)
                            {
                                diagnostic = diagnostic.with_fix(Fix::single(
                                    "use double quotes to write a string",
                                    Edit::replace(span, format!("\"{inner}\"")),
                                ));
                            }
                        }
                        self.diagnostics.push(diagnostic);
                    }
                    self.push(TokenKind::Char, start);
                    return;
                }
                Some('\\') => {
                    self.bump();
                    self.escape();
                    count += 1;
                }
                Some(_) => {
                    self.bump();
                    count += 1;
                }
            }
        }
    }

    /// Consume one escape sequence, having already consumed the backslash.
    fn escape(&mut self) {
        let start = self.offset() - 1; // include the backslash in the span
        match self.peek() {
            Some('n' | 'r' | 't' | '0' | '\\' | '"' | '\'') => {
                self.bump();
            }
            Some(other) => {
                self.bump();
                let span = Span::new(start, self.offset());
                self.error(
                    DiagCode::InvalidEscape,
                    span,
                    format!("`\\{other}` is not a recognised escape"),
                );
            }
            None => {
                // End of input: the enclosing literal reports the real problem.
            }
        }
    }
}

fn is_ident_start(ch: char) -> bool {
    ch == '_' || ch.is_ascii_alphabetic()
}

fn is_ident_continue(ch: char) -> bool {
    ch == '_' || ch.is_ascii_alphanumeric()
}

/// The ASCII character a confusable was probably meant to be.
///
/// Pasting from a chat window or a document is how these arrive, and the
/// resulting error is otherwise baffling because the characters look identical
/// in most terminals.
///
/// Space-like characters are deliberately absent. An ideographic space and a
/// non-breaking space both satisfy [`char::is_whitespace`], so they are
/// consumed as whitespace before reaching here — and that is the right
/// outcome, because whitespace carries no meaning in Xenith (design/0003) and
/// the canonical formatter rewrites it to ASCII anyway. Listing them would be
/// unreachable code that implies a rejection which never happens.
fn ascii_lookalike(ch: char) -> Option<char> {
    let ascii = match ch {
        '“' | '”' | '„' | '＂' => '"',
        '‘' | '’' | '‚' | '＇' => '\'',
        '，' | '、' => ',',
        '；' => ';',
        '：' => ':',
        '（' => '(',
        '）' => ')',
        '｛' => '{',
        '｝' => '}',
        '［' => '[',
        '］' => ']',
        '＋' => '+',
        '－' | '−' | '—' | '–' => '-',
        '＊' | '×' => '*',
        '／' => '/',
        '＝' => '=',
        '＜' => '<',
        '＞' => '>',
        '！' => '!',
        '？' => '?',
        '．' | '。' => '.',
        _ => return None,
    };
    Some(ascii)
}
