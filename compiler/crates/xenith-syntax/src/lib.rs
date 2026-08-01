//! Lexing and parsing for Xenith.
//!
//! Both stages are written by hand rather than generated. Diagnostic quality is
//! a large fraction of what this language is *for* — a model repairs code from
//! compiler output, so the compiler's output is the product — and generated
//! parsers do not produce the errors we need.
//!
//! Both stages are also total. Every input produces a token stream and a
//! [`Module`], with problems reported alongside rather than by refusing to
//! continue. Partial and broken programs are a normal state here, not an
//! exceptional one.
//!
//! [`Module`]: ast::Module

pub mod ast;
pub mod lexer;
pub mod parser;
pub mod token;

pub use lexer::{Lexed, lex};
pub use parser::{Parsed, parse};
pub use token::{RESERVED_WORDS, Token, TokenKind, is_reserved, keyword_kind};
