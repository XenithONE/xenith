//! The tree-walking interpreter.
//!
//! Values are values: binding, passing and returning copy them, which *is*
//! the kernel's value semantics — with no observable aliasing of unique
//! values, a copy and a move are indistinguishable, and the checker owns the
//! job of making wasteful patterns visible later.
//!
//! Control flow rides `Result`: the error side carries early `return`,
//! `break`, `continue`, and runtime traps. Traps are precise and carry a
//! span — a runtime error message with no position is a bug report nobody
//! can act on.

use xenith_diag::Span;
use xenith_sema::def::{DefKind, DefTable};
use xenith_sema::ty::DefId;
use xenith_syntax::ast;

// ------------------------------------------------------------------- values

#[derive(Clone, Debug)]
pub enum Value<'a> {
    Int(i64),
    Float(f64),
    Bool(bool),
    Str(String),
    Char(char),
    Unit,
    /// A `List<T>` value. Reads copy (design/0007 D1); only `push`, `pop`
    /// and `replace` write through the receiver in place.
    List(Vec<Value<'a>>),
    /// A `Map<K, V>` value in insertion order — the order is normative
    /// (design/0007 §3), so pairs beat a hash table at this scale.
    Map(Vec<(Value<'a>, Value<'a>)>),
    /// A value of the opaque prelude `Error` type. The message exists for
    /// debug rendering; nothing in the language reads it back out.
    ErrorValue(String),
    Struct {
        def: DefId,
        /// Field values in declaration order.
        fields: Vec<Value<'a>>,
    },
    Enum {
        def: DefId,
        variant: usize,
        payload: Vec<Value<'a>>,
    },
    /// A function value: a lambda, a named function, or reference thereto.
    Fn {
        params: Vec<String>,
        body: Body<'a>,
        captured: Vec<(String, Value<'a>)>,
        is_async: bool,
    },
    /// A variant constructor used as a value: `ScoreError.NotFound`.
    VariantCtor {
        def: DefId,
        variant: usize,
        arity: usize,
    },
    /// A capability handed to `main`. The name is the prelude type ("Io").
    Capability(&'static str),
    /// The result of calling an `async fn`. Single-threaded for now: the body
    /// has already run; `.await` unwraps.
    Task(Box<Value<'a>>),
}

#[derive(Clone, Debug)]
pub enum Body<'a> {
    Block(&'a ast::Block),
    Expr(&'a ast::Expr),
}

/// Why evaluation stopped early.
enum Control<'a> {
    Return(Value<'a>),
    Break,
    Continue,
    Trap { message: String, span: Span },
}

type Eval<'a, T> = Result<T, Control<'a>>;

fn trap<'a, T>(span: Span, message: impl Into<String>) -> Eval<'a, T> {
    Err(Control::Trap {
        message: message.into(),
        span,
    })
}

// ------------------------------------------------------------------ outcome

pub struct Outcome {
    /// 0 = `main` succeeded; 1 = `main` returned `Err`; 101 = a trap fired.
    pub exit: i32,
    pub stdout: Vec<u8>,
    /// The trap, when exit is 101.
    pub error: Option<(String, Span)>,
}

/// Run `fn main` of a checked module. The caller is responsible for having
/// refused a module with diagnostics; holes type-check clean and are allowed
/// through — reaching one is a precise trap, which is the workflow.
pub fn run(module: &ast::Module, table: &DefTable) -> Outcome {
    let mut interp = Interp {
        table,
        module,
        stdout: Vec::new(),
    };

    let Some(main) = find_fn(module, "main") else {
        return Outcome {
            exit: 101,
            stdout: Vec::new(),
            error: Some((
                "no `fn main` to run — a program starts there".to_string(),
                Span::EMPTY,
            )),
        };
    };

    // main's parameters are its capabilities; there is nowhere else a
    // capability can come from.
    let mut env = Env::new();
    for param in &main.params {
        let capability = match capability_name(&param.ty) {
            Some(name) => Value::Capability(name),
            None => {
                return Outcome {
                    exit: 101,
                    stdout: interp.stdout,
                    error: Some((
                        format!(
                            "`main` takes capabilities only; `{}` is not one",
                            param.name.name
                        ),
                        param.span,
                    )),
                };
            }
        };
        env.bind(&param.name.name, capability);
    }

    let result = match &main.body {
        Some(body) => interp.block(body, &mut env),
        None => Ok(Value::Unit),
    };

    match result {
        Ok(value) | Err(Control::Return(value)) => {
            let exit = match &value {
                Value::Enum { def, variant, .. }
                    if *def == table.result && *variant == err_index() =>
                {
                    1
                }
                _ => 0,
            };
            Outcome {
                exit,
                stdout: interp.stdout,
                error: None,
            }
        }
        Err(Control::Trap { message, span }) => Outcome {
            exit: 101,
            stdout: interp.stdout,
            error: Some((message, span)),
        },
        Err(Control::Break) | Err(Control::Continue) => Outcome {
            exit: 101,
            stdout: interp.stdout,
            error: Some((
                "`break` or `continue` escaped every loop — checker gap".to_string(),
                Span::EMPTY,
            )),
        },
    }
}

fn find_fn<'a>(module: &'a ast::Module, name: &str) -> Option<&'a ast::FnItem> {
    module.items.iter().find_map(|item| match &item.kind {
        ast::ItemKind::Fn(f) if f.name.name == name => Some(f),
        _ => None,
    })
}

fn capability_name(ty: &ast::Type) -> Option<&'static str> {
    if let ast::TypeKind::Named { path, args } = &ty.kind {
        if args.is_empty() && path.segments.len() == 1 {
            // The runtime knows how to service exactly these.
            return match path.segments[0].name.as_str() {
                "Io" => Some("Io"),
                _ => None,
            };
        }
    }
    None
}

/// `Ok` is variant 0, `Err` is 1, `Some` is 0, `None` is 1 — fixed by the
/// prelude's declaration order in `def.rs`.
fn ok_index() -> usize {
    0
}
fn err_index() -> usize {
    1
}
fn some_index() -> usize {
    0
}
fn none_index() -> usize {
    1
}

// -------------------------------------------------------------- environment

struct Env<'a> {
    scopes: Vec<Vec<(String, Value<'a>)>>,
}

impl<'a> Env<'a> {
    fn new() -> Env<'a> {
        Env {
            scopes: vec![Vec::new()],
        }
    }

    fn bind(&mut self, name: &str, value: Value<'a>) {
        if name.is_empty() {
            return;
        }
        self.scopes
            .last_mut()
            .expect("one scope")
            .push((name.to_string(), value));
    }

    fn get(&self, name: &str) -> Option<&Value<'a>> {
        self.scopes
            .iter()
            .rev()
            .flat_map(|scope| scope.iter().rev())
            .find(|(n, _)| n == name)
            .map(|(_, v)| v)
    }

    fn get_mut(&mut self, name: &str) -> Option<&mut Value<'a>> {
        self.scopes
            .iter_mut()
            .rev()
            .flat_map(|scope| scope.iter_mut().rev())
            .find(|(n, _)| n == name)
            .map(|(_, v)| v)
    }

    /// Everything visible, innermost occurrence winning — a lambda captures
    /// this by value.
    fn snapshot(&self) -> Vec<(String, Value<'a>)> {
        let mut seen = std::collections::HashSet::new();
        let mut out = Vec::new();
        for scope in self.scopes.iter().rev() {
            for (name, value) in scope.iter().rev() {
                if seen.insert(name.clone()) {
                    out.push((name.clone(), value.clone()));
                }
            }
        }
        out.reverse();
        out
    }
}

