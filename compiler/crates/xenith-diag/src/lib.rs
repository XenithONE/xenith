//! Diagnostics for the Xenith compiler.
//!
//! Compiler output is a protocol, not prose. Every diagnostic carries a stable
//! code, a byte span, and — wherever the fix is unambiguous — a machine
//! applicable [`Fix`]. Tools and models are expected to consume this directly;
//! the human rendering is a view over it, not the other way around.
//!
//! See `design/0002-design-review.md` for why this is load-bearing rather than
//! a convenience.

// `Deserialize` must come from the serde root so that both the trait and the
// derive macro are in scope. Importing `serde::de::Deserialize` shadows the
// derive and every `#[derive(Deserialize)]` below silently stops working.
use serde::de::{Deserializer, Error as DeError};
use serde::{Deserialize, Serialize, Serializer};

mod line_index;

pub use line_index::{LineCol, LineIndex};

/// A half-open byte range `[start, end)` into a source file.
///
/// Byte offsets rather than line/column: spans are compared and merged
/// constantly during parsing, and line/column is only needed when rendering.
/// Use [`LineIndex`] to convert at the boundary.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct Span {
    pub start: u32,
    pub end: u32,
}

impl Span {
    pub const EMPTY: Span = Span { start: 0, end: 0 };

    pub fn new(start: u32, end: u32) -> Span {
        debug_assert!(start <= end, "span start must not exceed end");
        Span { start, end }
    }

    /// A zero-width span at `offset`, used to point *between* characters —
    /// which is what an insertion fix needs.
    pub fn at(offset: u32) -> Span {
        Span {
            start: offset,
            end: offset,
        }
    }

    pub fn len(self) -> u32 {
        self.end - self.start
    }

    pub fn is_empty(self) -> bool {
        self.start == self.end
    }

    /// The smallest span covering both operands.
    pub fn to(self, other: Span) -> Span {
        Span {
            start: self.start.min(other.start),
            end: self.end.max(other.end),
        }
    }

    pub fn contains(self, offset: u32) -> bool {
        self.start <= offset && offset < self.end
    }

    /// Slice the source text this span refers to.
    ///
    /// Returns `None` if the span is out of bounds or does not land on
    /// character boundaries, rather than panicking — a diagnostic renderer
    /// should never be the thing that crashes the compiler.
    pub fn slice(self, source: &str) -> Option<&str> {
        source.get(self.start as usize..self.end as usize)
    }
}

impl std::fmt::Debug for Span {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}..{}", self.start, self.end)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Error,
    Warning,
}

/// A stable diagnostic identifier.
///
/// These are part of the public interface: they appear in JSON output, in
/// `xenith explain <code>`, and in the benchmark's repair traces. **Codes are
/// never reused or renumbered**, because a model that learned `XN0002` means
/// "unterminated string" must keep being right.
///
/// Ranges:
///
/// | Range    | Area                        |
/// |----------|-----------------------------|
/// | `XN0xxx` | lexical                     |
/// | `XN1xxx` | syntax                      |
/// | `XN2xxx` | name resolution             |
/// | `XN3xxx` | types                       |
/// | `XN4xxx` | capabilities and effects    |
/// | `XN5xxx` | exhaustiveness              |
/// | `XN6xxx` | concurrency (`Transfer` / `ShareSafe`) |
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum DiagCode {
    /// XN0001 — a character that cannot begin any token.
    UnexpectedCharacter,
    /// XN0002 — a string literal reached end of line or end of file unclosed.
    UnterminatedString,
    /// XN0003 — a backslash escape that is not one of the recognised forms.
    InvalidEscape,
    /// XN0004 — a numeric literal that does not parse.
    MalformedNumber,
    /// XN0005 — a character literal that is unclosed or holds more than one character.
    MalformedChar,
    /// XN0006 — an identifier that is reserved for a future version of Xenith.
    ReservedKeyword,
}

