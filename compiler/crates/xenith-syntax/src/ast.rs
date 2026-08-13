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

/// A generic parameter: `T`, or `T: Eq + Hash`.
///
/// Bounds name sealed type properties (`Eq`, `Ord`, `Hash`, `Copy`, `Text`).
/// The parser accepts any identifier here; the checker validates against the
/// closed set, so that an unknown property is reported with its name rather
/// than as a syntax error. See `design/0006-type-checking.md` §3.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GenericParam {
    pub name: Ident,
    pub bounds: Vec<Ident>,
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
    /// `pub const` — visible across module boundaries (design/0010 §4).
    pub is_pub: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FnItem {
    pub name: Ident,
    /// `pub fn` — callable across module boundaries (design/0010 §4).
    pub is_pub: bool,
    pub is_async: bool,
    pub generics: Vec<GenericParam>,
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
    /// `pub struct` — constructible and readable across module boundaries;
    /// field assignment still stops at the boundary (design/0010 §4).
    pub is_pub: bool,
    pub generics: Vec<GenericParam>,
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
    /// `pub enum` — every variant constructible and matchable across module
    /// boundaries (design/0010 §4).
    pub is_pub: bool,
    pub generics: Vec<GenericParam>,
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
    /// `fn(Int) -> Int`, `fn(acc: Int, x: Int) -> Int`.
    ///
    /// Parameter names are documentation (design/0014 §1: canonical when a
    /// fn type takes two or more parameters). The `uses` clause is parsed
    /// for recovery only — a fn type's effect set is always empty, so the
    /// shipped syntax has nowhere to write one.
    Fn {
        params: Vec<FnTypeParam>,
        ret: Box<Type>,
        effects: Option<EffectSet>,
    },
    /// `??` or `??name` in type position — ask the compiler what fits.
    Hole {
        name: Option<String>,
    },
    Error,
}

/// One parameter of a `fn(..)` type: an optional documentation name and the
/// type itself.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FnTypeParam {
    pub name: Option<Ident>,
    pub ty: Type,
    pub span: Span,
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

    /// `scope { .. }` — the task region (design/0015). A block expression;
    /// `scope` is contextual, recognised only when a `{` follows, so the
    /// word stays an ordinary identifier everywhere else.
    Scope(Block),
    /// `spawn path(args)` — run a pure child to completion (design/0015).
    /// The callee is a path, resolved like an ordinary call's; `spawn` is
    /// contextual, recognised only when an identifier follows.
    Spawn {
        path: Path,
        args: Vec<Arg>,
    },

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

    /// `[1, 2, 3]` and `[]`. The empty form needs an expected type to name
    /// its element — same policy as holes (design/0006 §1-1).
    ListLit(Vec<Expr>),

    /// `Player { name: n, score: 0 }`
    StructLit {
        path: Path,
        fields: Vec<FieldInit>,
    },

    /// `|x| expr` / `|acc, x| expr` / `|| expr` / `|_| expr`.
    ///
    /// Parameters are bare names — there is no annotation syntax; the types
    /// come from the `fn(..)` parameter the closure is passed to. A body of
    /// the form `{ expr }` parses as the expression itself, the same way
    /// parentheses vanish, so `|x| { x + 1 }` and `|x| x + 1` are one tree
    /// (design/0014 §3: one canonical spelling).
    Lambda {
        params: Vec<LambdaParam>,
        body: Box<Expr>,
    },

    Error,
}

/// One closure parameter: a plain name, or `_` (spelled with the name `"_"`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LambdaParam {
    pub name: Ident,
    pub span: Span,
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

// ------------------------------------------------------- structural equality

/// Set every span in the tree to [`Span::EMPTY`].
///
/// Two programs that differ only in layout parse to trees that differ only in
/// spans, so clearing the spans turns the derived `PartialEq` into structural
/// equality. The formatter relies on this to prove it has not changed the
/// meaning of the code it rewrote — see `design/0005-canonical-formatting.md`.
///
/// Documentation spans are cleared too: comments are checked separately, by
/// text, because their positions legitimately move during formatting.
pub fn normalize_spans(module: &mut Module) {
    module.span = Span::EMPTY;
    for item in &mut module.items {
        normalize_item(item);
    }
}

