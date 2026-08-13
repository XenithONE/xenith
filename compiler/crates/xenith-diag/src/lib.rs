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
/// | `XN7xxx` | modules and project layout  |
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
    /// XN1008 — a construct the parser accepts for recovery but the shipped
    /// language does not include.
    UnshippedConstruct,
    /// XN1009 — a closure parameter carries a type annotation.
    ClosureAnnotation,
    /// XN1010 — a Rust-shaped closure form: `move`, `async`, reference or
    /// destructuring patterns in the parameter list.
    ClosureRustForm,
    /// XN1011 — a closure written somewhere other than a call argument
    /// matching a `fn(..)` parameter.
    ClosureOutsideCall,
    /// XN1012 — `?`, `return`, `break` or `continue` trying to cross a
    /// closure boundary.
    ClosureEarlyExit,

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
    /// XN2007 — a `use` names a module the project does not contain.
    UnknownModule,
    /// XN2008 — a cross-module reference to an item that is not `pub`.
    PrivateItemAccess,
    /// XN2009 — a `use` declares a dependency nothing in the file consumes.
    UnusedUse,
    /// XN2010 — the same module is `use`d more than once.
    DuplicateUse,

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
    /// XN3011 — a struct or enum contains itself by value.
    InfiniteSizeType,
    /// XN3012 — a `const` initializer is not a constant expression.
    NotConstant,

    /// XN4001 — a call performs an effect the enclosing function does not declare.
    EffectNotPermitted,
    /// XN4005 — a closure captures a value whose type is not CaptureSafe.
    CapabilityCapture,
    /// XN4006 — a closure body performs a capability effect.
    EffectInClosure,
    /// XN4007 — a closure references the binding its own `let` is initializing.
    ClosureSelfReference,
    /// XN4008 — a closure captures a `var` binding.
    CaptureOfVar,

    /// XN5001 — a `match` does not cover every possible value.
    NonExhaustiveMatch,

    /// XN6001 — `spawn` outside a `scope { .. }` block.
    SpawnOutsideScope,
    /// XN6002 — the spawned callee declares a non-empty `uses` set.
    SpawnEffectfulCallee,
    /// XN6003 — a spawn argument's type is not CaptureSafe.
    SpawnArgumentNotCaptureSafe,
    /// XN6004 — the spawned callee is not a directly named fn.
    SpawnCalleeNotFn,
    /// XN6005 — a Join used as anything other than the receiver of `.await`.
    JoinEscape,
    /// XN6006 — a Join awaited more than once (or possibly more than once).
    JoinAwaitedTwice,
    /// XN6007 — a Join awaited on some control-flow paths but not others.
    JoinPartialAwait,
    /// XN6008 — a non-Unit Join never awaited before normal scope exit.
    JoinUnawaited,
    /// XN6009 — statement-form `spawn f(..);` of a callee with a result.
    SpawnStatementNotUnit,
    /// XN6010 — `scope` / `spawn` / `.await` inside a closure body.
    TaskInClosure,

    /// XN7001 — a source file cannot name a module (bad segment, symlink).
    InvalidModulePath,
    /// XN7002 — two module paths differ only by letter case.
    ModuleCaseCollision,
    /// XN7003 — a module path collides with a top-level item of its parent.
    ModuleItemClash,
    /// XN7004 — `fn main` outside `src/main.xn`.
    MisplacedMain,
    /// XN7005 — a local module claims the reserved root `std`.
    ReservedModuleRoot,
    /// XN7006 — a manifest nested inside another project's sources.
    NestedManifest,
    /// XN7007 — a `pub` signature mentions a private type.
    PubApiPrivateType,
    /// XN7008 — a field assignment crosses a module boundary.
    CrossModuleAssignment,
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
        DiagCode::UnshippedConstruct,
        DiagCode::ClosureAnnotation,
        DiagCode::ClosureRustForm,
        DiagCode::ClosureOutsideCall,
        DiagCode::ClosureEarlyExit,
        DiagCode::UnknownType,
        DiagCode::UnknownName,
        DiagCode::UnknownMethod,
        DiagCode::UnknownField,
        DiagCode::DuplicateDefinition,
        DiagCode::UnknownVariant,
        DiagCode::UnknownModule,
        DiagCode::PrivateItemAccess,
        DiagCode::UnusedUse,
        DiagCode::DuplicateUse,
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
        DiagCode::InfiniteSizeType,
        DiagCode::NotConstant,
        DiagCode::EffectNotPermitted,
        DiagCode::CapabilityCapture,
        DiagCode::EffectInClosure,
        DiagCode::ClosureSelfReference,
        DiagCode::CaptureOfVar,
        DiagCode::NonExhaustiveMatch,
        DiagCode::SpawnOutsideScope,
        DiagCode::SpawnEffectfulCallee,
        DiagCode::SpawnArgumentNotCaptureSafe,
        DiagCode::SpawnCalleeNotFn,
        DiagCode::JoinEscape,
        DiagCode::JoinAwaitedTwice,
        DiagCode::JoinPartialAwait,
        DiagCode::JoinUnawaited,
        DiagCode::SpawnStatementNotUnit,
        DiagCode::TaskInClosure,
        DiagCode::InvalidModulePath,
        DiagCode::ModuleCaseCollision,
        DiagCode::ModuleItemClash,
        DiagCode::MisplacedMain,
        DiagCode::ReservedModuleRoot,
        DiagCode::NestedManifest,
        DiagCode::PubApiPrivateType,
        DiagCode::CrossModuleAssignment,
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
            DiagCode::UnshippedConstruct => "XN1008",
            DiagCode::ClosureAnnotation => "XN1009",
            DiagCode::ClosureRustForm => "XN1010",
            DiagCode::ClosureOutsideCall => "XN1011",
            DiagCode::ClosureEarlyExit => "XN1012",
            DiagCode::UnknownType => "XN2001",
            DiagCode::UnknownName => "XN2002",
            DiagCode::UnknownMethod => "XN2003",
            DiagCode::UnknownField => "XN2004",
            DiagCode::DuplicateDefinition => "XN2005",
            DiagCode::UnknownVariant => "XN2006",
            DiagCode::UnknownModule => "XN2007",
            DiagCode::PrivateItemAccess => "XN2008",
            DiagCode::UnusedUse => "XN2009",
            DiagCode::DuplicateUse => "XN2010",
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
            DiagCode::InfiniteSizeType => "XN3011",
            DiagCode::NotConstant => "XN3012",
            DiagCode::EffectNotPermitted => "XN4001",
            DiagCode::CapabilityCapture => "XN4005",
            DiagCode::EffectInClosure => "XN4006",
            DiagCode::ClosureSelfReference => "XN4007",
            DiagCode::CaptureOfVar => "XN4008",
            DiagCode::NonExhaustiveMatch => "XN5001",
            DiagCode::SpawnOutsideScope => "XN6001",
            DiagCode::SpawnEffectfulCallee => "XN6002",
            DiagCode::SpawnArgumentNotCaptureSafe => "XN6003",
            DiagCode::SpawnCalleeNotFn => "XN6004",
            DiagCode::JoinEscape => "XN6005",
            DiagCode::JoinAwaitedTwice => "XN6006",
            DiagCode::JoinPartialAwait => "XN6007",
            DiagCode::JoinUnawaited => "XN6008",
            DiagCode::SpawnStatementNotUnit => "XN6009",
            DiagCode::TaskInClosure => "XN6010",
            DiagCode::InvalidModulePath => "XN7001",
            DiagCode::ModuleCaseCollision => "XN7002",
            DiagCode::ModuleItemClash => "XN7003",
            DiagCode::MisplacedMain => "XN7004",
            DiagCode::ReservedModuleRoot => "XN7005",
            DiagCode::NestedManifest => "XN7006",
            DiagCode::PubApiPrivateType => "XN7007",
            DiagCode::CrossModuleAssignment => "XN7008",
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
                 Reserved words: `trait`, `impl`, `where`, `mod`, `loop`, \
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
            DiagCode::UnshippedConstruct => {
                "This construct is parsed but is not part of the shipped language.\n\n\
                 The parser is total: it accepts `async fn`, `for`, function-type \
                 annotations and named functions used as values, so that a broken \
                 or half-edited file still yields a tree to repair from. Accepting \
                 the syntax is not shipping the feature — each of these is gated on \
                 a future RFC, and passing them through half-checked would let an \
                 effect escape its declaration.\n\n\
                 Closures shipped with design/0014, but only as call arguments to \
                 `fn(..)`-typed parameters (the std combinators `map`, `filter`, \
                 `fold`, `find`). Function types still cannot be written in user \
                 code, and a named function is still called, never passed: wrap the \
                 call in a closure instead. Iterate with `while` + `len()` + \
                 `get(index:)`.\n\n\
                 `.await` shipped with design/0015, but in exactly one position: \
                 on a task handle bound by `let name = spawn f(..);` inside a \
                 `scope { .. }` block. On anything else `.await` remains refused, \
                 and `async fn` / async closures remain out of the language."
            }
            DiagCode::ClosureAnnotation => {
                "Closure parameters take no type annotation.\n\n\
                 There is no `|x: Int|` form. A closure is written only as a call \
                 argument, and the parameter types come from the `fn(..)` type of \
                 the parameter it is passed to — `xs.map(f: fn(x: Int) -> U)` \
                 already fixes what `x` is, so an annotation could only agree or \
                 lie.\n\n\
                 Write the bare name: `xs.map(|x| x + 1)`. The attached fix deletes \
                 the annotation."
            }
            DiagCode::ClosureRustForm => {
                "This closure uses a form from another language that Xenith does \
                 not have.\n\n\
                 There is no `move` (a closure always copies the values it uses at \
                 creation — there is nothing else it could do), no `async` closure, \
                 no reference patterns (`|&x|` — the closure receives a copy), no \
                 `mut` parameters, and no destructuring in the parameter list.\n\n\
                 A closure parameter is a plain name or `_`. Closures are plans: \
                 effects run in the enclosing named fn's `while` loop, and the \
                 closure returns data."
            }
            DiagCode::ClosureOutsideCall => {
                "A closure can be written in exactly one place: as a call argument \
                 for a parameter declared with a `fn(..)` type — in v1, the std \
                 combinators `map`, `filter`, `fold` and `find`.\n\n\
                 It cannot be bound with `let`, returned, stored in a container or \
                 a field, or passed where a non-function type is expected. This is \
                 what keeps \"call a function value\" out of the language: a \
                 closure that cannot be stored cannot be called from somewhere its \
                 effects were never checked.\n\n\
                 Either inline the closure at the call that consumes it, or extract \
                 a named fn and call that instead."
            }
            DiagCode::ClosureEarlyExit => {
                "`?`, `return`, `break` and `continue` cannot cross a closure \
                 boundary.\n\n\
                 A closure body is an expression that produces a value for its \
                 combinator — `map` collects it, `filter` and `find` test it, \
                 `fold` threads it. There is no enclosing function to return from \
                 and no enclosing loop to break: the combinator owns the \
                 iteration.\n\n\
                 Closures cannot early-return; failure-carrying iteration belongs \
                 in a `while` loop of the enclosing named fn, where `?`, `break` \
                 and `return` all mean what they say."
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
                 scope to build it from.\n\n\
                 When exactly one known name sits within two edits of what was \
                 written, the message suggests it; a tie between equally close \
                 names suggests nothing."
            }
            DiagCode::UnknownMethod => {
                "The receiver's type has no method of this name.\n\n\
                 The message names the receiver's type. Methods in Xenith are \
                 currently provided by the language, not declared by user code, so a \
                 misspelling is the usual cause — the naming rules make the correct \
                 spelling guessable: `to_` converts totally, `try_` returns `Result`, \
                 `is_`/`has_` return `Bool`, `checked_` returns `Option`.\n\n\
                 A type declared by a module has no methods at all. Its operations \
                 are the defining module's `pub` functions, which take the value as \
                 an ordinary parameter and are called module-qualified: not \
                 `locker.stow(load: 12)` but \
                 `depot.locker.stow(locker: locker, load: 12)`. When the module \
                 exports functions taking the receiver's type, the diagnostic \
                 lists them with the rewritten call shape.\n\n\
                 When exactly one of the receiver's methods sits within two edits \
                 of what was written, the message suggests it; a tie between \
                 equally close names suggests nothing."
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
            DiagCode::UnknownModule => {
                "This `use` names a module the project does not contain.\n\n\
                 A module is a file under `src/`: `src/game/player.xn` is the \
                 module `game.player`, and `use game.player;` declares a \
                 dependency on it. `use` takes module paths only — there is no \
                 item `use`, no glob, and no alias; items are referenced fully \
                 qualified (`game.player.Player`) once the module is `use`d.\n\n\
                 Check the path against the files under `src/`."
            }
            DiagCode::PrivateItemAccess => {
                "This reference crosses a module boundary to an item that is \
                 not `pub`.\n\n\
                 Top-level items are private to their module unless declared \
                 `pub`. There is no parent or child privilege: a module's \
                 private items are its own from every other module equally.\n\n\
                 No fix is attached on purpose — making an item `pub` is an API \
                 decision for the owning module, not a local syntax repair. \
                 Either use the module's public surface, or widen it there."
            }
            DiagCode::UnusedUse => {
                "This `use` declares a dependency nothing in the file consumes.\n\n\
                 The `use` list is the file's exact dependency list — that is \
                 what makes it worth reading. An unused entry is a hard error \
                 rather than a lint so the list cannot silently rot, and so \
                 \"use everything, then guess\" never becomes a strategy.\n\n\
                 Delete the line, or reference the module."
            }
            DiagCode::DuplicateUse => {
                "The same module is `use`d more than once.\n\n\
                 One dependency, one line: the canonical form keeps `use`s at \
                 the top in dictionary order with no repeats. Delete the extra."
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
            DiagCode::InfiniteSizeType => {
                "This struct or enum contains itself by value, so no value of                  it could ever be finished.

                 Values are values in Xenith: a struct holds its fields                  directly, an enum holds its payloads directly. A type that                  reaches itself that way — directly, or through a chain of                  other types, across modules or not — would need infinite                  space.

                 The containers break the chain: `Option`, `List` and `Map`                  hold their contents indirectly, so `next: Option<Node>` is                  the ordinary way to write a recursive shape. The message                  names the cycle; put one of them on any link in it."
            }
            DiagCode::NotConstant => {
                "A `const` initializer must be a constant expression, and this \
                 one is not.\n\n\
                 A constant expression in 0.0 is a literal, or arithmetic \
                 (`+ - * / %`, unary `-`, unary `!`) over literals — nothing \
                 else. No calls, no other names, no struct or list literals, \
                 not even another `const`: the value is decided while the \
                 module is being checked, so anything that would have to *run* \
                 first cannot appear. Keeping references out also means a const \
                 cannot depend on a const, so there is no initialization order \
                 and no initialization cycle to diagnose.\n\n\
                 The arithmetic is folded during the check, which is why an \
                 overflow or a division by zero in an initializer is this error \
                 rather than a trap at every use.\n\n\
                 Where a value has to be computed, write a function returning \
                 it and call that."
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
            DiagCode::CapabilityCapture => {
                "A closure may capture only CaptureSafe values, and this one is \
                 not.\n\n\
                 CaptureSafe is decided by the type, inductively: primitives are \
                 safe; structs, enums, `List`, `Map`, `Option` and `Result` are \
                 safe when every component is; `fn(..)` types are safe. \
                 Capabilities (`Io`), types with identity or shared mutation \
                 (`Shared`), resource handles (`Task`), and type parameters with \
                 no bound to promise otherwise are not — a capture is a copy \
                 taken when the closure is created, and none of those have an \
                 honest copy.\n\n\
                 Closures are plans: effects run in the enclosing named fn's \
                 `while` loop, and the closure returns data. Keep the capability \
                 in the named fn and pass the closure the data it needs."
            }
            DiagCode::EffectInClosure => {
                "A closure body performs no effects — its effect budget is the \
                 empty set, implicitly and always.\n\n\
                 That is the first pillar of design/0014: whatever route an \
                 effect takes — a capability method called directly, a capability \
                 arriving through a parameter, a call to a named fn whose `uses` \
                 clause is non-empty, a generic that turns out effectful — the \
                 body check refuses it. There is no clause to widen, because a \
                 `fn(..)` type carries no effect set to widen it against.\n\n\
                 Closures are plans: effects run in the enclosing named fn's \
                 `while` loop, and the closure returns data. Compute in the \
                 closure; print, read and send from the named fn that holds the \
                 capability."
            }
            DiagCode::ClosureSelfReference => {
                "This closure refers to the binding its own `let` is \
                 initializing.\n\n\
                 The closure is created while the binding still has no value, and \
                 a capture is a copy taken at creation — there is nothing to \
                 copy yet. The same rule refuses a closure reaching a binding \
                 forward before its `let` runs: definite initialization, applied \
                 to captures.\n\n\
                 Recursion belongs in a named fn, which is resolved rather than \
                 captured and needs no value to exist before it is called."
            }
            DiagCode::CaptureOfVar => {
                "A closure cannot capture a `var` binding.\n\n\
                 A capture is a copy taken once, when the closure is created. A \
                 `var` exists to be reassigned, and updates after that snapshot \
                 would never be visible inside the closure — the two meanings \
                 contradict, so the capture is refused rather than silently \
                 freezing a value that looks live.\n\n\
                 Bind the current value to a `let` first and let the closure \
                 capture that: `let snapshot = total;` makes the copy explicit \
                 and the code honest about when it was taken."
            }
            DiagCode::SpawnOutsideScope => {
                "`spawn` is legal only inside a `scope { .. }` block.\n\n\
                 The scope is where every task's result is owned and consumed: \
                 each `spawn` inside it either discards a Unit result (statement \
                 form) or produces a handle that the same scope must await. \
                 Without the block there would be no place where \"every child is \
                 accounted for\" can be checked, which is the whole structure \
                 design/0015 ships.\n\n\
                 Wrap the region that spawns and awaits in `scope { .. }`:\n\n  \
                 scope {\n      let j = spawn plan(n: 21);\n      j.await\n  }\n\n\
                 A task computes a plan — effects run in the parent, after await."
            }
            DiagCode::SpawnEffectfulCallee => {
                "A spawned fn must declare no effects: its `uses` set is empty or \
                 it cannot be a child.\n\n\
                 Capabilities do not cross the task boundary in v1 — there is no \
                 transfer semantics that could say which task may act through a \
                 capability, so the boundary refuses them entirely. A child \
                 therefore computes; it never writes, reads or sends.\n\n\
                 Split the work: give the child the pure part (parse, plan, \
                 total), return the result, and perform the effect in the parent \
                 once `.await` has delivered the value. The parent already holds \
                 the capability and declares the effect.\n\n\
                 A task computes a plan — effects run in the parent, after await."
            }
            DiagCode::SpawnArgumentNotCaptureSafe => {
                "Every argument to `spawn` must have a CaptureSafe type.\n\n\
                 CaptureSafe is the same inductive rule closures use \
                 (design/0014): primitives are safe; structs, enums, `List`, \
                 `Map`, `Option` and `Result` are safe when every component is; \
                 capabilities (`Io`), `Shared`, `Task` and unbounded type \
                 parameters are not. A spawn argument is a copy handed across the \
                 task boundary, and none of those have an honest copy.\n\n\
                 Pass the child data — extract what it needs from the capability \
                 in the parent first, or restructure so the effectful step \
                 happens in the parent.\n\n\
                 A task computes a plan — effects run in the parent, after await."
            }
            DiagCode::SpawnCalleeNotFn => {
                "`spawn` takes a directly named fn: `spawn plan(n: 21)` or \
                 `spawn game.workers.plan(n: 21)`.\n\n\
                 A method call, a computed expression, a constructor or a local \
                 value cannot be spawned. The callee must be resolvable at check \
                 time, because the child's contract — empty `uses`, CaptureSafe \
                 parameters — is checked against its declaration before anything \
                 runs. Closures cannot be spawned either; that surface is \
                 deliberately frozen (design/0015 §8).\n\n\
                 Extract the work into a named fn and spawn that."
            }
            DiagCode::JoinEscape => {
                "The result of `spawn` is a task handle, and a handle does only \
                 one thing: it is awaited.\n\n\
                 Bind it with a bare `let` and consume it with `.await`, exactly \
                 once:\n\n  \
                 let j = spawn plan(n: 21);\n  let v = j.await;\n\n\
                 The handle cannot be copied to another binding, stored in a \
                 container or field, returned, passed as an argument, captured \
                 by a closure, rebound with `var`, or given a type annotation — \
                 it has no written type. Each of those would create a second \
                 route to a result that must be consumed exactly once.\n\n\
                 A task computes a plan — effects run in the parent, after await."
            }
            DiagCode::JoinAwaitedTwice => {
                "`.await` consumes the task handle; it cannot run twice.\n\n\
                 The first `.await` moves the child's result out of the handle. \
                 A second one would have nothing to take — so the checker \
                 refuses any await that could run after the handle is already \
                 consumed, including an await inside a loop when the handle was \
                 created outside it, and an await inside a `match` guard, which \
                 may run for several arms.\n\n\
                 Await once, bind the value with `let`, and use the binding as \
                 often as needed — values copy freely; handles do not."
            }
            DiagCode::JoinPartialAwait => {
                "Every control-flow path must await this handle the same number \
                 of times: exactly once.\n\n\
                 When one branch of an `if` or one `match` arm awaits and \
                 another does not, whether the result was consumed depends on a \
                 runtime condition — the exactly-once contract can no longer be \
                 checked, so the shape is refused.\n\n\
                 Either hoist the `.await` above the branch and let both sides \
                 use the value, or await in every branch. (Early exits — `?`, \
                 `return`, a trap — are exempt: a path that leaves the function \
                 discards the result, which is legal because the child was pure.)"
            }
            DiagCode::JoinUnawaited => {
                "This task's result is never consumed: the handle reaches the \
                 end of its block without `.await`.\n\n\
                 A child that returns a value computed something the program \
                 then silently drops on the normal path — almost always a \
                 mistake, and the mistake the statement-form rule (XN6009) exists \
                 to catch at the other end. Await the handle, or, if the result \
                 is genuinely unneeded, make the child return Unit and use the \
                 statement form `spawn f(..);`.\n\n\
                 Early exits are exempt: on `?`, `return` or a trap the ready \
                 result is discarded, which is safe because the child was pure."
            }
            DiagCode::SpawnStatementNotUnit => {
                "The statement form `spawn f(..);` is for children that return \
                 Unit; this callee returns a value.\n\n\
                 Writing it as a statement would silence the result at the spawn \
                 site. If the result matters, bind and await it:\n\n  \
                 let j = spawn f(..);\n  let v = j.await;\n\n\
                 If it does not, say so in the child's signature by returning \
                 Unit.\n\n\
                 A task computes a plan — effects run in the parent, after await."
            }
            DiagCode::TaskInClosure => {
                "`scope`, `spawn` and `.await` cannot appear in a closure body.\n\n\
                 A closure's effect budget is the empty set, implicitly and \
                 always (design/0014), and `Task.spawn` is an effect — so a \
                 spawn inside a closure is refused the same way `io.write` is. \
                 `scope` and `.await` follow it: task structure belongs to the \
                 named fn whose `uses` clause declares `Task.spawn`.\n\n\
                 Move the scope into the enclosing named fn; let the closure \
                 compute data for it.\n\n\
                 A task computes a plan — effects run in the parent, after await."
            }
            DiagCode::NonExhaustiveMatch => {
                "This `match` does not cover every value its scrutinee can hold.\n\n\
                 Every possible value must land on some arm. A wildcard `_` or a \
                 binding covers everything at its position; an OR pattern covers \
                 each alternative; enum variants with payloads are covered when \
                 their payload patterns are. `Int`, `Float`, `String` and `Char` \
                 cannot be enumerated by literals, so a `match` on one of them \
                 needs a `_` or binding arm.\n\n\
                 A guarded arm contributes nothing to coverage: its guard can be \
                 false at runtime, so the value must still land somewhere else.\n\n\
                 The message names a value no arm covers; add an arm for it, or a \
                 `_` arm to catch what is left."
            }
            DiagCode::InvalidModulePath => {
                "This file cannot name a module.\n\n\
                 Module path segments are `lower_snake` identifiers: a file or \
                 directory under `src/` must be named like `player.xn` or \
                 `game/`, so that its module path is spellable in source. \
                 Hyphens, spaces, uppercase letters and non-identifier \
                 characters are rejected, and so are symbolic links — a module \
                 has exactly one path.\n\n\
                 Rename the file to a `lower_snake` identifier."
            }
            DiagCode::ModuleCaseCollision => {
                "Two module paths differ only by letter case.\n\n\
                 Some file systems distinguish `Game.xn` from `game.xn` and \
                 some do not, so a project holding both means different \
                 machines would build different programs. The collision is \
                 rejected on every host for the same reason diagnostics are \
                 deterministic everywhere.\n\n\
                 Rename one of the files; module names are `lower_snake` \
                 anyway."
            }
            DiagCode::ModuleItemClash => {
                "A module path collides with a top-level item of its parent \
                 module.\n\n\
                 `game.xn` declaring an item `player` while `game/player.xn` \
                 exists would make `game.player` mean two things. Module paths \
                 and item names are exclusive under the same parent so that \
                 every dotted reference has exactly one reading.\n\n\
                 Rename the item or the file."
            }
            DiagCode::MisplacedMain => {
                "`fn main` lives in `src/main.xn` and nowhere else.\n\n\
                 The entry point is a property of the project, not of an \
                 arbitrary module — two files claiming `main` would make `run` \
                 ambiguous. A project without `src/main.xn` is a library: it \
                 checks, and `run` says there is nothing to run.\n\n\
                 Move the function, or rename it if it was never the entry \
                 point."
            }
            DiagCode::ReservedModuleRoot => {
                "`std` is a reserved module root.\n\n\
                 The prelude (Option, Result, List, Map, String, Io) needs no \
                 `use`, and the root name `std` is sealed for the language's \
                 own future modules. A local `src/std.xn` or `src/std/` would \
                 shadow that namespace differently on every compiler version.\n\n\
                 Pick another name for the directory or file."
            }
            DiagCode::NestedManifest => {
                "A manifest is nested inside another project's sources.\n\n\
                 `xenith.toml` marks a project root, and a root inside \
                 `src/` would make module paths depend on which root a build \
                 started from. One project, one manifest.\n\n\
                 Remove the inner manifest, or move the inner project out of \
                 `src/`."
            }
            DiagCode::PubApiPrivateType => {
                "A `pub` signature mentions a private type.\n\n\
                 A `pub fn`'s parameters and return, a `pub struct`'s fields \
                 and a `pub enum`'s payloads are the module's public surface; \
                 naming a private type there promises callers a value they can \
                 hold but never spell.\n\n\
                 Either mark the mentioned type `pub`, or keep the whole item \
                 private."
            }
            DiagCode::CrossModuleAssignment => {
                "This assignment writes a field across a module boundary.\n\n\
                 A `pub struct` shows its representation — construction, \
                 reads and pattern matching all work from outside — but writes \
                 do not cross the boundary, `var` field or not. Invariants \
                 live with the owning module, so mutation goes through a `pub` \
                 function it exports.\n\n\
                 Call the owning module's API, or move this code into it."
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

/// The one closure teach (design/0014 §1): every closure diagnostic that
/// teaches converges on this sentence, appended as a teach note so
/// `--diagnostic-teaching=off` strips exactly it.
pub const CLOSURE_PLAN_TEACH: &str = "closures are plans — effects run in the \
enclosing named fn's `while` loop, and the closure returns data";

/// The early-exit teach (design/0014 §3): `?`/`return`/`break` in a closure
/// body converge on this sentence instead.
pub const CLOSURE_EXIT_TEACH: &str = "closures cannot early-return; \
failure-carrying iteration belongs in a `while` loop";

/// The one task teach (design/0015 §6): every task diagnostic that teaches
/// converges on this sentence, appended as a teach note so
/// `--diagnostic-teaching=off` strips exactly it.
pub const TASK_PLAN_TEACH: &str = "a task computes a plan — effects run in the parent, after await";

/// The most items one teaching block may carry (design/0009 §3): a finite
/// contract, not a terminal-height guess.
pub const MAX_TEACH_ITEMS: usize = 6;

/// The most bytes one taught signature may occupy before it is cut with `…`.
pub const MAX_SIGNATURE_BYTES: usize = 200;

/// What kind of knowledge a [`Teach`] carries. The set is deliberately small:
/// each kind is a measured failure family (design/0009 §6 step 0), not a
/// place to be helpful in general.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TeachKind {
    /// The full signature of the callee an argument-shape diagnostic is
    /// about.
    CallSignature,
    /// The method catalogue of the receiver type an unknown-method
    /// diagnostic is about.
    AvailableMethods,
    /// The modules that export the exact name an unknown reference used —
    /// listed in canonical order, never auto-picked (design/0010 §6).
    UseCandidates,
    /// The defining module's `pub` functions that take the receiver type as
    /// an input parameter — the rewrite bridge for a method call on a
    /// module-owned type (design/0012 §1). Return-only matches are excluded.
    ModuleCall,
}

/// One taught entry: a name and its signature, rendered the way source
/// writes it.
///
/// The two optional fields belong to [`TeachKind::ModuleCall`] (design/0012
/// §1) and ride the wire only when present — a tolerant reader that predates
/// them keeps parsing the shape it knows.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TeachItem {
    pub name: String,
    pub signature: String,
    /// The parameter of a module-call candidate that takes the receiver.
    /// Absent when more than one input position fits — naming one of several
    /// would be mis-guidance.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub receiver_parameter: Option<String>,
    /// The concrete rewrite shape, `depot.locker.stow(locker: <receiver>,
    /// load: ...)`. Absent under the same ambiguity rule.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rewrite: Option<String>,
}

impl TeachItem {
    /// Build an item, cutting the signature at [`MAX_SIGNATURE_BYTES`] on a
    /// character boundary with a trailing `…`.
    pub fn new(name: impl Into<String>, signature: impl Into<String>) -> TeachItem {
        let mut signature: String = signature.into();
        if signature.len() > MAX_SIGNATURE_BYTES {
            let mut cut = MAX_SIGNATURE_BYTES;
            while !signature.is_char_boundary(cut) {
                cut -= 1;
            }
            signature.truncate(cut);
            signature.push('…');
        }
        TeachItem {
            name: name.into(),
            signature,
            receiver_parameter: None,
            rewrite: None,
        }
    }
}

/// One block of knowledge attached to a diagnostic (design/0009 §3).
///
/// Structured, never pre-rendered: the wire carries data and the renderer
/// formats it, so tools and models read the same facts the terminal shows.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Teach {
    pub kind: TeachKind,
    /// The receiver type for [`TeachKind::AvailableMethods`] and for method
    /// call signatures; empty for a free function, which has no receiver to
    /// name.
    #[serde(rename = "type")]
    pub type_name: String,
    pub items: Vec<TeachItem>,
    /// How many items exist, whether or not they all fit.
    pub total_items: usize,
    pub truncated: bool,
}

impl Teach {
    /// A single resolved callee signature.
    pub fn call_signature(type_name: impl Into<String>, item: TeachItem) -> Teach {
        Teach {
            kind: TeachKind::CallSignature,
            type_name: type_name.into(),
            items: vec![item],
            total_items: 1,
            truncated: false,
        }
    }

    /// A method catalogue in declaration order, cut to [`MAX_TEACH_ITEMS`]
    /// with the real count and the cut made explicit.
    pub fn available_methods(type_name: impl Into<String>, items: Vec<TeachItem>) -> Teach {
        let total_items = items.len();
        let truncated = total_items > MAX_TEACH_ITEMS;
        let mut items = items;
        items.truncate(MAX_TEACH_ITEMS);
        Teach {
            kind: TeachKind::AvailableMethods,
            type_name: type_name.into(),
            items,
            total_items,
            truncated,
        }
    }

    /// The modules exporting an exact-match name, in canonical path order.
    /// `type_name` carries the name that failed to resolve.
    pub fn use_candidates(name: impl Into<String>, items: Vec<TeachItem>) -> Teach {
        let total_items = items.len();
        let truncated = total_items > MAX_TEACH_ITEMS;
        let mut items = items;
        items.truncate(MAX_TEACH_ITEMS);
        Teach {
            kind: TeachKind::UseCandidates,
            type_name: name.into(),
            items,
            total_items,
            truncated,
        }
    }

    /// The module-call bridge for a method call on a module-owned type
    /// (design/0012 §1). `items` arrive ranked and already filtered to whole
    /// candidates — a signature is never cut mid-way, candidates are included
    /// or omitted whole — and `total_items` counts every candidate found,
    /// omissions included, so the cut stays structural.
    pub fn module_call(
        type_name: impl Into<String>,
        items: Vec<TeachItem>,
        total_items: usize,
    ) -> Teach {
        let mut items = items;
        items.truncate(MAX_TEACH_ITEMS);
        let truncated = total_items > items.len();
        Teach {
            kind: TeachKind::ModuleCall,
            type_name: type_name.into(),
            items,
            total_items,
            truncated,
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
    /// Knowledge attached at the point of failure (design/0009). Absent from
    /// the wire when empty, so a diagnostic without teaching keeps its
    /// pre-teaching shape byte for byte.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub teaches: Vec<Teach>,
    /// How many trailing bytes of `message` belong to teaching (design/0012
    /// §1: the module-call sentence). Never on the wire — it exists so
    /// `--diagnostic-teaching=off` can restore the pre-teaching message byte
    /// for byte, the same contract the teach blocks obey.
    #[serde(skip)]
    pub teach_note: u32,
}

impl Diagnostic {
    pub fn error(code: DiagCode, span: Span, message: impl Into<String>) -> Diagnostic {
        Diagnostic {
            code,
            severity: Severity::Error,
            span,
            message: message.into(),
            fix: None,
            teaches: Vec::new(),
            teach_note: 0,
        }
    }

    pub fn warning(code: DiagCode, span: Span, message: impl Into<String>) -> Diagnostic {
        Diagnostic {
            code,
            severity: Severity::Warning,
            span,
            message: message.into(),
            fix: None,
            teaches: Vec::new(),
            teach_note: 0,
        }
    }

    pub fn with_fix(mut self, fix: Fix) -> Diagnostic {
        self.fix = Some(fix);
        self
    }

    pub fn with_teach(mut self, teach: Teach) -> Diagnostic {
        self.teaches.push(teach);
        self
    }

    /// Append a teaching sentence to the message. It reads as ordinary
    /// message text while teaching is on; [`Diagnostic::strip_teaching`]
    /// removes exactly it, so the off-mode message is the pre-teaching one.
    pub fn with_teach_note(mut self, note: impl AsRef<str>) -> Diagnostic {
        let note = note.as_ref();
        self.message.push_str(note);
        self.teach_note = note.len() as u32;
        self
    }

    /// `--diagnostic-teaching=off`: drop the teach blocks and the teach note,
    /// and nothing else — the byte-identity contract (design/0009 §3,
    /// design/0012 §1).
    pub fn strip_teaching(&mut self) {
        self.teaches.clear();
        let keep = self.message.len().saturating_sub(self.teach_note as usize);
        self.message.truncate(keep);
        self.teach_note = 0;
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
                | DiagCode::UnshippedConstruct
                | DiagCode::ClosureAnnotation
                | DiagCode::ClosureRustForm
                | DiagCode::ClosureOutsideCall
                | DiagCode::ClosureEarlyExit
                | DiagCode::UnknownType
                | DiagCode::UnknownName
                | DiagCode::UnknownMethod
                | DiagCode::UnknownField
                | DiagCode::DuplicateDefinition
                | DiagCode::UnknownVariant
                | DiagCode::UnknownModule
                | DiagCode::PrivateItemAccess
                | DiagCode::UnusedUse
                | DiagCode::DuplicateUse
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
                | DiagCode::InfiniteSizeType
                | DiagCode::NotConstant
                | DiagCode::EffectNotPermitted
                | DiagCode::CapabilityCapture
                | DiagCode::EffectInClosure
                | DiagCode::ClosureSelfReference
                | DiagCode::CaptureOfVar
                | DiagCode::NonExhaustiveMatch
                | DiagCode::SpawnOutsideScope
                | DiagCode::SpawnEffectfulCallee
                | DiagCode::SpawnArgumentNotCaptureSafe
                | DiagCode::SpawnCalleeNotFn
                | DiagCode::JoinEscape
                | DiagCode::JoinAwaitedTwice
                | DiagCode::JoinPartialAwait
                | DiagCode::JoinUnawaited
                | DiagCode::SpawnStatementNotUnit
                | DiagCode::TaskInClosure
                | DiagCode::InvalidModulePath
                | DiagCode::ModuleCaseCollision
                | DiagCode::ModuleItemClash
                | DiagCode::MisplacedMain
                | DiagCode::ReservedModuleRoot
                | DiagCode::NestedManifest
                | DiagCode::PubApiPrivateType
                | DiagCode::CrossModuleAssignment => seen += 1,
            }
        }
        assert_eq!(seen, 64, "update DiagCode::ALL when adding a variant");
        assert_eq!(DiagCode::ALL.len(), 64);
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

    #[test]
    fn empty_teaches_are_absent_from_json_so_untaught_shapes_do_not_move() {
        let diag = Diagnostic::error(DiagCode::TypeMismatch, Span::new(0, 2), "mismatch");
        let json = serde_json::to_value(&diag).unwrap();
        assert!(json.get("teaches").is_none());
    }

    #[test]
    fn teaches_round_trip_and_spell_their_kind_the_way_0009_does() {
        let diag = Diagnostic::error(DiagCode::UnknownMethod, Span::new(0, 2), "no such method")
            .with_teach(Teach::available_methods(
                "List<Int>",
                vec![TeachItem::new("len", "len() -> Int")],
            ));
        let json = serde_json::to_value(&diag).unwrap();
        assert_eq!(json["teaches"][0]["kind"], "available_methods");
        assert_eq!(json["teaches"][0]["type"], "List<Int>");
        assert_eq!(json["teaches"][0]["items"][0]["signature"], "len() -> Int");
        assert_eq!(json["teaches"][0]["total_items"], 1);
        assert_eq!(json["teaches"][0]["truncated"], false);
        let back: Diagnostic = serde_json::from_value(json).unwrap();
        assert_eq!(back, diag);
    }

    #[test]
    fn a_signature_is_cut_at_the_byte_budget_on_a_character_boundary() {
        let item = TeachItem::new("f", "x".repeat(300));
        assert!(item.signature.ends_with('…'));
        assert!(item.signature.len() <= MAX_SIGNATURE_BYTES + '…'.len_utf8());

        // A multi-byte character straddling the cut moves the cut back
        // rather than splitting the character.
        let item = TeachItem::new("f", format!("{}ああ", "x".repeat(199)));
        assert!(item.signature.ends_with('…'), "{}", item.signature);
    }

    #[test]
    fn a_method_catalogue_is_cut_at_six_with_the_real_count_kept() {
        let items = (0..10)
            .map(|i| TeachItem::new(format!("m{i}"), format!("m{i}()")))
            .collect();
        let teach = Teach::available_methods("T", items);
        assert_eq!(teach.items.len(), MAX_TEACH_ITEMS);
        assert_eq!(teach.total_items, 10);
        assert!(teach.truncated);
    }

    #[test]
    fn a_module_call_teach_spells_its_kind_and_bridge_fields_on_the_wire() {
        let mut item = TeachItem::new(
            "depot.locker.stow",
            "depot.locker.stow(locker: depot.locker.Locker, load: Int) -> Int",
        );
        item.receiver_parameter = Some("locker".to_string());
        item.rewrite = Some("depot.locker.stow(locker: <receiver>, load: ...)".to_string());
        let diag = Diagnostic::error(DiagCode::UnknownMethod, Span::new(0, 4), "no such method")
            .with_teach(Teach::module_call("depot.locker.Locker", vec![item], 3));

        let json = serde_json::to_value(&diag).unwrap();
        assert_eq!(json["teaches"][0]["kind"], "module_call");
        assert_eq!(json["teaches"][0]["type"], "depot.locker.Locker");
        assert_eq!(json["teaches"][0]["items"][0]["name"], "depot.locker.stow");
        assert_eq!(
            json["teaches"][0]["items"][0]["receiver_parameter"],
            "locker"
        );
        assert_eq!(
            json["teaches"][0]["items"][0]["rewrite"],
            "depot.locker.stow(locker: <receiver>, load: ...)"
        );
        // Omissions are structural: one included, three found.
        assert_eq!(json["teaches"][0]["total_items"], 3);
        assert_eq!(json["teaches"][0]["truncated"], true);

        let back: Diagnostic = serde_json::from_value(json).unwrap();
        assert_eq!(back, diag);
    }

    #[test]
    fn an_ambiguous_module_call_item_omits_its_bridge_fields_from_the_wire() {
        let diag = Diagnostic::error(DiagCode::UnknownMethod, Span::new(0, 4), "no such method")
            .with_teach(Teach::module_call(
                "depot.locker.Locker",
                vec![TeachItem::new(
                    "depot.locker.transfer",
                    "depot.locker.transfer(from: depot.locker.Locker, to: depot.locker.Locker) -> Int",
                )],
                1,
            ));
        let json = serde_json::to_value(&diag).unwrap();
        let item = &json["teaches"][0]["items"][0];
        assert!(item.get("receiver_parameter").is_none(), "{item}");
        assert!(item.get("rewrite").is_none(), "{item}");
        assert_eq!(json["teaches"][0]["truncated"], false);
    }

    #[test]
    fn strip_teaching_removes_the_teach_note_and_blocks_and_nothing_else() {
        let base = "`depot.locker.Locker` has no method named `stow`";
        let mut diag = Diagnostic::error(DiagCode::UnknownMethod, Span::new(0, 4), base)
            .with_teach_note("; module functions are called as `depot.locker.stow(...)`")
            .with_teach(Teach::module_call("depot.locker.Locker", Vec::new(), 0));
        assert!(diag.message.ends_with("stow(...)`"));

        let untaught = serde_json::to_value(Diagnostic::error(
            DiagCode::UnknownMethod,
            Span::new(0, 4),
            base,
        ))
        .unwrap();
        diag.strip_teaching();
        assert_eq!(serde_json::to_value(&diag).unwrap(), untaught);
    }

    #[test]
    fn the_teach_note_length_never_reaches_the_wire() {
        let diag = Diagnostic::error(DiagCode::UnknownMethod, Span::new(0, 4), "m")
            .with_teach_note("; note");
        let json = serde_json::to_value(&diag).unwrap();
        assert!(json.get("teach_note").is_none(), "{json}");
        assert_eq!(json["message"], "m; note");
    }
}
