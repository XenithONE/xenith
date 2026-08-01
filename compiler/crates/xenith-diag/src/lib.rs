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

    /// XN1001 — a specific token was required here and something else appeared.
    ExpectedToken,
    /// XN1002 — a statement ended without its `;`.
    MissingSemicolon,
    /// XN1003 — an opening delimiter was never closed.
    UnclosedDelimiter,
    /// XN1004 — an expression was required here.
    ExpectedExpression,
    /// XN1005 — a type was required here.
    ExpectedType,
    /// XN1006 — a pattern was required here.
    ExpectedPattern,
    /// XN1007 — a declaration was required at the top level of a module.
    ExpectedItem,

    /// XN2001 — a type name that resolves to nothing.
    UnknownType,
    /// XN2002 — a value name that resolves to nothing.
    UnknownName,
    /// XN2003 — the receiver's type has no method of this name.
    UnknownMethod,
    /// XN2004 — the type has no field of this name.
    UnknownField,
    /// XN2005 — two declarations share a name.
    DuplicateDefinition,
    /// XN2006 — the enum has no variant of this name.
    UnknownVariant,

    /// XN3001 — one type was required and a different one was found.
    TypeMismatch,
    /// XN3002 — a call supplied the wrong number of arguments.
    WrongArgumentCount,
    /// XN3003 — a named argument does not match the parameter at its position.
    ArgumentNameMismatch,
    /// XN3004 — the expression being called is not a function.
    NotCallable,
    /// XN3005 — a type annotation is required here.
    AnnotationRequired,
    /// XN3006 — a generic bound names a property that does not exist.
    UnknownProperty,
    /// XN3007 — a struct literal leaves a field unset.
    MissingField,
    /// XN3008 — a call with two or more arguments must name them.
    NamedArgumentsRequired,
    /// XN3009 — assignment through a `let` binding or to a non-`var` field.
    AssignmentToImmutable,
    /// XN3010 — a type does not satisfy a required sealed property.
    PropertyNotSatisfied,

    /// XN4001 — a call performs an effect the enclosing function does not declare.
    EffectNotPermitted,
}