fn normalize_item(item: &mut Item) {
    item.span = Span::EMPTY;
    item.docs.clear();
    match &mut item.kind {
        ItemKind::Use(u) => normalize_path(&mut u.path),
        ItemKind::Const(c) => {
            normalize_ident(&mut c.name);
            normalize_type(&mut c.ty);
            normalize_expr(&mut c.value);
        }
        ItemKind::Fn(f) => {
            normalize_ident(&mut f.name);
            normalize_generics(&mut f.generics);
            for param in &mut f.params {
                param.span = Span::EMPTY;
                normalize_ident(&mut param.name);
                normalize_type(&mut param.ty);
            }
            if let Some(ty) = &mut f.return_type {
                normalize_type(ty);
            }
            if let Some(effects) = &mut f.effects {
                normalize_effects(effects);
            }
            if let Some(body) = &mut f.body {
                normalize_block(body);
            }
        }
        ItemKind::Struct(s) => {
            normalize_ident(&mut s.name);
            normalize_generics(&mut s.generics);
            for field in &mut s.fields {
                field.span = Span::EMPTY;
                field.docs.clear();
                normalize_ident(&mut field.name);
                normalize_type(&mut field.ty);
            }
        }
        ItemKind::Enum(e) => {
            normalize_ident(&mut e.name);
            normalize_generics(&mut e.generics);
            for variant in &mut e.variants {
                variant.span = Span::EMPTY;
                variant.docs.clear();
                normalize_ident(&mut variant.name);
                variant.payload.iter_mut().for_each(normalize_type);
            }
        }
        ItemKind::Error => {}
    }
}

fn normalize_ident(ident: &mut Ident) {
    ident.span = Span::EMPTY;
}

fn normalize_generics(generics: &mut [GenericParam]) {
    for param in generics {
        param.span = Span::EMPTY;
        normalize_ident(&mut param.name);
        param.bounds.iter_mut().for_each(normalize_ident);
    }
}

fn normalize_path(path: &mut Path) {
    path.span = Span::EMPTY;
    path.segments.iter_mut().for_each(normalize_ident);
}

fn normalize_effects(effects: &mut EffectSet) {
    effects.span = Span::EMPTY;
    effects.effects.iter_mut().for_each(normalize_path);
}

fn normalize_type(ty: &mut Type) {
    ty.span = Span::EMPTY;
    match &mut ty.kind {
        TypeKind::Named { path, args } => {
            normalize_path(path);
            args.iter_mut().for_each(normalize_type);
        }
        TypeKind::Fn {
            params,
            ret,
            effects,
        } => {
            for param in params {
                param.span = Span::EMPTY;
                if let Some(name) = &mut param.name {
                    normalize_ident(name);
                }
                normalize_type(&mut param.ty);
            }
            normalize_type(ret);
            if let Some(effects) = effects {
                normalize_effects(effects);
            }
        }
        TypeKind::Unit | TypeKind::Hole { .. } | TypeKind::Error => {}
    }
}

fn normalize_block(block: &mut Block) {
    block.span = Span::EMPTY;
    block.stmts.iter_mut().for_each(normalize_stmt);
    if let Some(tail) = &mut block.tail {
        normalize_expr(tail);
    }
}

fn normalize_stmt(stmt: &mut Stmt) {
    stmt.span = Span::EMPTY;
    match &mut stmt.kind {
        StmtKind::Let {
            pattern, ty, init, ..
        } => {
            normalize_pattern(pattern);
            if let Some(ty) = ty {
                normalize_type(ty);
            }
            normalize_expr(init);
        }
        StmtKind::Expr(expr) => normalize_expr(expr),
        StmtKind::Return(value) => {
            if let Some(value) = value {
                normalize_expr(value);
            }
        }
        StmtKind::While { cond, body } => {
            normalize_expr(cond);
            normalize_block(body);
        }
        StmtKind::For {
            pattern,
            iter,
            body,
        } => {
            normalize_pattern(pattern);
            normalize_expr(iter);
            normalize_block(body);
        }
        StmtKind::Break | StmtKind::Continue | StmtKind::Error => {}
    }
}

