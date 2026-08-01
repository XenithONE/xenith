//! Lexing and parsing for Xenith.
//!
//! Both stages are written by hand rather than generated. Diagnostic quality is
//! a large fraction of what this language is *for* — a model repairs code from
//! compiler output, so the compiler's output is the product — and generated
//! parsers do not produce the errors we need.

pub mod lexer;
pub mod token;

pub use lexer::{Lexed, lex};
pub use token::{RESERVED_WORDS, Token, TokenKind, is_reserved, keyword_kind};