impl DiagCode {
    /// The stable textual identifier, for example `"XN0002"`.
    pub fn id(self) -> &'static str {
        match self {
            DiagCode::UnexpectedCharacter => "XN0001",
            DiagCode::UnterminatedString => "XN0002",
            DiagCode::InvalidEscape => "XN0003",
            DiagCode::MalformedNumber => "XN0004",
            DiagCode::MalformedChar => "XN0005",
            DiagCode::ReservedKeyword => "XN0006",
        }
    }

    pub fn from_id(id: &str) -> Option<DiagCode> {
        let code = match id {
            "XN0001" => DiagCode::UnexpectedCharacter,
            "XN0002" => DiagCode::UnterminatedString,
            "XN0003" => DiagCode::InvalidEscape,
            "XN0004" => DiagCode::MalformedNumber,
            "XN0005" => DiagCode::MalformedChar,
            "XN0006" => DiagCode::ReservedKeyword,
            _ => return None,
        };
        Some(code)
    }

    /// The long-form explanation shown by `xenith explain <code>`.
    ///
    /// Written for a reader who has the error in front of them and wants to
    /// know the rule, not the etymology. Every explanation states the rule and
    /// then how to satisfy it.
    pub fn explain(self) -> &'static str {
        match self {
            DiagCode::UnexpectedCharacter => {
                "This character cannot start a token in Xenith.\n\n\
                 Xenith source is UTF-8. Identifiers begin with a letter or `_`, \
                 and the operator set is fixed — there are no user-defined operators, \
                 so an unrecognised symbol is always an error rather than something \
                 that might be defined elsewhere.\n\n\
                 A common cause is a character that looks like ASCII but is not — a \
                 full-width or \"smart\" quote pasted from a document, or a full-width \
                 comma. Replace it with the ASCII character it resembles.\n\n\
                 Space-like characters are not affected: an ideographic space and a \
                 non-breaking space are both accepted as ordinary whitespace, because \
                 whitespace carries no meaning in Xenith."
            }
            DiagCode::UnterminatedString => {
                "A string literal was opened but never closed.\n\n\
                 String literals may not span lines. If you need a newline inside a \
                 string, write the escape `\\n`.\n\n\
                 If the string was meant to contain a double quote, escape it as `\\\"`."
            }
            DiagCode::InvalidEscape => {
                "This backslash escape is not recognised.\n\n\
                 Xenith accepts exactly these escapes, and no others:\n\n  \
                 \\n  newline\n  \\r  carriage return\n  \\t  tab\n  \\0  null\n  \
                 \\\\  backslash\n  \\\"  double quote\n  \\'  single quote\n\n\
                 The set is deliberately small and closed. To write a literal \
                 backslash, double it: `\\\\`."
            }
            DiagCode::MalformedNumber => {
                "This numeric literal does not parse.\n\n\
                 Integers are decimal digits, optionally separated by `_` for \
                 readability: `1_000_000`. Floats require digits on both sides of the \
                 point: write `1.0`, not `1.` or `.5`.\n\n\
                 There are no implicit numeric conversions in Xenith, so `1` and `1.0` \
                 are different types and the compiler will not silently bridge them."
            }
            DiagCode::MalformedChar => {
                "A character literal must hold exactly one character.\n\n\
                 Write `'a'` for a character and `\"a\"` for a string. An empty `''` \
                 has no value, and `'ab'` is two characters — neither is a `Char`.\n\n\
                 Note that a `Char` is a Unicode scalar value, so `'あ'` is one \
                 character even though it is three bytes."
            }
            DiagCode::ReservedKeyword => {
                "This word is reserved for a future version of Xenith and cannot be \
                 used as an identifier.\n\n\
                 Reserving them now means adding the feature later will not break \
                 existing code. Rename the binding — appending a descriptive word is \
                 usually clearest.\n\n\
                 Reserved words: `trait`, `impl`, `where`, `pub`, `mod`, `loop`, \
                 `defer`, `yield`, `capability`, `effect`, `extern`, `static`, `macro`."
            }
        }
    }
}

impl std::fmt::Display for DiagCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.id())
    }
}

impl Serialize for DiagCode {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.id())
    }
}

impl<'de> Deserialize<'de> for DiagCode {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<DiagCode, D::Error> {
        let id = String::deserialize(deserializer)?;
        DiagCode::from_id(&id)
            .ok_or_else(|| D::Error::custom(format!("unknown diagnostic code `{id}`")))
    }
}

/// A single text replacement.
///
/// An empty `span` inserts; an empty `replacement` deletes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Edit {
    pub span: Span,
    pub replacement: String,
}

impl Edit {
    pub fn replace(span: Span, replacement: impl Into<String>) -> Edit {
        Edit {
            span,
            replacement: replacement.into(),
        }
    }

    pub fn insert(offset: u32, text: impl Into<String>) -> Edit {
        Edit {
            span: Span::at(offset),
            replacement: text.into(),
        }
    }

    pub fn delete(span: Span) -> Edit {
        Edit {
            span,
            replacement: String::new(),
        }
    }
}

/// A fix that can be applied without human judgement.
///
/// Only attach one when it is *unambiguously* correct. A fix that is merely
/// plausible teaches a model to apply suggestions blindly, which is worse than
/// offering nothing.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Fix {
    pub description: String,
    pub edits: Vec<Edit>,
}

impl Fix {
    pub fn new(description: impl Into<String>, edits: Vec<Edit>) -> Fix {
        Fix {
            description: description.into(),
            edits,
        }
    }

