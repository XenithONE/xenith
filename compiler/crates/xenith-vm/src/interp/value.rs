use std::sync::Arc;

use xenith_diag::Span;
use xenith_sema::ty::DefId;
use xenith_syntax::ast;

use super::{Eval, trap};

// ------------------------------------------------------------------- values

/// A runtime value.
///
/// Every aggregate arm holds its payload behind an [`Arc`], and every write
/// path goes through [`Arc::make_mut`] — copy-on-write (design/0017 §4).
/// This is the "implementation may share storage under the hood" clause of
/// spec/04 §1 taken up: a copy is O(1) until somebody writes, and the write
/// uniquifies **the whole path** it walks, so reading a value out of a
/// container still yields an independent value (D1). An implementation that
/// uniquified only the outermost node and then wrote through a shared inner
/// node would be a bug, not an optimisation.
///
/// The arms are also all `Send`, statically ([`VALUE_IS_SEND`]): design/0017
/// §3 runs children on real threads, and the type system — not a comment —
/// is what keeps `Rc` and interior mutability out.
#[derive(Clone, Debug)]
pub enum Value<'a> {
    Int(i64),
    Float(f64),
    Bool(bool),
    Str(Arc<String>),
    Char(char),
    Unit,
    /// A `List<T>` value. Reads copy (design/0007 D1); only `push`, `pop`
    /// and `replace` write through the receiver in place.
    List(Arc<Vec<Value<'a>>>),
    /// A `Map<K, V>` value in insertion order — the order is normative
    /// (design/0007 §3), so pairs beat a hash table at this scale.
    Map(Arc<Vec<(Value<'a>, Value<'a>)>>),
    /// A value of the opaque prelude `Error` type. The message exists for
    /// debug rendering; nothing in the language reads it back out.
    ErrorValue(Arc<String>),
    Struct {
        def: DefId,
        /// Field values in declaration order.
        fields: Arc<Vec<Value<'a>>>,
    },
    Enum {
        def: DefId,
        variant: usize,
        payload: Arc<Vec<Value<'a>>>,
    },
    /// A function value: a lambda, a named function, or reference thereto.
    Fn {
        params: Arc<Vec<String>>,
        body: Body<'a>,
        captured: Arc<Vec<(String, Value<'a>)>>,
        is_async: bool,
        /// Index of the module whose bare names the body resolves against.
        home: usize,
    },
    /// A variant constructor used as a value: `ScoreError.NotFound`.
    VariantCtor {
        def: DefId,
        variant: usize,
        arity: usize,
    },
    /// A capability handed to `main`. The name is the prelude type ("Io").
    Capability(&'static str),
    /// The result of calling an `async fn`, and the handle the sequential
    /// executor hands back from `spawn`: the body has already run, and
    /// `.await` unwraps.
    Task(Arc<Value<'a>>),
    /// The handle the parallel executor hands back from `spawn`: the
    /// child's position in the run's spawn order. `.await` commits every
    /// outcome up to and including it (design/0017 §3).
    Pending {
        index: usize,
    },
}

/// `Value` crosses thread boundaries in the parallel executor (design/0017
/// §3), so `Send` is a compile-time obligation, not a review note. An `Rc`
/// or a `Cell` smuggled into any arm breaks this line.
const VALUE_IS_SEND: () = {
    const fn assert_send<T: Send>() {}
    assert_send::<Value<'static>>();
};
const _: () = VALUE_IS_SEND;

impl<'a> Value<'a> {
    /// Wrap an owned `String` as a value. The `Arc` is the sharing, not the
    /// semantics: `String` has no mutating method in the language.
    pub(super) fn str(text: impl Into<String>) -> Value<'a> {
        Value::Str(Arc::new(text.into()))
    }

    pub(super) fn list(items: Vec<Value<'a>>) -> Value<'a> {
        Value::List(Arc::new(items))
    }

    pub(super) fn map(entries: Vec<(Value<'a>, Value<'a>)>) -> Value<'a> {
        Value::Map(Arc::new(entries))
    }

    pub(super) fn error_value(message: impl Into<String>) -> Value<'a> {
        Value::ErrorValue(Arc::new(message.into()))
    }

    pub(super) fn structure(def: DefId, fields: Vec<Value<'a>>) -> Value<'a> {
        Value::Struct {
            def,
            fields: Arc::new(fields),
        }
    }

    pub(super) fn enumeration(def: DefId, variant: usize, payload: Vec<Value<'a>>) -> Value<'a> {
        Value::Enum {
            def,
            variant,
            payload: Arc::new(payload),
        }
    }
}

/// Take the owned payload out of an `Arc`, copying only when it is shared.
/// The copy is what keeps D1 honest when a value is consumed by move.
pub(super) fn owned<T: Clone>(shared: Arc<T>) -> T {
    Arc::try_unwrap(shared).unwrap_or_else(|shared| (*shared).clone())
}

#[derive(Clone, Debug)]
pub enum Body<'a> {
    Block(&'a ast::Block),
    Expr(&'a ast::Expr),
}

/// Trapping integer arithmetic, IEEE float arithmetic.
pub(super) fn arith<'a>(
    op: ast::BinaryOp,
    left: &Value<'a>,
    right: &Value<'a>,
    span: Span,
) -> Eval<'a, Value<'a>> {
    use ast::BinaryOp as B;
    match (left, right) {
        (Value::Int(a), Value::Int(b)) => {
            let result = match op {
                B::Add => a.checked_add(*b),
                B::Sub => a.checked_sub(*b),
                B::Mul => a.checked_mul(*b),
                B::Div => {
                    if *b == 0 {
                        return trap(span, "division by zero");
                    }
                    a.checked_div(*b)
                }
                B::Rem => {
                    if *b == 0 {
                        return trap(span, "remainder by zero");
                    }
                    a.checked_rem(*b)
                }
                _ => return trap(span, "not an arithmetic operator"),
            };
            match result {
                Some(value) => Ok(Value::Int(value)),
                // The kernel's rule: overflow traps, deterministically,
                // rather than wrapping (design/0003).
                None => trap(span, format!("integer overflow in `{}`", op.symbol())),
            }
        }
        (Value::Float(a), Value::Float(b)) => {
            let result = match op {
                B::Add => a + b,
                B::Sub => a - b,
                B::Mul => a * b,
                B::Div => a / b,
                B::Rem => a % b,
                _ => return trap(span, "not an arithmetic operator"),
            };
            Ok(Value::Float(result))
        }
        _ => trap(span, "arithmetic needs two Ints or two Floats"),
    }
}

/// Structural equality — the runtime twin of the sealed `Eq` property.
pub(super) fn values_equal<'a>(a: &Value<'a>, b: &Value<'a>, span: Span) -> Eval<'a, bool> {
    match (a, b) {
        (Value::Int(x), Value::Int(y)) => Ok(x == y),
        // IEEE equality: NaN != NaN. The checker allows Float: Eq for exactly
        // this behaviour.
        (Value::Float(x), Value::Float(y)) => Ok(x == y),
        (Value::Bool(x), Value::Bool(y)) => Ok(x == y),
        (Value::Str(x), Value::Str(y)) => Ok(x == y),
        (Value::Char(x), Value::Char(y)) => Ok(x == y),
        (Value::Unit, Value::Unit) => Ok(true),
        (Value::List(xs), Value::List(ys)) => {
            if xs.len() != ys.len() {
                return Ok(false);
            }
            for (x, y) in xs.iter().zip(ys.iter()) {
                if !values_equal(x, y, span)? {
                    return Ok(false);
                }
            }
            Ok(true)
        }
        (Value::Map(xs), Value::Map(ys)) => {
            // Insertion order is display order, not identity: `==` is
            // key-value correspondence (0007 §3). Keys within a map are
            // unique, so equal lengths plus every pair found is a bijection.
            if xs.len() != ys.len() {
                return Ok(false);
            }
            for (key, value) in xs.iter() {
                let mut matched = false;
                for (other_key, other_value) in ys.iter() {
                    if values_equal(key, other_key, span)? {
                        matched = values_equal(value, other_value, span)?;
                        break;
                    }
                }
                if !matched {
                    return Ok(false);
                }
            }
            Ok(true)
        }
        (Value::ErrorValue(x), Value::ErrorValue(y)) => Ok(x == y),
        (
            Value::Struct {
                def: d1,
                fields: f1,
            },
            Value::Struct {
                def: d2,
                fields: f2,
            },
        ) => {
            if d1 != d2 || f1.len() != f2.len() {
                return Ok(false);
            }
            for (x, y) in f1.iter().zip(f2.iter()) {
                if !values_equal(x, y, span)? {
                    return Ok(false);
                }
            }
            Ok(true)
        }
        (
            Value::Enum {
                def: d1,
                variant: v1,
                payload: p1,
            },
            Value::Enum {
                def: d2,
                variant: v2,
                payload: p2,
            },
        ) => {
            if d1 != d2 || v1 != v2 || p1.len() != p2.len() {
                return Ok(false);
            }
            for (x, y) in p1.iter().zip(p2.iter()) {
                if !values_equal(x, y, span)? {
                    return Ok(false);
                }
            }
            Ok(true)
        }
        _ => trap(span, "these values cannot be compared with `==`"),
    }
}

/// `None` models IEEE's unordered comparisons (NaN).
pub(super) fn compare<'a>(
    a: &Value<'a>,
    b: &Value<'a>,
    span: Span,
) -> Eval<'a, Option<std::cmp::Ordering>> {
    match (a, b) {
        (Value::Int(x), Value::Int(y)) => Ok(Some(x.cmp(y))),
        (Value::Float(x), Value::Float(y)) => Ok(x.partial_cmp(y)),
        (Value::Str(x), Value::Str(y)) => Ok(Some(x.cmp(y))),
        (Value::Char(x), Value::Char(y)) => Ok(Some(x.cmp(y))),
        // Bool and Unit satisfy `Ord` structurally, so `sorted` must order
        // them; false < true.
        (Value::Bool(x), Value::Bool(y)) => Ok(Some(x.cmp(y))),
        (Value::Unit, Value::Unit) => Ok(Some(std::cmp::Ordering::Equal)),
        _ => trap(span, "these values cannot be ordered"),
    }
}