// ------------------------------------------------------------- interpreter

struct Interp<'a> {
    table: &'a DefTable,
    module: &'a ast::Module,
    stdout: Vec<u8>,
}

impl<'a> Interp<'a> {
    // ----- blocks and statements -----

    fn block(&mut self, block: &'a ast::Block, env: &mut Env<'a>) -> Eval<'a, Value<'a>> {
        env.scopes.push(Vec::new());
        let result = self.block_inner(block, env);
        env.scopes.pop();
        result
    }

    fn block_inner(&mut self, block: &'a ast::Block, env: &mut Env<'a>) -> Eval<'a, Value<'a>> {
        for stmt in &block.stmts {
            self.stmt(stmt, env)?;
        }
        match &block.tail {
            Some(tail) => self.eval(tail, env),
            None => Ok(Value::Unit),
        }
    }

    fn stmt(&mut self, stmt: &'a ast::Stmt, env: &mut Env<'a>) -> Eval<'a, ()> {
        match &stmt.kind {
            ast::StmtKind::Let { pattern, init, .. } => {
                let value = self.eval(init, env)?;
                self.bind_pattern(pattern, value, env, stmt.span)?;
                Ok(())
            }
            ast::StmtKind::Expr(expr) => {
                self.eval(expr, env)?;
                Ok(())
            }
            ast::StmtKind::Return(value) => {
                let value = match value {
                    Some(value) => self.eval(value, env)?,
                    None => Value::Unit,
                };
                Err(Control::Return(value))
            }
            ast::StmtKind::Break => Err(Control::Break),
            ast::StmtKind::Continue => Err(Control::Continue),
            ast::StmtKind::While { cond, body } => {
                loop {
                    match self.eval(cond, env)? {
                        Value::Bool(true) => {}
                        Value::Bool(false) => break,
                        _ => return trap(cond.span, "`while` needs a Bool"),
                    }
                    match self.block(body, env) {
                        Ok(_) => {}
                        Err(Control::Break) => break,
                        Err(Control::Continue) => continue,
                        Err(other) => return Err(other),
                    }
                }
                Ok(())
            }
            ast::StmtKind::For { iter, .. } => {
                // Iteration syntax is deferred to a later RFC (design/0007
                // §2); until it lands, iteration is `while` + `len` + `get`.
                trap(
                    iter.span,
                    "`for` cannot run yet: iterate with `while` + `len` + `get`",
                )
            }
            ast::StmtKind::Error => Ok(()),
        }
    }

    // ----- expressions -----

    fn eval(&mut self, expr: &'a ast::Expr, env: &mut Env<'a>) -> Eval<'a, Value<'a>> {
        match &expr.kind {
            ast::ExprKind::Int(text) => {
                let cleaned: String = text.chars().filter(|c| *c != '_').collect();
                match cleaned.parse::<i64>() {
                    Ok(value) => Ok(Value::Int(value)),
                    Err(_) => trap(expr.span, "integer literal does not fit in 64 bits"),
                }
            }
            ast::ExprKind::Float(text) => {
                let cleaned: String = text.chars().filter(|c| *c != '_').collect();
                match cleaned.parse::<f64>() {
                    Ok(value) => Ok(Value::Float(value)),
                    Err(_) => trap(expr.span, "float literal does not parse"),
                }
            }
            ast::ExprKind::Str(raw) => Ok(Value::Str(unescape(raw, expr.span)?)),
            ast::ExprKind::Char(raw) => {
                let text = unescape(raw, expr.span)?;
                let mut chars = text.chars();
                match (chars.next(), chars.next()) {
                    (Some(c), None) => Ok(Value::Char(c)),
                    _ => trap(expr.span, "character literal must hold one character"),
                }
            }
            ast::ExprKind::Bool(value) => Ok(Value::Bool(*value)),
            ast::ExprKind::Unit => Ok(Value::Unit),

            ast::ExprKind::Hole { name } => {
                let shown = name.as_deref().unwrap_or("");
                trap(
                    expr.span,
                    format!(
                        "reached hole ??{shown} — ask `xenith goals` what belongs here, then fill it"
                    ),
                )
            }

            ast::ExprKind::Path(path) => self.path_value(path, expr.span, env),

            ast::ExprKind::Unary { op, operand } => {
                let value = self.eval(operand, env)?;
                match (op, value) {
                    (ast::UnaryOp::Neg, Value::Int(v)) => match v.checked_neg() {
                        Some(v) => Ok(Value::Int(v)),
                        None => trap(expr.span, "integer overflow negating i64::MIN"),
                    },
                    (ast::UnaryOp::Neg, Value::Float(v)) => Ok(Value::Float(-v)),
                    (ast::UnaryOp::Not, Value::Bool(v)) => Ok(Value::Bool(!v)),
                    _ => trap(expr.span, "operand type does not fit this operator"),
                }
            }

            ast::ExprKind::Binary { op, lhs, rhs } => self.binary(*op, lhs, rhs, expr.span, env),

            ast::ExprKind::Assign { target, op, value } => {
                let mut new_value = self.eval(value, env)?;
                if let Some(op) = op {
                    let current = self.read_place(target, env)?;
                    new_value = arith(*op, &current, &new_value, expr.span)?;
                }
                self.write_place(target, new_value, env)?;
                Ok(Value::Unit)
            }

            ast::ExprKind::Call { callee, args } => self.call(callee, args, expr.span, env),

            ast::ExprKind::MethodCall {
                receiver,
                method,
                args,
            } => self.method_call(receiver, method, args, expr.span, env),

            ast::ExprKind::Field { receiver, name } => {
                // `Enum.Variant` before value field access, mirroring the
                // checker's resolution order.
                if let ast::ExprKind::Path(path) = &receiver.kind {
                    if let [single] = path.segments.as_slice() {
                        if env.get(&single.name).is_none() {
                            if let Some(def) = self.table.lookup(&single.name) {
                                return self.variant_ref(def, &name.name, expr.span);
                            }
                        }
                    }
                }
                let value = self.eval(receiver, env)?;
                self.field_of(value, &name.name, expr.span)
            }

            ast::ExprKind::Await(inner) => match self.eval(inner, env)? {
                Value::Task(value) => Ok(*value),
                _ => trap(expr.span, "`.await` needs a Task"),
            },

            ast::ExprKind::Try(inner) => {
                let value = self.eval(inner, env)?;
                match value {
                    Value::Enum {
                        def,
                        variant,
                        mut payload,
                    } if def == self.table.result => {
                        if variant == ok_index() {
                            Ok(payload.remove(0))
                        } else {
                            // Propagate the whole Err to the caller.
                            Err(Control::Return(Value::Enum {
                                def,
                                variant,
                                payload,
                            }))
                        }
                    }
                    Value::Enum {
                        def,
                        variant,
                        mut payload,
                    } if def == self.table.option => {
                        if variant == some_index() {
                            Ok(payload.remove(0))
                        } else {
                            Err(Control::Return(Value::Enum {
                                def,
                                variant: none_index(),
                                payload: Vec::new(),
                            }))
                        }
                    }
                    _ => trap(expr.span, "`?` needs a Result or Option"),
                }
            }

            ast::ExprKind::If {
                cond,
                then_block,
                else_branch,
            } => match self.eval(cond, env)? {
                Value::Bool(true) => self.block(then_block, env),
                Value::Bool(false) => match else_branch {
                    Some(branch) => self.eval(branch, env),
                    None => Ok(Value::Unit),
                },
                _ => trap(cond.span, "`if` needs a Bool"),
            },

            ast::ExprKind::Match { scrutinee, arms } => {
                let value = self.eval(scrutinee, env)?;
                for arm in arms {
                    env.scopes.push(Vec::new());
                    let matched = self.try_pattern(&arm.pattern, &value, env, arm.span)?;
                    if matched {
                        if let Some(guard) = &arm.guard {
                            match self.eval(guard, env)? {
                                Value::Bool(true) => {}
                                Value::Bool(false) => {
                                    env.scopes.pop();
                                    continue;
                                }
                                _ => {
                                    env.scopes.pop();
                                    return trap(guard.span, "a guard needs a Bool");
                                }
                            }
                        }
                        let result = self.eval(&arm.body, env);
                        env.scopes.pop();
                        return result;
                    }
                    env.scopes.pop();
                }
                // Exhaustiveness checking is deferred (design/0006 §5), so
                // this is reachable and must be a precise trap, not UB.
                trap(
                    expr.span,
                    "no `match` arm matched — exhaustiveness is on you until XN5xxx lands",
                )
            }

            ast::ExprKind::Block(block) => self.block(block, env),

            ast::ExprKind::ListLit(elements) => {
                let mut items = Vec::with_capacity(elements.len());
                for element in elements {
                    items.push(self.eval(element, env)?);
                }
                Ok(Value::List(items))
            }

            ast::ExprKind::StructLit { path, fields } => {
                let name = &path.segments[0].name;
                let Some(def) = self.table.lookup(name) else {
                    return trap(expr.span, format!("`{name}` is not a struct"));
                };
                let DefKind::Struct {
                    fields: declared, ..
                } = &self.table.def(def).kind
                else {
                    return trap(expr.span, format!("`{name}` is not a struct"));
                };
                // Evaluate in source order (kernel: strict left-to-right),
                // store in declaration order.
                let mut evaluated: Vec<(String, Value)> = Vec::new();
                for init in fields {
                    let value = self.eval(&init.value, env)?;
                    evaluated.push((init.name.name.clone(), value));
                }
                let mut ordered = Vec::with_capacity(declared.len());
                for field in declared {
                    match evaluated.iter().position(|(n, _)| *n == field.name) {
                        Some(index) => ordered.push(evaluated.remove(index).1),
                        None => {
                            return trap(
                                expr.span,
                                format!("field `{}` was never set", field.name),
                            );
                        }
                    }
                }
                Ok(Value::Struct {
                    def,
                    fields: ordered,
                })
            }

            ast::ExprKind::Lambda {
                params,
                is_async,
                body,
                ..
            } => Ok(Value::Fn {
                params: params.iter().map(|p| p.name.name.clone()).collect(),
                body: Body::Expr(body),
                captured: env.snapshot(),
                is_async: *is_async,
            }),

            ast::ExprKind::Error => trap(expr.span, "cannot run code the parser could not read"),
        }
    }

    // ----- names -----

    fn path_value(
        &mut self,
        path: &'a ast::Path,
        span: Span,
        env: &mut Env<'a>,
    ) -> Eval<'a, Value<'a>> {
        let name = &path.segments[0].name;
        if let Some(value) = env.get(name) {
            return Ok(value.clone());
        }
        if let Some(f) = find_fn(self.module, name) {
            return Ok(Value::Fn {
                params: f.params.iter().map(|p| p.name.name.clone()).collect(),
                body: match &f.body {
                    Some(body) => Body::Block(body),
                    None => return trap(span, format!("`{name}` has no body")),
                },
                captured: Vec::new(),
                is_async: f.is_async,
            });
        }
        if let Some((def, variant)) = self.table.unqualified_variant(name) {
            let index = self.variant_index(def, &variant.name);
            if variant.payload.is_empty() {
                return Ok(Value::Enum {
                    def,
                    variant: index,
                    payload: Vec::new(),
                });
            }
            return Ok(Value::VariantCtor {
                def,
                variant: index,
                arity: variant.payload.len(),
            });
        }
        trap(span, format!("nothing named `{name}` at runtime"))
    }

    fn variant_index(&self, def: DefId, variant_name: &str) -> usize {
        match &self.table.def(def).kind {
            DefKind::Enum { variants } => variants
                .iter()
                .position(|v| v.name == variant_name)
                .unwrap_or(0),
            _ => 0,
        }
    }

    fn variant_ref(&mut self, def: DefId, variant_name: &str, span: Span) -> Eval<'a, Value<'a>> {
        let Some(variant) = self.table.variant_named(def, variant_name) else {
            return trap(span, format!("no variant `{variant_name}`"));
        };
        let index = self.variant_index(def, variant_name);
        if variant.payload.is_empty() {
            Ok(Value::Enum {
                def,
                variant: index,
                payload: Vec::new(),
            })
        } else {
            Ok(Value::VariantCtor {
                def,
                variant: index,
                arity: variant.payload.len(),
            })
        }
    }

    fn field_of(&self, value: Value<'a>, field: &str, span: Span) -> Eval<'a, Value<'a>> {
        match value {
            Value::Struct { def, mut fields } => {
                let DefKind::Struct {
                    fields: declared, ..
                } = &self.table.def(def).kind
                else {
                    return trap(span, "not a struct");
                };
                match declared.iter().position(|f| f.name == field) {
                    Some(index) => Ok(fields.remove(index)),
                    None => trap(span, format!("no field `{field}`")),
                }
            }
            _ => trap(span, format!("`{field}` is not a field of this value")),
        }
    }

    // ----- calls -----

    fn call(
        &mut self,
        callee: &'a ast::Expr,
        args: &'a [ast::Arg],
        span: Span,
        env: &mut Env<'a>,
    ) -> Eval<'a, Value<'a>> {
        // Named functions and variant constructors resolve before locals do
        // not shadow them — same order as the checker.
        let callee_value = match &callee.kind {
            ast::ExprKind::Path(path) if path.segments.len() == 1 => {
                // The one prelude free function (design/0007 D4). A user
                // declaration of the same name is a duplicate-definition
                // error, so nothing real is shadowed here.
                let name = &path.segments[0].name;
                if name == "empty_map"
                    && env.get(name).is_none()
                    && find_fn(self.module, name).is_none()
                {
                    for arg in args {
                        self.eval(&arg.value, env)?;
                    }
                    return Ok(Value::Map(Vec::new()));
                }
                self.path_value(path, callee.span, env)?
            }
            ast::ExprKind::Field { receiver, name } => {
                if let ast::ExprKind::Path(path) = &receiver.kind {
                    if let [single] = path.segments.as_slice() {
                        if env.get(&single.name).is_none() {
                            if let Some(def) = self.table.lookup(&single.name) {
                                self.variant_ref(def, &name.name, callee.span)?
                            } else {
                                self.eval(callee, env)?
                            }
                        } else {
                            self.eval(callee, env)?
                        }
                    } else {
                        self.eval(callee, env)?
                    }
                } else {
                    self.eval(callee, env)?
                }
            }
            _ => self.eval(callee, env)?,
        };

        let mut evaluated = Vec::with_capacity(args.len());
        for arg in args {
            evaluated.push(self.eval(&arg.value, env)?);
        }

        self.apply(callee_value, evaluated, span)
    }

    fn apply(
        &mut self,
        callee: Value<'a>,
        args: Vec<Value<'a>>,
        span: Span,
    ) -> Eval<'a, Value<'a>> {
        match callee {
            Value::Fn {
                params,
                body,
                captured,
                is_async,
            } => {
                if params.len() != args.len() {
                    return trap(span, "wrong number of arguments");
                }
                let mut env = Env::new();
                for (name, value) in captured {
                    env.bind(&name, value);
                }
                env.scopes.push(Vec::new());
                for (param, value) in params.iter().zip(args) {
                    env.bind(param, value);
                }
                let result = match body {
                    Body::Block(block) => self.block_inner(block, &mut env),
                    Body::Expr(expr) => self.eval(expr, &mut env),
                };
                let value = match result {
                    Ok(value) | Err(Control::Return(value)) => value,
                    Err(other) => return Err(other),
                };
                if is_async {
                    Ok(Value::Task(Box::new(value)))
                } else {
                    Ok(value)
                }
            }
            Value::VariantCtor {
                def,
                variant,
                arity,
            } => {
                if args.len() != arity {
                    return trap(span, "wrong number of constructor arguments");
                }
                Ok(Value::Enum {
                    def,
                    variant,
                    payload: args,
                })
            }
            _ => trap(span, "this value is not callable"),
        }
    }

    /// Built-in methods — the runtime half of the provisional prelude in
    /// `def.rs`. The two tables must agree; the examples exercise both.
    fn method_call(
        &mut self,
        receiver: &'a ast::Expr,
        method: &'a ast::Ident,
        args: &'a [ast::Arg],
        span: Span,
        env: &mut Env<'a>,
    ) -> Eval<'a, Value<'a>> {
        // `Grade.Pass(95)` parses as a method call; construct the variant.
        // Mirrors the checker's resolution order exactly.
        if let ast::ExprKind::Path(path) = &receiver.kind {
            if let [single] = path.segments.as_slice() {
                if env.get(&single.name).is_none() {
                    if let Some(def) = self.table.lookup(&single.name) {
                        if self.table.variant_named(def, &method.name).is_some() {
                            let ctor = self.variant_ref(def, &method.name, span)?;
                            let mut evaluated = Vec::with_capacity(args.len());
                            for arg in args {
                                evaluated.push(self.eval(&arg.value, env)?);
                            }
                            return self.apply(ctor, evaluated, span);
                        }
                    }
                }
            }
        }

        // The container mutators write through the receiver in place, so it
        // is resolved as a place — the same resolution `=` uses — rather than
        // evaluated to a copy. Arguments go first, as assignment evaluates
        // its right-hand side first, so the place borrow overlaps nothing.
        if matches!(
            method.name.as_str(),
            "push" | "pop" | "replace" | "insert" | "remove"
        ) {
            let mut evaluated = Vec::with_capacity(args.len());
            for arg in args {
                evaluated.push(self.eval(&arg.value, env)?);
            }
            let slot = self.resolve_place(receiver, env)?;
            return match (&mut *slot, method.name.as_str()) {
                (Value::List(items), "push") => {
                    let Some(item) = evaluated.into_iter().next() else {
                        return trap(span, "push takes a value");
                    };
                    items.push(item);
                    Ok(Value::Unit)
                }
                (Value::List(items), "pop") => Ok(self.option_of(items.pop())),
                (Value::List(items), "replace") => {
                    let mut taken = evaluated.into_iter();
                    let (Some(Value::Int(index)), Some(value)) = (taken.next(), taken.next())
                    else {
                        return trap(span, "replace takes an index and a value");
                    };
                    // Out of range leaves the list untouched (0007 §3).
                    let old = usize::try_from(index)
                        .ok()
                        .filter(|i| *i < items.len())
                        .map(|i| std::mem::replace(&mut items[i], value));
                    Ok(self.option_of(old))
                }
                (Value::Map(entries), "insert") => {
                    let mut taken = evaluated.into_iter();
                    let (Some(key), Some(value)) = (taken.next(), taken.next()) else {
                        return trap(span, "insert takes a key and a value");
                    };
                    // An existing key keeps its position and its stored key;
                    // only the value moves (0007 §3 normative order).
                    let mut existing = None;
                    for (index, (stored, _)) in entries.iter().enumerate() {
                        if values_equal(stored, &key, span)? {
                            existing = Some(index);
                            break;
                        }
                    }
                    match existing {
                        Some(index) => {
                            let old = std::mem::replace(&mut entries[index].1, value);
                            Ok(self.option_of(Some(old)))
                        }
                        None => {
                            entries.push((key, value));
                            Ok(self.option_of(None))
                        }
                    }
                }
                (Value::Map(entries), "remove") => {
                    let Some(key) = evaluated.into_iter().next() else {
                        return trap(span, "remove takes a key");
                    };
                    let mut found = None;
                    for (index, (stored, _)) in entries.iter().enumerate() {
                        if values_equal(stored, &key, span)? {
                            found = Some(index);
                            break;
                        }
                    }
                    // Vec::remove shifts, so the survivors keep their order;
                    // a later re-insert of the key lands at the end.
                    Ok(self.option_of(found.map(|index| entries.remove(index).1)))
                }
                _ => trap(
                    span,
                    format!("no runtime method `{}` for this value", method.name),
                ),
            };
        }

        let receiver_value = self.eval(receiver, env)?;
        let mut evaluated = Vec::with_capacity(args.len());
        for arg in args {
            evaluated.push(self.eval(&arg.value, env)?);
        }

        match (&receiver_value, method.name.as_str()) {
            (Value::Int(a), "checked_add") => {
                let Some(Value::Int(b)) = evaluated.first() else {
                    return trap(span, "checked_add takes an Int");
                };
                Ok(match a.checked_add(*b) {
                    Some(sum) => Value::Enum {
                        def: self.table.option,
                        variant: some_index(),
                        payload: vec![Value::Int(sum)],
                    },
                    None => Value::Enum {
                        def: self.table.option,
                        variant: none_index(),
                        payload: Vec::new(),
                    },
                })
            }
            (Value::Int(a), "to_text") => Ok(Value::Str(a.to_string())),
            (Value::Str(a), "concat") => {
                let Some(Value::Str(b)) = evaluated.first() else {
                    return trap(span, "concat takes a String");
                };
                Ok(Value::Str(format!("{a}{b}")))
            }
            // `len` counts Unicode scalar values, never bytes (D2).
            (Value::Str(a), "len") => Ok(Value::Int(a.chars().count() as i64)),
            (Value::Str(a), "split") => {
                let Some(Value::Str(sep)) = evaluated.first() else {
                    return trap(span, "split takes a String");
                };
                // Lossless by construction: `pieces.join(sep)` rebuilds the
                // input exactly, empty pieces included. The empty separator
                // is the `chars` replacement — one piece per scalar.
                let pieces: Vec<Value> = if sep.is_empty() {
                    a.chars().map(|c| Value::Str(c.to_string())).collect()
                } else {
                    a.split(sep.as_str())
                        .map(|piece| Value::Str(piece.to_string()))
                        .collect()
                };
                Ok(Value::List(pieces))
            }
            (Value::Str(a), "trim") => Ok(Value::Str(
                a.trim_matches(|c: char| matches!(c, ' ' | '\t' | '\r' | '\n'))
                    .to_string(),
            )),
            (Value::Str(a), "try_to_int") => {
                // Accepted shape: ASCII whitespace, then [+-]?[0-9]+ (0007
                // §3). Everything else — separators, decimals, overflow — is
                // an Err value, never a trap.
                let trimmed = a.trim_matches(|c: char| matches!(c, ' ' | '\t' | '\r' | '\n'));
                Ok(match trimmed.parse::<i64>() {
                    Ok(value) => Value::Enum {
                        def: self.table.result,
                        variant: ok_index(),
                        payload: vec![Value::Int(value)],
                    },
                    Err(error) => {
                        let message = match error.kind() {
                            std::num::IntErrorKind::PosOverflow
                            | std::num::IntErrorKind::NegOverflow => "out of Int range",
                            _ => "not an integer",
                        };
                        Value::Enum {
                            def: self.table.result,
                            variant: err_index(),
                            payload: vec![Value::ErrorValue(message.to_string())],
                        }
                    }
                })
            }
            (Value::Str(a), "starts_with") => {
                let Some(Value::Str(prefix)) = evaluated.first() else {
                    return trap(span, "starts_with takes a String");
                };
                Ok(Value::Bool(a.starts_with(prefix.as_str())))
            }
            (Value::Str(a), "contains") => {
                let Some(Value::Str(sub)) = evaluated.first() else {
                    return trap(span, "contains takes a String");
                };
                Ok(Value::Bool(a.contains(sub.as_str())))
            }
            (Value::List(items), "len") => Ok(Value::Int(items.len() as i64)),
            (Value::List(items), "is_empty") => Ok(Value::Bool(items.is_empty())),
            (Value::List(items), "get") => {
                let Some(Value::Int(index)) = evaluated.first() else {
                    return trap(span, "get takes an Int");
                };
                // Negative and out-of-range are both None; the hit is a copy
                // of the element (D1).
                let item = usize::try_from(*index)
                    .ok()
                    .and_then(|i| items.get(i))
                    .cloned();
                Ok(self.option_of(item))
            }
            (Value::List(items), "contains") => {
                let Some(needle) = evaluated.first() else {
                    return trap(span, "contains takes a value");
                };
                let mut found = false;
                for item in items {
                    if values_equal(item, needle, span)? {
                        found = true;
                        break;
                    }
                }
                Ok(Value::Bool(found))
            }
            (Value::List(items), "sorted") => {
                // Insertion keeps the sort stable and lets a comparison trap
                // propagate, which `sort_by` cannot.
                let mut sorted = items.clone();
                let mut i = 1;
                while i < sorted.len() {
                    let mut j = i;
                    while j > 0 {
                        let ordering = compare(&sorted[j - 1], &sorted[j], span)?;
                        if ordering != Some(std::cmp::Ordering::Greater) {
                            break;
                        }
                        sorted.swap(j - 1, j);
                        j -= 1;
                    }
                    i += 1;
                }
                Ok(Value::List(sorted))
            }
            (Value::List(items), "concat") => {
                let Some(Value::List(other)) = evaluated.first() else {
                    return trap(span, "concat takes a List");
                };
                let mut joined = items.clone();
                joined.extend(other.iter().cloned());
                Ok(Value::List(joined))
            }
            (Value::List(items), "join") => {
                let Some(Value::Str(sep)) = evaluated.first() else {
                    return trap(span, "join takes a String");
                };
                let rendered: Vec<String> =
                    items.iter().map(|item| self.value_text(item)).collect();
                Ok(Value::Str(rendered.join(sep)))
            }
            (Value::Map(entries), "len") => Ok(Value::Int(entries.len() as i64)),
            (Value::Map(entries), "is_empty") => Ok(Value::Bool(entries.is_empty())),
            (Value::Map(entries), "get") => {
                let Some(key) = evaluated.first() else {
                    return trap(span, "get takes a key");
                };
                let mut hit = None;
                for (stored, value) in entries {
                    if values_equal(stored, key, span)? {
                        // D1: the read is a copy of the value.
                        hit = Some(value.clone());
                        break;
                    }
                }
                Ok(self.option_of(hit))
            }
            (Value::Map(entries), "has_key") => {
                let Some(key) = evaluated.first() else {
                    return trap(span, "has_key takes a key");
                };
                let mut found = false;
                for (stored, _) in entries {
                    if values_equal(stored, key, span)? {
                        found = true;
                        break;
                    }
                }
                Ok(Value::Bool(found))
            }
            // Insertion-order snapshot: later mutation of the map must not
            // reach into a list already handed out (0007 §3).
            (Value::Map(entries), "keys") => Ok(Value::List(
                entries.iter().map(|(key, _)| key.clone()).collect(),
            )),
            (Value::Enum { def, variant, .. }, "to_result") if *def == self.table.option => {
                let error = evaluated.into_iter().next().unwrap_or(Value::Unit);
                let Value::Enum {
                    variant, payload, ..
                } = receiver_value
                else {
                    unreachable!("matched above");
                };
                Ok(if variant == some_index() {
                    Value::Enum {
                        def: self.table.result,
                        variant: ok_index(),
                        payload,
                    }
                } else {
                    Value::Enum {
                        def: self.table.result,
                        variant: err_index(),
                        payload: vec![error],
                    }
                })
            }
            (Value::Capability("Io"), "write") => {
                let Some(Value::Str(text)) = evaluated.first() else {
                    return trap(span, "write takes a String");
                };
                self.stdout.extend_from_slice(text.as_bytes());
                Ok(Value::Enum {
                    def: self.table.result,
                    variant: ok_index(),
                    payload: vec![Value::Unit],
                })
            }
            _ => trap(
                span,
                format!("no runtime method `{}` for this value", method.name),
            ),
        }
    }

    /// `Some(value)` / `None` from a Rust `Option`.
    fn option_of(&self, value: Option<Value<'a>>) -> Value<'a> {
        match value {
            Some(value) => Value::Enum {
                def: self.table.option,
                variant: some_index(),
                payload: vec![value],
            },
            None => Value::Enum {
                def: self.table.option,
                variant: none_index(),
                payload: Vec::new(),
            },
        }
    }

    /// Total, deterministic rendering — the runtime face of the sealed `Text`
    /// property, which is total today (design/0006 §3-5). `String` renders
    /// verbatim; everything else the way a literal would be written.
    fn value_text(&self, value: &Value<'a>) -> String {
        match value {
            Value::Int(v) => v.to_string(),
            Value::Float(v) => v.to_string(),
            Value::Bool(v) => v.to_string(),
            Value::Str(v) => v.clone(),
            Value::Char(v) => v.to_string(),
            Value::Unit => "unit".to_string(),
            Value::List(items) => {
                let parts: Vec<String> = items.iter().map(|item| self.value_text(item)).collect();
                format!("[{}]", parts.join(", "))
            }
            // Rendered in insertion order — deterministic by the normative
            // order rules, even though `==` ignores it.
            Value::Map(entries) => {
                let parts: Vec<String> = entries
                    .iter()
                    .map(|(key, value)| {
                        format!("{}: {}", self.value_text(key), self.value_text(value))
                    })
                    .collect();
                format!("{{{}}}", parts.join(", "))
            }
            Value::ErrorValue(message) => format!("Error({message})"),
            Value::Struct { def, fields } => {
                let name = self.table.name_of(*def);
                let DefKind::Struct { fields: declared } = &self.table.def(*def).kind else {
                    return name;
                };
                let parts: Vec<String> = declared
                    .iter()
                    .zip(fields)
                    .map(|(field, value)| format!("{}: {}", field.name, self.value_text(value)))
                    .collect();
                format!("{name} {{ {} }}", parts.join(", "))
            }
            Value::Enum {
                def,
                variant,
                payload,
            } => {
                let name = match &self.table.def(*def).kind {
                    DefKind::Enum { variants } => variants
                        .get(*variant)
                        .map(|v| v.name.clone())
                        .unwrap_or_default(),
                    _ => String::new(),
                };
                if payload.is_empty() {
                    name
                } else {
                    let parts: Vec<String> =
                        payload.iter().map(|part| self.value_text(part)).collect();
                    format!("{name}({})", parts.join(", "))
                }
            }
            Value::Fn { .. } | Value::VariantCtor { .. } => "<fn>".to_string(),
            Value::Capability(name) => format!("<{name}>"),
            Value::Task(_) => "<task>".to_string(),
        }
    }

    // ----- patterns -----

    fn bind_pattern(
        &mut self,
        pattern: &'a ast::Pattern,
        value: Value<'a>,
        env: &mut Env<'a>,
        span: Span,
    ) -> Eval<'a, ()> {
        if self.try_pattern(pattern, &value, env, span)? {
            Ok(())
        } else {
            trap(span, "`let` pattern did not match the value")
        }
    }

    /// Attempt a match, binding as it goes. Bindings from a failed attempt are
    /// discarded by the caller popping the scope.
    fn try_pattern(
        &mut self,
        pattern: &'a ast::Pattern,
        value: &Value<'a>,
        env: &mut Env<'a>,
        span: Span,
    ) -> Eval<'a, bool> {
        match &pattern.kind {
            ast::PatternKind::Wildcard | ast::PatternKind::Error => Ok(true),

            ast::PatternKind::Binding(ident) => {
                // Variant-of-the-scrutinee names match the variant, mirroring
                // the checker (a misspelt `None` must not become a catch-all).
                if let Value::Enum { def, variant, .. } = value {
                    if let Some(found) = self.table.variant_named(*def, &ident.name) {
                        let index = self.variant_index(*def, &found.name);
                        return Ok(index == *variant);
                    }
                }
                env.bind(&ident.name, value.clone());
                Ok(true)
            }

            ast::PatternKind::Literal(expr) => {
                let literal = self.eval(expr, env)?;
                values_equal(&literal, value, span)
            }

            ast::PatternKind::Path(path) => {
                // Enum.Variant
                let (Some(enum_ident), Some(variant_ident)) =
                    (path.segments.first(), path.segments.get(1))
                else {
                    return Ok(false);
                };
                let Some(def) = self.table.lookup(&enum_ident.name) else {
                    return Ok(false);
                };
                let index = self.variant_index(def, &variant_ident.name);
                Ok(matches!(
                    value,
                    Value::Enum { def: d, variant, .. } if *d == def && *variant == index
                ))
            }

            ast::PatternKind::Variant { path, elements } => {
                let Value::Enum {
                    def,
                    variant,
                    payload,
                } = value
                else {
                    return Ok(false);
                };
                let variant_name = match path.segments.as_slice() {
                    [single] => &single.name,
                    [_, second] => &second.name,
                    _ => return Ok(false),
                };
                let index = self.variant_index(*def, variant_name);
                if index != *variant || elements.len() != payload.len() {
                    return Ok(false);
                }
                for (element, part) in elements.iter().zip(payload.iter()) {
                    if !self.try_pattern(element, part, env, span)? {
                        return Ok(false);
                    }
                }
                Ok(true)
            }

            ast::PatternKind::Struct { fields, .. } => {
                let Value::Struct { def, fields: parts } = value else {
                    return Ok(false);
                };
                let DefKind::Struct {
                    fields: declared, ..
                } = &self.table.def(*def).kind
                else {
                    return Ok(false);
                };
                for field in fields {
                    let Some(index) = declared.iter().position(|f| f.name == field.name.name)
                    else {
                        return Ok(false);
                    };
                    let part = &parts[index];
                    match &field.pattern {
                        Some(sub) => {
                            if !self.try_pattern(sub, part, env, span)? {
                                return Ok(false);
                            }
                        }
                        None => env.bind(&field.name.name, part.clone()),
                    }
                }
                Ok(true)
            }

            ast::PatternKind::Or(alternatives) => {
                for alternative in alternatives {
                    if self.try_pattern(alternative, value, env, span)? {
                        return Ok(true);
                    }
                }
                Ok(false)
            }
        }
    }

    // ----- places (assignment targets) -----

    fn read_place(&mut self, target: &'a ast::Expr, env: &mut Env<'a>) -> Eval<'a, Value<'a>> {
        self.eval(target, env)
    }

    fn write_place(
        &mut self,
        target: &'a ast::Expr,
        value: Value<'a>,
        env: &mut Env<'a>,
    ) -> Eval<'a, ()> {
        let slot = self.resolve_place(target, env)?;
        *slot = value;
        Ok(())
    }

    fn resolve_place<'e>(
        &self,
        target: &'a ast::Expr,
        env: &'e mut Env<'a>,
    ) -> Eval<'a, &'e mut Value<'a>> {
        match &target.kind {
            ast::ExprKind::Path(path) => {
                let name = &path.segments[0].name;
                match env.get_mut(name) {
                    Some(slot) => Ok(slot),
                    None => trap(target.span, format!("no binding named `{name}`")),
                }
            }
            ast::ExprKind::Field { receiver, name } => {
                let table = self.table;
                let base = self.resolve_place(receiver, env)?;
                let Value::Struct { def, fields } = base else {
                    return trap(target.span, "not a struct");
                };
                let DefKind::Struct {
                    fields: declared, ..
                } = &table.def(*def).kind
                else {
                    return trap(target.span, "not a struct");
                };
                match declared.iter().position(|f| f.name == name.name) {
                    Some(index) => Ok(&mut fields[index]),
                    None => trap(target.span, format!("no field `{}`", name.name)),
                }
            }
            _ => trap(target.span, "this expression cannot be assigned to"),
        }
    }

    // ----- operators -----

    fn binary(
        &mut self,
        op: ast::BinaryOp,
        lhs: &'a ast::Expr,
        rhs: &'a ast::Expr,
        span: Span,
        env: &mut Env<'a>,
    ) -> Eval<'a, Value<'a>> {
        use ast::BinaryOp as B;

        // Short-circuit first: the kernel names && and || as the only two
        // operators that do not evaluate both sides.
        match op {
            B::And => {
                return match self.eval(lhs, env)? {
                    Value::Bool(false) => Ok(Value::Bool(false)),
                    Value::Bool(true) => self.eval(rhs, env),
                    _ => trap(lhs.span, "`&&` needs Bool"),
                };
            }
            B::Or => {
                return match self.eval(lhs, env)? {
                    Value::Bool(true) => Ok(Value::Bool(true)),
                    Value::Bool(false) => self.eval(rhs, env),
                    _ => trap(lhs.span, "`||` needs Bool"),
                };
            }
            _ => {}
        }

        let left = self.eval(lhs, env)?;
        let right = self.eval(rhs, env)?;

        match op {
            B::Add | B::Sub | B::Mul | B::Div | B::Rem => arith(op, &left, &right, span),
            B::BitAnd | B::BitOr | B::BitXor | B::Shl | B::Shr => {
                let (Value::Int(a), Value::Int(b)) = (&left, &right) else {
                    return trap(span, "bitwise operators need Int");
                };
                let result = match op {
                    B::BitAnd => a & b,
                    B::BitOr => a | b,
                    B::BitXor => a ^ b,
                    B::Shl | B::Shr => {
                        if *b < 0 || *b >= 64 {
                            return trap(span, "shift amount out of range 0..64");
                        }
                        if matches!(op, B::Shl) {
                            match a.checked_shl(*b as u32) {
                                Some(v) => v,
                                None => return trap(span, "integer overflow in `<<`"),
                            }
                        } else {
                            a >> b
                        }
                    }
                    _ => unreachable!(),
                };
                Ok(Value::Int(result))
            }
            B::Eq => Ok(Value::Bool(values_equal(&left, &right, span)?)),
            B::Ne => Ok(Value::Bool(!values_equal(&left, &right, span)?)),
            B::Lt | B::Le | B::Gt | B::Ge => {
                let ordering = compare(&left, &right, span)?;
                let result = match (op, ordering) {
                    (B::Lt, Some(o)) => o.is_lt(),
                    (B::Le, Some(o)) => o.is_le(),
                    (B::Gt, Some(o)) => o.is_gt(),
                    (B::Ge, Some(o)) => o.is_ge(),
                    // IEEE: any comparison with NaN is false.
                    (_, None) => false,
                    _ => unreachable!(),
                };
                Ok(Value::Bool(result))
            }
            B::Identity => trap(span, "`is` needs Shared values, which cannot be built yet"),
            B::And | B::Or => unreachable!("short-circuited above"),
        }
    }
}

