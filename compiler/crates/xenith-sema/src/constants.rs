//! What a `const` initializer may say, and what it folds to.
//!
//! A constant expression is a literal, or arithmetic over literals — nothing
//! else. Not a call, not another name, **not another `const`**. The last
//! exclusion is the load-bearing one: with no references there is no
//! initialization order, so the initialization cycle design/0010 §5 reserves
//! a diagnostic for cannot arise, and the const surface ships without owing
//! one. Widening the grammar later is additive; it is the widening that has
//! to answer the ordering question, not this.
//!
//! The integer arithmetic is folded here rather than left to the interpreter
//! so that an overflow or a division by zero in an initializer is a refusal
//! at check time — the one place where trapping arithmetic (design/0003) can
//! be turned into a diagnostic instead of a trap at every use. The other
//! literal kinds contribute only their type: nothing folds *across* a string
//! or a char, so their values stay where every other literal's value lives,
//! in the expression the interpreter reads.

use xenith_diag::{DiagCode, Diagnostic, Span};
use xenith_syntax::ast;

use crate::ty::Type;

/// What a constant initializer folded to.
pub enum Folded {
    Int(i64),
    Float(f64),
    /// A literal that participates in no arithmetic: `Bool`, `String`,
    /// `Char`, `Unit`. Carries its type and nothing else.
    Opaque(Type),
}

impl Folded {
    pub fn ty(&self) -> Type {
        match self {
            Folded::Int(_) => Type::Int,
            Folded::Float(_) => Type::Float,
            Folded::Opaque(ty) => ty.clone(),
        }
    }

    /// The source spelling of what folded. Only scalars reach here, so no
    /// definition table is needed to name them.
    fn shown(&self) -> &'static str {
        match self.ty() {
            Type::Int => "Int",
            Type::Float => "Float",
            Type::Bool => "Bool",
            Type::Str => "String",
            Type::Char => "Char",
            _ => "Unit",
        }
    }
}

/// Fold a `const` initializer, or say why it is not one.
///
/// The returned diagnostic is the only one this expression produces: an
/// initializer the fold refused has no meaningful type, and reporting a
/// second failure derived from the first is the cascade the checker's poison
/// discipline exists to prevent (design/0006 §2).
pub fn fold(expr: &ast::Expr) -> Result<Folded, Diagnostic> {
    match &expr.kind {
        ast::ExprKind::Int(text) => digits(text)
            .parse::<i64>()
            .map(Folded::Int)
            .map_err(|_| refuse(expr.span, "this integer literal does not fit in 64 bits")),
        ast::ExprKind::Float(text) => digits(text)
            .parse::<f64>()
            .map(Folded::Float)
            .map_err(|_| refuse(expr.span, "this float literal does not parse")),
        ast::ExprKind::Bool(_) => Ok(Folded::Opaque(Type::Bool)),
        ast::ExprKind::Str(_) => Ok(Folded::Opaque(Type::Str)),
        ast::ExprKind::Char(_) => Ok(Folded::Opaque(Type::Char)),
        ast::ExprKind::Unit => Ok(Folded::Opaque(Type::Unit)),

        ast::ExprKind::Unary { op, operand } => unary(*op, operand, expr.span),
        ast::ExprKind::Binary { op, lhs, rhs } => binary(*op, lhs, rhs, expr.span),

        // A hole in an initializer is still a gap, not a value — and a
        // `const` has no expected type to hand it beyond its annotation, so
        // there is nothing a goal here could teach that the annotation does
        // not already say.
        ast::ExprKind::Hole { .. } => Err(refuse(
            expr.span,
            "a hole cannot stand in for a `const` value; a constant must be \
             decided while the module is checked",
        )),

        _ => Err(refuse(
            expr.span,
            "only a literal, or arithmetic over literals, may initialize a \
             `const`; write a fn returning the value and call that",
        )),
    }
}

fn unary(op: ast::UnaryOp, operand: &ast::Expr, span: Span) -> Result<Folded, Diagnostic> {
    let value = fold(operand)?;
    match (op, value) {
        (ast::UnaryOp::Neg, Folded::Int(n)) => n
            .checked_neg()
            .map(Folded::Int)
            .ok_or_else(|| refuse(span, "integer overflow negating `-9223372036854775808`")),
        (ast::UnaryOp::Neg, Folded::Float(f)) => Ok(Folded::Float(-f)),
        (ast::UnaryOp::Not, Folded::Opaque(Type::Bool)) => Ok(Folded::Opaque(Type::Bool)),
        (ast::UnaryOp::Neg, other) => Err(mismatch(
            span,
            format!("`-` needs `Int` or `Float`, found `{}`", other.shown()),
        )),
        (ast::UnaryOp::Not, other) => Err(mismatch(
            span,
            format!("`!` needs `Bool`, found `{}`", other.shown()),
        )),
    }
}

fn binary(
    op: ast::BinaryOp,
    lhs: &ast::Expr,
    rhs: &ast::Expr,
    span: Span,
) -> Result<Folded, Diagnostic> {
    use ast::BinaryOp as B;
    if !matches!(op, B::Add | B::Sub | B::Mul | B::Div | B::Rem) {
        return Err(refuse(
            span,
            format!(
                "`{}` is not folded in a `const` initializer; the constant \
                 operators are `+ - * / %` and unary `-` / `!`",
                op.symbol()
            ),
        ));
    }
    let left = fold(lhs)?;
    let right = fold(rhs)?;
    match (left, right) {
        // Trapping integer arithmetic (design/0003), evaluated early: what
        // would trap at every use is refused once, here.
        (Folded::Int(a), Folded::Int(b)) => {
            let folded = match op {
                B::Add => a.checked_add(b),
                B::Sub => a.checked_sub(b),
                B::Mul => a.checked_mul(b),
                B::Div if b == 0 => return Err(refuse(span, "division by zero")),
                B::Div => a.checked_div(b),
                B::Rem if b == 0 => return Err(refuse(span, "remainder by zero")),
                B::Rem => a.checked_rem(b),
                _ => unreachable!("the operator set was checked above"),
            };
            folded
                .map(Folded::Int)
                .ok_or_else(|| refuse(span, format!("integer overflow in `{}`", op.symbol())))
        }
        // IEEE float arithmetic, exactly as the interpreter would do it.
        (Folded::Float(a), Folded::Float(b)) => Ok(Folded::Float(match op {
            B::Add => a + b,
            B::Sub => a - b,
            B::Mul => a * b,
            B::Div => a / b,
            B::Rem => a % b,
            _ => unreachable!("the operator set was checked above"),
        })),
        (left, right) => Err(mismatch(
            span,
            format!(
                "`{}` needs two `Int`s or two `Float`s, found `{}` and `{}`",
                op.symbol(),
                left.shown(),
                right.shown()
            ),
        )),
    }
}

/// `1_000` -> `1000`: separators carry no meaning (design/0003 §3).
fn digits(text: &str) -> String {
    text.chars().filter(|c| *c != '_').collect()
}

fn refuse(span: Span, message: impl Into<String>) -> Diagnostic {
    Diagnostic::error(DiagCode::NotConstant, span, message)
}

fn mismatch(span: Span, message: impl Into<String>) -> Diagnostic {
    Diagnostic::error(DiagCode::TypeMismatch, span, message)
}