    /// Convenience for the common single-edit case.
    pub fn single(description: impl Into<String>, edit: Edit) -> Fix {
        Fix::new(description, vec![edit])
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Diagnostic {
    pub code: DiagCode,
    pub severity: Severity,
    pub span: Span,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fix: Option<Fix>,
}

impl Diagnostic {
    pub fn error(code: DiagCode, span: Span, message: impl Into<String>) -> Diagnostic {
        Diagnostic {
            code,
            severity: Severity::Error,
            span,
            message: message.into(),
            fix: None,
        }
    }

    pub fn warning(code: DiagCode, span: Span, message: impl Into<String>) -> Diagnostic {
        Diagnostic {
            code,
            severity: Severity::Warning,
            span,
            message: message.into(),
            fix: None,
        }
    }

    pub fn with_fix(mut self, fix: Fix) -> Diagnostic {
        self.fix = Some(fix);
        self
    }

    pub fn is_error(&self) -> bool {
        self.severity == Severity::Error
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn span_merges_to_the_outer_bounds() {
        let a = Span::new(3, 7);
        let b = Span::new(10, 12);
        assert_eq!(a.to(b), Span::new(3, 12));
        assert_eq!(b.to(a), Span::new(3, 12));
    }

    #[test]
    fn span_contains_is_half_open() {
        let s = Span::new(2, 5);
        assert!(!s.contains(1));
        assert!(s.contains(2));
        assert!(s.contains(4));
        assert!(!s.contains(5), "end offset is excluded");
    }

    #[test]
    fn span_slice_of_out_of_bounds_range_is_none_not_a_panic() {
        assert_eq!(Span::new(0, 3).slice("abcdef"), Some("abc"));
        assert_eq!(Span::new(0, 99).slice("abc"), None);
    }

    #[test]
    fn span_slice_inside_a_multibyte_character_is_none_not_a_panic() {
        // "あ" is three bytes; offset 1 is not a character boundary.
        assert_eq!(Span::new(0, 1).slice("あ"), None);
        assert_eq!(Span::new(0, 3).slice("あ"), Some("あ"));
    }

    #[test]
    fn every_code_round_trips_through_its_id() {
        let codes = [
            DiagCode::UnexpectedCharacter,
            DiagCode::UnterminatedString,
            DiagCode::InvalidEscape,
            DiagCode::MalformedNumber,
            DiagCode::MalformedChar,
            DiagCode::ReservedKeyword,
        ];
        for code in codes {
            assert_eq!(DiagCode::from_id(code.id()), Some(code), "{}", code.id());
        }
    }

    #[test]
    fn codes_are_unique() {
        let codes = [
            DiagCode::UnexpectedCharacter,
            DiagCode::UnterminatedString,
            DiagCode::InvalidEscape,
            DiagCode::MalformedNumber,
            DiagCode::MalformedChar,
            DiagCode::ReservedKeyword,
        ];
        let mut ids: Vec<&str> = codes.iter().map(|c| c.id()).collect();
        ids.sort_unstable();
        let count = ids.len();
        ids.dedup();
        assert_eq!(ids.len(), count, "two variants share a diagnostic id");
    }

    #[test]
    fn every_code_has_a_non_empty_explanation() {
        let codes = [
            DiagCode::UnexpectedCharacter,
            DiagCode::UnterminatedString,
            DiagCode::InvalidEscape,
            DiagCode::MalformedNumber,
            DiagCode::MalformedChar,
            DiagCode::ReservedKeyword,
        ];
        for code in codes {
            assert!(
                code.explain().len() > 40,
                "{} needs a real explanation",
                code.id()
            );
        }
    }

    #[test]
    fn json_uses_the_stable_code_string_not_the_variant_name() {
        let diag = Diagnostic::error(
            DiagCode::UnterminatedString,
            Span::new(4, 9),
            "unterminated string literal",
        )
        .with_fix(Fix::single("close the string", Edit::insert(9, "\"")));

        let json = serde_json::to_value(&diag).unwrap();
        assert_eq!(json["code"], "XN0002");
        assert_eq!(json["severity"], "error");
        assert_eq!(json["span"]["start"], 4);
        assert_eq!(json["fix"]["edits"][0]["replacement"], "\"");

        let back: Diagnostic = serde_json::from_value(json).unwrap();
        assert_eq!(back, diag);
    }

    #[test]
    fn absent_fix_is_omitted_from_json_rather_than_null() {
        let diag = Diagnostic::error(DiagCode::MalformedNumber, Span::new(0, 2), "bad number");
        let json = serde_json::to_value(&diag).unwrap();
        assert!(
            json.get("fix").is_none(),
            "a missing fix should not appear as null"
        );
    }
}