/// Trapping integer arithmetic, IEEE float arithmetic.
fn arith<'a>(
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
fn values_equal<'a>(a: &Value<'a>, b: &Value<'a>, span: Span) -> Eval<'a, bool> {
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
            for (x, y) in xs.iter().zip(ys) {
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
            for (key, value) in xs {
                let mut matched = false;
                for (other_key, other_value) in ys {
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
            for (x, y) in f1.iter().zip(f2) {
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
            for (x, y) in p1.iter().zip(p2) {
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
fn compare<'a>(a: &Value<'a>, b: &Value<'a>, span: Span) -> Eval<'a, Option<std::cmp::Ordering>> {
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

/// Strip quotes and resolve the closed escape set. The lexer accepted the
/// literal, so anything unexpected here is a lexer bug worth trapping loudly.
fn unescape<'a>(raw: &str, span: Span) -> Eval<'a, String> {
    let inner = raw
        .strip_prefix('"')
        .and_then(|r| r.strip_suffix('"'))
        .or_else(|| raw.strip_prefix('\'').and_then(|r| r.strip_suffix('\'')))
        .unwrap_or(raw);

    let mut out = String::with_capacity(inner.len());
    let mut chars = inner.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('r') => out.push('\r'),
            Some('t') => out.push('\t'),
            Some('0') => out.push('\0'),
            Some('\\') => out.push('\\'),
            Some('"') => out.push('"'),
            Some('\'') => out.push('\''),
            other => {
                return trap(
                    span,
                    format!("unrecognised escape `\\{}`", other.unwrap_or(' ')),
                );
            }
        }
    }
    Ok(out)
}