impl DiagCode {
    /// Every code, so that tests and `xenith explain --list` never fall out of
    /// step with the enum.
    pub const ALL: &'static [DiagCode] = &[
        DiagCode::UnexpectedCharacter,
        DiagCode::UnterminatedString,
        DiagCode::InvalidEscape,
        DiagCode::MalformedNumber,
        DiagCode::MalformedChar,
        DiagCode::ReservedKeyword,
        DiagCode::ExpectedToken,
        DiagCode::MissingSemicolon,
        DiagCode::UnclosedDelimiter,
        DiagCode::ExpectedExpression,
        DiagCode::ExpectedType,
        DiagCode::ExpectedPattern,
        DiagCode::ExpectedItem,
        DiagCode::UnknownType,
        DiagCode::UnknownName,
        DiagCode::UnknownMethod,
        DiagCode::UnknownField,
        DiagCode::DuplicateDefinition,
        DiagCode::UnknownVariant,
        DiagCode::TypeMismatch,
        DiagCode::WrongArgumentCount,
        DiagCode::ArgumentNameMismatch,
        DiagCode::NotCallable,
        DiagCode::AnnotationRequired,
        DiagCode::UnknownProperty,
        DiagCode::MissingField,
        DiagCode::NamedArgumentsRequired,
        DiagCode::AssignmentToImmutable,
        DiagCode::PropertyNotSatisfied,
        DiagCode::EffectNotPermitted,
    ];

    /// The stable textual identifier, for example `"XN0002"`.
    pub fn id(self) -> &'static str {
        match self {
            DiagCode::UnexpectedCharacter => "XN0001",
            DiagCode::UnterminatedString => "XN0002",
            DiagCode::InvalidEscape => "XN0003",
            DiagCode::MalformedNumber => "XN0004",
            DiagCode::MalformedChar => "XN0005",
            DiagCode::ReservedKeyword => "XN0006",
            DiagCode::ExpectedToken => "XN1001",
            DiagCode::MissingSemicolon => "XN1002",
            DiagCode::UnclosedDelimiter => "XN1003",
            DiagCode::ExpectedExpression => "XN1004",
            DiagCode::ExpectedType => "XN1005",
            DiagCode::ExpectedPattern => "XN1006",
            DiagCode::ExpectedItem => "XN1007",
            DiagCode::UnknownType => "XN2001",
            DiagCode::UnknownName => "XN2002",
            DiagCode::UnknownMethod => "XN2003",
            DiagCode::UnknownField => "XN2004",
            DiagCode::DuplicateDefinition => "XN2005",
            DiagCode::UnknownVariant => "XN2006",
            DiagCode::TypeMismatch => "XN3001",
            DiagCode::WrongArgumentCount => "XN3002",
            DiagCode::ArgumentNameMismatch => "XN3003",
            DiagCode::NotCallable => "XN3004",
            DiagCode::AnnotationRequired => "XN3005",
            DiagCode::UnknownProperty => "XN3006",
            DiagCode::MissingField => "XN3007",
            DiagCode::NamedArgumentsRequired => "XN3008",
            DiagCode::AssignmentToImmutable => "XN3009",
            DiagCode::PropertyNotSatisfied => "XN3010",
            DiagCode::EffectNotPermitted => "XN4001",
        }
    }

    pub fn from_id(id: &str) -> Option<DiagCode> {
        DiagCode::ALL.iter().copied().find(|c| c.id() == id)
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
            DiagCode::ExpectedToken => {
                "The grammar requires a specific token at this position and a \
                 different one appeared.\n\n\
                 The message names what was required. When the required token is \
                 punctuation that was simply left out, an applicable fix is attached \
                 and inserting it is safe."
            }
            DiagCode::MissingSemicolon => {
                "Statements are terminated by `;`.\n\n\
                 Xenith gives whitespace no meaning at all: newlines and indentation \
                 never change what a program does. That property is what makes `;` \
                 mandatory — without a terminator the parser would have to infer \
                 statement boundaries from line breaks, and reflowing a long line \
                 could silently change behaviour.\n\n\
                 A block's value is its final expression, written *without* a \
                 trailing `;`. Adding one makes the block evaluate to `unit`, which \
                 is usually reported as a type mismatch rather than here."
            }
            DiagCode::UnclosedDelimiter => {
                "An opening `(`, `[` or `{` has no matching close.\n\n\
                 The span points at the opening delimiter, which is nearly always \
                 closer to the real mistake than the end of the file is.\n\n\
                 Parsing continues past this point, so later errors in the same run \
                 may be consequences of this one. Fix this first."
            }
            DiagCode::ExpectedExpression => {
                "An expression was required at this position.\n\n\
                 If you do not yet know what belongs here, write a hole rather than \
                 leaving it blank: `??` on its own, or `??name` to refer to it later. \
                 A hole is a legal program element — the code still compiles, and \
                 `xenith goals` will report the type required here, what is in scope, \
                 and which effects are permitted."
            }
            DiagCode::ExpectedType => {
                "A type was required at this position.\n\n\
                 Xenith does not infer types across function boundaries, so every \
                 parameter, return type and field must be written out. That is \
                 deliberate: with whole-program inference, changing one expression can \
                 flip the type of something far away, which makes a program \
                 unrepairable in small steps.\n\n\
                 Types may also be holes: `??` asks the compiler what would fit."
            }
            DiagCode::ExpectedPattern => {
                "A pattern was required at this position.\n\n\
                 Patterns appear in `let`, `var`, `for`, and `match` arms. The forms \
                 are: a binding (`total`), a wildcard (`_`), a literal (`0`, `\"ok\"`), \
                 a path (`Rank.Gold`), a variant with fields (`Ok(value)`), a struct \
                 pattern (`Player { name, score }`), and alternatives joined by `|`."
            }
            DiagCode::ExpectedItem => {
                "Only declarations may appear at the top level of a module.\n\n\
                 Those are `use`, `const`, `fn`, `struct` and `enum`. Statements and \
                 expressions belong inside a function body.\n\n\
                 A program starts at `fn main`, which receives its capabilities as \
                 parameters — there is no ambient environment to reach for, so there \
                 is nothing a top-level statement could usefully do."
            }
            DiagCode::UnknownType => {
                "This type name does not resolve to anything.\n\n\
                 A type is either built in (`Int`, `Float`, `Bool`, `String`, `Char`, \
                 `Unit`, `List`, `Option`, `Result`, `Map`, `Shared`, `Task`), a \
                 `struct` or `enum` declared in this module, or a generic parameter of \
                 the enclosing declaration. There are no implicit imports.\n\n\
                 If you do not yet know the type, `??` is legal in type position and \
                 becomes a goal instead of an error."
            }
            DiagCode::UnknownName => {
                "This name does not resolve to anything in scope.\n\n\
                 Values come from `let` and `var` bindings, function parameters, \
                 pattern bindings, module functions and constants. Enum variants are \
                 named through their enum (`Rank.Gold`); only the variants of \
                 `Option` and `Result` (`Some`, `None`, `Ok`, `Err`) may be used \
                 unqualified.\n\n\
                 If the value does not exist yet, write a hole: `??name`. The \
                 compiler will report what type is required there and what is in \
                 scope to build it from."
            }
            DiagCode::UnknownMethod => {
                "The receiver's type has no method of this name.\n\n\
                 The message names the receiver's type. Methods in Xenith are \
                 currently provided by the language, not declared by user code, so a \
                 misspelling is the usual cause — the naming rules make the correct \
                 spelling guessable: `to_` converts totally, `try_` returns `Result`, \
                 `is_`/`has_` return `Bool`, `checked_` returns `Option`."
            }
            DiagCode::UnknownField => {
                "The type has no field of this name.\n\n\
                 Field names are declared on the struct and are the only way to reach \
                 its contents — there is no index-based or reflective access. Check \
                 the struct's declaration; the diagnostic names the type so the \
                 declaration is easy to find."
            }
            DiagCode::DuplicateDefinition => {
                "Two declarations in this module share a name.\n\n\
                 Names at the top level are unique, with no overloading and no \
                 shadowing between declarations. Overloading is excluded deliberately: \
                 when two functions share a name, every reader — human or model — has \
                 to re-derive which one a call site means. Rename one of them; the \
                 naming rules suggest encoding what distinguishes them (`from_text`, \
                 `from_bytes`) rather than reusing the name."
            }
            DiagCode::UnknownVariant => {
                "The enum has no variant of this name.\n\n\
                 The diagnostic names the enum; its declaration lists the variants. \
                 In a `match`, a lowercase name that is not a variant becomes a \
                 binding that matches everything — so a misspelt variant name here \
                 would otherwise silently change what the arm does. That is why \
                 variant names are checked against the scrutinee's enum."
            }
            DiagCode::TypeMismatch => {
                "One type was required at this position and a different one was \
                 found.\n\n\
                 Xenith performs no implicit conversions: `Int` and `Float` never \
                 bridge silently, and `1` and `1.0` are different values of different \
                 types. Conversions are spelled out with `to_` functions, which are \
                 total, or `try_` functions where they can fail.\n\n\
                 If the two types in the message look the same, check their \
                 arguments: `List<Int>` and `List<Float>` differ in the argument, \
                 not the head."
            }
            DiagCode::WrongArgumentCount => {
                "This call supplies the wrong number of arguments.\n\n\
                 The message states how many the callee declares and how many were \
                 given. Xenith has no default arguments, no variadics and no \
                 overloading, so the declared count is exact — every parameter is \
                 filled at every call site. If you do not have a value for one yet, \
                 pass a hole: `f(config: ??)`."
            }
            DiagCode::ArgumentNameMismatch => {
                "This named argument does not match the parameter declared at its \
                 position.\n\n\
                 Named arguments follow declaration order, and each name must match \
                 the parameter it lands on. The fix carries the declared name; \
                 applying it is safe. If the values themselves are in the wrong \
                 order, reorder them instead — the mismatch this rule exists to \
                 catch."
            }
            DiagCode::NotCallable => {
                "The expression being called is not a function.\n\n\
                 Only functions, lambdas, and enum variant constructors can be \
                 applied. A common cause is calling a value that merely holds the \
                 result of a function: `total()` when `total` is already an `Int`."
            }
            DiagCode::AnnotationRequired => {
                "The type here cannot be determined locally, so an annotation is \
                 required.\n\n\
                 Xenith infers types only within an expression, never across \
                 bindings or declarations — inference at a distance means an edit in \
                 one place can change a type far away, which makes programs \
                 unrepairable in small steps.\n\n\
                 The usual causes: a hole with nothing to say what it should be \
                 (`let x = ??;` — annotate the binding: `let x: Config = ??;`), or a \
                 constructor whose type parameter no argument pins down \
                 (`let r = Ok(5);` — annotate: `let r: Result<Int, ApiError> = \
                 Ok(5);`)."
            }
            DiagCode::UnknownProperty => {
                "A generic bound names a property that does not exist.\n\n\
                 The property set is sealed: `Eq`, `Ord`, `Hash`, `Copy`, `Text`. \
                 It cannot be extended by user code, which is what makes bound \
                 checking a table lookup instead of a search — there is never a \
                 question of which implementation applies, because there are no \
                 implementations to choose between."
            }
            DiagCode::MissingField => {
                "This struct literal leaves a field unset.\n\n\
                 Every field is filled at every construction site; there are no \
                 default values. This is what makes adding a field to a struct a \
                 safe change — the compiler points at every construction site that \
                 needs updating.\n\n\
                 The fix inserts the missing field with a hole as its value, so the \
                 literal compiles and `xenith goals` reports what belongs there."
            }
            DiagCode::NamedArgumentsRequired => {
                "Calls with two or more arguments must name them.\n\n\
                 `move(2, 3)` reads fine until the parameters are `(dy, dx)` — \
                 swapped positional arguments type-check and then misbehave at \
                 runtime, and models make exactly this mistake at scale. \
                 `move(dx: 2, dy: 3)` cannot be swapped silently.\n\n\
                 Single-argument calls may omit the name. Enum variant payloads are \
                 unnamed and stay positional: `Ok(value)`, `NotFound(id)`."
            }
            DiagCode::AssignmentToImmutable => {
                "This assignment writes through something not declared mutable.\n\n\
                 Bindings are immutable unless introduced with `var`, and fields are \
                 immutable unless declared `var` in the struct. Mutability is spelled \
                 at the declaration, not the use — one line to read to know whether \
                 a thing can change.\n\n\
                 If the mutation is intended, change `let` to `var` at the binding, \
                 or mark the field `var` in the struct declaration."
            }
            DiagCode::PropertyNotSatisfied => {
                "A type does not satisfy a sealed property that this position \
                 requires.\n\n\
                 Properties are decided structurally from the type's definition and \
                 cannot be implemented by hand. Scalars and `String` satisfy `Eq`, \
                 `Ord` and `Hash`; a struct or enum satisfies `Eq` and `Hash` when \
                 every field does; function types, capabilities and `Shared` satisfy \
                 none of them (`Shared` identity is compared with `is`).\n\n\
                 Two deliberate exclusions, both from IEEE NaN: `Float` satisfies \
                 `Eq` but neither `Ord` nor `Hash`, so it cannot be a sort key or a \
                 `Map` key. Aggregates never satisfy `Ord`, because ordering derived \
                 from field declaration order changes when fields are reordered. In \
                 both cases, pass an explicit comparison: `sorted_by(compare: ...)`. \
                 What runs is then readable at the call site."
            }
            DiagCode::EffectNotPermitted => {
                "This call performs an effect the enclosing function does not \
                 declare.\n\n\
                 A function may only perform the effects in its `uses { .. }` \
                 clause; an absent clause means none at all. The check is what makes \
                 a signature trustworthy — a caller reads `uses {Fs.read}` and knows \
                 the function touches nothing else.\n\n\
                 Two fixes, and which is right is a design decision: add the effect \
                 to this function's `uses` clause (the attached fix does this), \
                 which also widens what *its* callers must permit — or move the \
                 effectful call out to a caller that already holds the capability, \
                 and pass the result in as a value."
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
        for &code in DiagCode::ALL {
            assert_eq!(DiagCode::from_id(code.id()), Some(code), "{}", code.id());
        }
    }

    #[test]
    fn codes_are_unique() {
        let mut ids: Vec<&str> = DiagCode::ALL.iter().map(|c| c.id()).collect();
        ids.sort_unstable();
        let count = ids.len();
        ids.dedup();
        assert_eq!(ids.len(), count, "two variants share a diagnostic id");
    }

    #[test]
    fn every_code_has_a_non_empty_explanation() {
        for &code in DiagCode::ALL {
            assert!(
                code.explain().len() > 40,
                "{} needs a real explanation",
                code.id()
            );
        }
    }

    #[test]
    fn all_lists_every_variant() {
        // `ALL` drives the tests above, so a variant missing from it would be
        // silently untested. This match is exhaustive on purpose and has no
        // `_` arm: adding a variant stops this file compiling until someone
        // looks here, and the count then catches a variant that was added to
        // the match but not to `ALL`.
        let mut seen = 0;
        for &code in DiagCode::ALL {
            match code {
                DiagCode::UnexpectedCharacter
                | DiagCode::UnterminatedString
                | DiagCode::InvalidEscape
                | DiagCode::MalformedNumber
                | DiagCode::MalformedChar
                | DiagCode::ReservedKeyword
                | DiagCode::ExpectedToken
                | DiagCode::MissingSemicolon
                | DiagCode::UnclosedDelimiter
                | DiagCode::ExpectedExpression
                | DiagCode::ExpectedType
                | DiagCode::ExpectedPattern
                | DiagCode::ExpectedItem
                | DiagCode::UnknownType
                | DiagCode::UnknownName
                | DiagCode::UnknownMethod
                | DiagCode::UnknownField
                | DiagCode::DuplicateDefinition
                | DiagCode::UnknownVariant
                | DiagCode::TypeMismatch
                | DiagCode::WrongArgumentCount
                | DiagCode::ArgumentNameMismatch
                | DiagCode::NotCallable
                | DiagCode::AnnotationRequired
                | DiagCode::UnknownProperty
                | DiagCode::MissingField
                | DiagCode::NamedArgumentsRequired
                | DiagCode::AssignmentToImmutable
                | DiagCode::PropertyNotSatisfied
                | DiagCode::EffectNotPermitted => seen += 1,
            }
        }
        assert_eq!(seen, 30, "update DiagCode::ALL when adding a variant");
        assert_eq!(DiagCode::ALL.len(), 30);
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
