//! The Xenith syntax tree.
//!
//! Three properties are deliberate.
//!
//! **Every node carries a span.** Diagnostics, holes and structural edits all
//! address source by range, so a node without a span is a node no tool can
//! point at.
//!
//! **Holes are ordinary nodes, not errors.** [`ExprKind::Hole`] and
//! [`TypeKind::Hole`] are legal in a well-formed tree. A partial program is a
//! normal state — see `design/0002-design-review.md`.
//!
//! **Errors are also ordinary nodes.** `Error` variants let the parser produce
//! a complete tree from broken input, so later stages still see structure
//! around the damage instead of nothing at all.

use xenith_diag::Span;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Ident {
    pub name: String,
    pub span: Span,
}

impl Ident {
    pub fn new(name: impl Into<String>, span: Span) -> Ident {
        Ident {
            name: name.into(),
            span,
        }
    }
}

/// A dotted path: `io`, `Rank.Gold`, `std.collections.Map`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Path {
    pub segments: Vec<Ident>,
    pub span: Span,
}

// ------------------------------------------------------------------- module

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Module {
    pub items: Vec<Item>,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Item {
    pub kind: ItemKind,
    /// Spans of the `///` lines attached to this item, in source order.
    pub docs: Vec<Span>,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ItemKind {
    Use(UseItem),
    Const(ConstItem),
    Fn(FnItem),
    Struct(StructItem),
    Enum(EnumItem),
    /// Recovery: the parser could not identify a declaration here.
    Error,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UseItem {
    pub path: Path,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConstItem {
    pub name: Ident,
    pub ty: Type,
    pub value: Expr,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FnItem {
    pub name: Ident,
    pub is_async: bool,
    pub generics: Vec<Ident>,
    pub params: Vec<Param>,
    /// Absent means the function returns `unit`.
    pub return_type: Option<Type>,
    /// The `uses { .. }` clause. Absent means the empty effect set, which is
    /// the strongest claim a signature can make: the function performs no
    /// effects at all.
    pub effects: Option<EffectSet>,
    /// Absent only in recovery, when the body could not be parsed.
    pub body: Option<Block>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Param {
    pub name: Ident,
    pub ty: Type,
    pub span: Span,
}

/// A closed set of effects: `uses {Fs.read, Net.send}`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EffectSet {
    pub effects: Vec<Path>,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StructItem {
    pub name: Ident,
    pub generics: Vec<Ident>,
    pub fields: Vec<FieldDef>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FieldDef {
    pub name: Ident,
    pub ty: Type,
    /// `var name: T` — fields are immutable unless marked.
    pub mutable: bool,
    pub docs: Vec<Span>,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EnumItem {
    pub name: Ident,
    pub generics: Vec<Ident>,
    pub variants: Vec<VariantDef>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VariantDef {
    pub name: Ident,
    /// Payload types: `NotFound(Int)`. Empty for a bare variant.
    pub payload: Vec<Type>,
    pub docs: Vec<Span>,
    pub span: Span,
}

// --------------------------------------------------------------------- types

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Type {
    pub kind: TypeKind,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TypeKind {
    /// `Int`, `Result<T, E>`, `std.io.Handle`
    Named {
        path: Path,
        args: Vec<Type>,
    },
    /// `()`
    Unit,
    /// `fn(Int, Int) -> Int uses {Io.write}`
    Fn {
        params: Vec<Type>,
        ret: Box<Type>,
        effects: Option<EffectSet>,
    },
    /// `??` or `??name` in type position — ask the compiler what fits.
    Hole {
        name: Option<String>,
    },
    Error,
}

// ---------------------------------------------------------------- statements

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Block {
    pub stmts: Vec<Stmt>,
    /// The final expression, written without a trailing `;`. This is the
    /// block's value; `None` means the block evaluates to `unit`.
    pub tail: Option<Box<Expr>>,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Stmt {
    pub kind: StmtKind,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StmtKind {
    /// `let x: T = e;` and `var x: T = e;`
    Let {
        pattern: Pattern,
        ty: Option<Type>,
        init: Expr,
        /// `var` rather than `let`. Xenith spells mutability with a distinct
        /// keyword rather than a modifier, because a modifier is something you
        /// can forget to write.
        mutable: bool,
    },
    /// An expression evaluated for its effects, terminated by `;`.
    Expr(Expr),
    /// `return e;` — the operand is required by the grammar. A bare `return`
    /// is parsed here as `None` only so that the error can be reported with a
    /// fix rather than by refusing to parse.
    Return(Option<Expr>),
    Break,
    Continue,
    While {
        cond: Expr,
        body: Block,
    },
    For {
        pattern: Pattern,
        iter: Expr,
        body: Block,
    },
    Error,
}

// --------------------------------------------------------------- expressions

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Expr {
    pub kind: ExprKind,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ExprKind {
    Int(String),
    Float(String),
    Str(String),
    Char(String),
    Bool(bool),
    Unit,

    /// A name or dotted path used as a value: `total`, `Rank.Gold`.
    Path(Path),

    /// `??` or `??name`. Legal, not an error.
    Hole {
        name: Option<String>,
    },

    Unary {
        op: UnaryOp,
        operand: Box<Expr>,
    },
    Binary {
        op: BinaryOp,
        lhs: Box<Expr>,
        rhs: Box<Expr>,
    },
    /// `target = value` and the compound forms.
    Assign {
        target: Box<Expr>,
        /// `None` for `=`; `Some(op)` for `+=` and friends.
        op: Option<BinaryOp>,
        value: Box<Expr>,
    },

    Call {
        callee: Box<Expr>,
        args: Vec<Arg>,
    },
    MethodCall {
        receiver: Box<Expr>,
        method: Ident,
        args: Vec<Arg>,
    },
    Field {
        receiver: Box<Expr>,
        name: Ident,
    },

    /// `receiver.await`
    Await(Box<Expr>),
    /// `expr?` — return early on the error case.
    Try(Box<Expr>),

    If {
        cond: Box<Expr>,
        then_block: Block,
        /// Another `if` (for `else if`) or a block expression.
        else_branch: Option<Box<Expr>>,
    },
    Match {
        scrutinee: Box<Expr>,
        arms: Vec<MatchArm>,
    },
    Block(Block),

    /// `Player { name: n, score: 0 }`
    StructLit {
        path: Path,
        fields: Vec<FieldInit>,
    },

    /// `move || { .. }` / `async move |x: Int| { .. }`
    Lambda {
        params: Vec<Param>,
        is_move: bool,
        is_async: bool,
        body: Box<Expr>,
    },

    Error,
}

/// One argument at a call site.
///
/// Xenith requires named arguments once a call takes two or more, which turns
/// argument-order mistakes into a compile error rather than a silent bug. The
/// parser accepts both forms and records which was used; the rule is enforced
/// later, where the callee's parameter names are known and the diagnostic can
/// carry a real fix.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Arg {
    pub name: Option<Ident>,
    pub value: Expr,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FieldInit {
    pub name: Ident,
    pub value: Expr,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MatchArm {
    pub pattern: Pattern,
    /// `if cond` after the pattern.
    pub guard: Option<Expr>,
    pub body: Expr,
    pub span: Span,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnaryOp {
    /// `-`
    Neg,
    /// `!`
    Not,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BinaryOp {
    Add,
    Sub,
    Mul,
    Div,
    Rem,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    And,
    Or,
    BitAnd,
    BitOr,
    BitXor,
    Shl,
    Shr,
    /// `is` — identity, valid only on `Shared` and handles. Distinct from `==`,
    /// which is structural equality.
    Identity,
}

impl BinaryOp {
    pub fn symbol(self) -> &'static str {
        match self {
            BinaryOp::Add => "+",
            BinaryOp::Sub => "-",
            BinaryOp::Mul => "*",
            BinaryOp::Div => "/",
            BinaryOp::Rem => "%",
            BinaryOp::Eq => "==",
            BinaryOp::Ne => "!=",
            BinaryOp::Lt => "<",
            BinaryOp::Le => "<=",
            BinaryOp::Gt => ">",
            BinaryOp::Ge => ">=",
            BinaryOp::And => "&&",
            BinaryOp::Or => "||",
            BinaryOp::BitAnd => "&",
            BinaryOp::BitOr => "|",
            BinaryOp::BitXor => "^",
            BinaryOp::Shl => "<<",
            BinaryOp::Shr => ">>",
            BinaryOp::Identity => "is",
        }
    }
}

// ------------------------------------------------------------------ patterns

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pattern {
    pub kind: PatternKind,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PatternKind {
    /// `_`
    Wildcard,
    /// `total`
    Binding(Ident),
    /// `0`, `"ok"`, `true`
    Literal(Expr),
    /// `Rank.Gold`
    Path(Path),
    /// `Ok(value)`, `NotFound(id)`
    Variant {
        path: Path,
        elements: Vec<Pattern>,
    },
    /// `Player { name, score }`
    Struct {
        path: Path,
        fields: Vec<FieldPattern>,
    },
    /// `A | B`
    Or(Vec<Pattern>),
    Error,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FieldPattern {
    pub name: Ident,
    /// `None` is shorthand: `{ score }` binds `score` to itself.
    pub pattern: Option<Pattern>,
    pub span: Span,
}