fn normalize_expr(expr: &mut Expr) {
    expr.span = Span::EMPTY;
    match &mut expr.kind {
        ExprKind::Path(path) => normalize_path(path),
        ExprKind::Unary { operand, .. } => normalize_expr(operand),
        ExprKind::Binary { lhs, rhs, .. } => {
            normalize_expr(lhs);
            normalize_expr(rhs);
        }
        ExprKind::Assign { target, value, .. } => {
            normalize_expr(target);
            normalize_expr(value);
        }
        ExprKind::Call { callee, args } => {
            normalize_expr(callee);
            args.iter_mut().for_each(normalize_arg);
        }
        ExprKind::MethodCall {
            receiver,
            method,
            args,
        } => {
            normalize_expr(receiver);
            normalize_ident(method);
            args.iter_mut().for_each(normalize_arg);
        }
        ExprKind::Field { receiver, name } => {
            normalize_expr(receiver);
            normalize_ident(name);
        }
        ExprKind::Await(inner) | ExprKind::Try(inner) => normalize_expr(inner),
        ExprKind::Scope(block) => normalize_block(block),
        ExprKind::Spawn { path, args } => {
            normalize_path(path);
            args.iter_mut().for_each(normalize_arg);
        }
        ExprKind::If {
            cond,
            then_block,
            else_branch,
        } => {
            normalize_expr(cond);
            normalize_block(then_block);
            if let Some(branch) = else_branch {
                normalize_expr(branch);
            }
        }
        ExprKind::Match { scrutinee, arms } => {
            normalize_expr(scrutinee);
            for arm in arms {
                arm.span = Span::EMPTY;
                normalize_pattern(&mut arm.pattern);
                if let Some(guard) = &mut arm.guard {
                    normalize_expr(guard);
                }
                normalize_expr(&mut arm.body);
            }
        }
        ExprKind::Block(block) => normalize_block(block),
        ExprKind::ListLit(elements) => elements.iter_mut().for_each(normalize_expr),
        ExprKind::StructLit { path, fields } => {
            normalize_path(path);
            for field in fields {
                field.span = Span::EMPTY;
                normalize_ident(&mut field.name);
                normalize_expr(&mut field.value);
            }
        }
        ExprKind::Lambda { params, body } => {
            for param in params {
                param.span = Span::EMPTY;
                normalize_ident(&mut param.name);
            }
            normalize_expr(body);
        }
        ExprKind::Int(_)
        | ExprKind::Float(_)
        | ExprKind::Str(_)
        | ExprKind::Char(_)
        | ExprKind::Bool(_)
        | ExprKind::Unit
        | ExprKind::Hole { .. }
        | ExprKind::Error => {}
    }
}

fn normalize_arg(arg: &mut Arg) {
    arg.span = Span::EMPTY;
    if let Some(name) = &mut arg.name {
        normalize_ident(name);
    }
    normalize_expr(&mut arg.value);
}

fn normalize_pattern(pattern: &mut Pattern) {
    pattern.span = Span::EMPTY;
    match &mut pattern.kind {
        PatternKind::Binding(ident) => normalize_ident(ident),
        PatternKind::Literal(expr) => normalize_expr(expr),
        PatternKind::Path(path) => normalize_path(path),
        PatternKind::Variant { path, elements } => {
            normalize_path(path);
            elements.iter_mut().for_each(normalize_pattern);
        }
        PatternKind::Struct { path, fields } => {
            normalize_path(path);
            for field in fields {
                field.span = Span::EMPTY;
                normalize_ident(&mut field.name);
                if let Some(pattern) = &mut field.pattern {
                    normalize_pattern(pattern);
                }
            }
        }
        PatternKind::Or(alternatives) => alternatives.iter_mut().for_each(normalize_pattern),
        PatternKind::Wildcard | PatternKind::Error => {}
    }
}
