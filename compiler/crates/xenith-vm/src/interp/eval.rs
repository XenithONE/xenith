use std::sync::Arc;

use xenith_diag::Span;
use xenith_sema::def::DefKind;
use xenith_sema::ty::DefId;
use xenith_syntax::ast;

use super::value::{arith, compare, owned, values_equal};
use super::{
    Body, Control, Env, Eval, Interp, RuntimeRef, Value, find_const, find_fn, none_index, ok_index,
    some_index, trap,
};

/// The dotted names of a pure field chain, for module-path resolution.
pub(super) fn expr_segments(expr: &ast::Expr) -> Option<Vec<String>> {
    match &expr.kind {
        ast::ExprKind::Path(path) => Some(path.segments.iter().map(|s| s.name.clone()).collect()),
        ast::ExprKind::Field { receiver, name } => {
            let mut segments = expr_segments(receiver)?;
            segments.push(name.name.clone());
            Some(segments)
        }
        _ => None,
    }
}

impl<'a> Interp<'a> {
    // ----- blocks and statements -----

    pub(super) fn block(
        &mut self,
        block: &'a ast::Block,
        env: &mut Env<'a>,
    ) -> Eval<'a, Value<'a>> {
        env.scopes.push(Vec::new());
        let result = self.block_inner(block, env);
        env.scopes.pop();
        result
    }

    pub(super) fn block_inner(
        &mut self,
        block: &'a ast::Block,
        env: &mut Env<'a>,
    ) -> Eval<'a, Value<'a>> {
        for stmt in &block.stmts {
            self.stmt(stmt, env)?;
        }
        match &block.tail {
            Some(tail) => self.eval(tail, env),
            None => Ok(Value::Unit),
        }
    }

    fn stmt(&mut self, stmt: &'a ast::Stmt, env: &mut Env<'a>) -> Eval<'a, ()> {
        // Safe point (design/0017 §3): statement boundaries, loop iterations
        // and calls are where a cancelled child notices and unwinds. Every
        // way a Xenith program can diverge — `while`, recursion — passes
        // through one of the three, so a diverging sibling is reclaimable.
        self.safe_point()?;
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
                    self.safe_point()?;
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

    pub(super) fn eval(&mut self, expr: &'a ast::Expr, env: &mut Env<'a>) -> Eval<'a, Value<'a>> {
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
            ast::ExprKind::Str(raw) => Ok(Value::str(unescape(raw, expr.span)?)),
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
                            if let Some(def) = self.lookup_type(&single.name) {
                                return self.variant_ref(def, &name.name, expr.span);
                            }
                        }
                    }
                }
                // `game.player.Rank.Gold`, or a foreign function as a value.
                if let Some(mut segments) = expr_segments(receiver) {
                    if env.get(&segments[0]).is_none() {
                        segments.push(name.name.clone());
                        match self.runtime_ref(&segments) {
                            Some(RuntimeRef::Fn(home, bare)) => {
                                return self.fn_value(home, &bare, expr.span);
                            }
                            Some(RuntimeRef::Const(home, bare)) => {
                                return self.const_value(home, &bare, expr.span);
                            }
                            Some(RuntimeRef::Variant(def, variant)) => {
                                return self.variant_ref(def, &variant, expr.span);
                            }
                            None => {}
                        }
                    }
                }
                let value = self.eval(receiver, env)?;
                self.field_of(value, &name.name, expr.span)
            }

            ast::ExprKind::Await(inner) => match self.eval(inner, env)? {
                // The sequential executor's handle: the body already ran.
                Value::Task(value) => Ok(owned(value)),
                // The parallel executor's handle: commit every earlier
                // child of this region first, then take this one's value.
                Value::Pending { index } => {
                    self.commit_through(index)?;
                    match self.children[index].value.take() {
                        Some(value) => Ok(value),
                        // XN6006 refuses a second await; reaching this is a
                        // checker gap, reported as one.
                        None => trap(expr.span, "this task was already awaited — checker gap"),
                    }
                }
                _ => trap(expr.span, "`.await` needs a Task"),
            },

            // The task region (design/0015 §1). Under the parallel executor
            // it is also the join point: the closing brace resolves, in
            // spawn order, every child nothing awaited (design/0017 §3).
            ast::ExprKind::Scope(block) => {
                if self.pool.is_none() {
                    return self.block(block, env);
                }
                let start = self.children.len();
                self.regions.push(start);
                let body = self.block(block, env);
                let drained = self.drain_region(start);
                self.regions.pop();
                // Retire this region's children: a handle cannot outlive its
                // scope, so the list stays bounded even when the scope sits
                // inside a loop.
                self.children.truncate(start);
                self.committed = self.committed.min(start);
                // A child's fate was sealed at its spawn statement, which is
                // before anything the parent did afterwards — so a child
                // trap outranks whatever the parent was carrying out of the
                // block. That is what the sequential executor reports, and
                // reporting anything else here would be the difference.
                drained?;
                body
            }

            // `spawn f(args)`: evaluate the arguments here, in normal order,
            // exactly once (design/0015 §1). Then hand the child to the pool
            // — or, with no pool, run it to completion on the spot, which is
            // the sequential executor. A trap inside the child surfaces at
            // the spawn site either way, carrying the child's name.
            ast::ExprKind::Spawn { path, args } => {
                let segments: Vec<String> = path.segments.iter().map(|s| s.name.clone()).collect();
                let shown = segments.join(".");
                let callee = if let [single] = segments.as_slice() {
                    self.fn_value(self.current, single, path.span)?
                } else {
                    match self.runtime_ref(&segments) {
                        Some(RuntimeRef::Fn(home, bare)) => {
                            self.fn_value(home, &bare, path.span)?
                        }
                        _ => return trap(path.span, format!("no function `{shown}` at runtime")),
                    }
                };
                let mut evaluated = Vec::with_capacity(args.len());
                for arg in args {
                    evaluated.push(self.eval(&arg.value, env)?);
                }
                if self.pool.is_some() && !self.regions.is_empty() {
                    return self.submit_child(callee, evaluated, shown, expr.span);
                }
                match self.apply(callee, evaluated, expr.span) {
                    Ok(value) => Ok(Value::Task(Arc::new(value))),
                    Err(Control::Trap { message, .. }) => Err(Control::Trap {
                        message: format!("task `{shown}` trapped: {message}"),
                        span: expr.span,
                    }),
                    Err(other) => Err(other),
                }
            }

            ast::ExprKind::Try(inner) => {
                let value = self.eval(inner, env)?;
                match value {
                    Value::Enum {
                        def,
                        variant,
                        payload,
                    } if def == self.table.result => {
                        if variant == ok_index() {
                            Ok(owned(payload).remove(0))
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
                        payload,
                    } if def == self.table.option => {
                        if variant == some_index() {
                            Ok(owned(payload).remove(0))
                        } else {
                            Err(Control::Return(Value::enumeration(
                                def,
                                none_index(),
                                Vec::new(),
                            )))
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
                // XN5001 refuses non-exhaustive matches, and `run` refuses
                // files with diagnostics — so this is a checker gap, reported
                // as one rather than as undefined behaviour.
                trap(
                    expr.span,
                    "no `match` arm matched — XN5001 should have refused this program; checker gap",
                )
            }

            ast::ExprKind::Block(block) => self.block(block, env),

            ast::ExprKind::ListLit(elements) => {
                let mut items = Vec::with_capacity(elements.len());
                for element in elements {
                    items.push(self.eval(element, env)?);
                }
                Ok(Value::list(items))
            }

            ast::ExprKind::StructLit { path, fields } => {
                let shown = path
                    .segments
                    .iter()
                    .map(|s| s.name.as_str())
                    .collect::<Vec<_>>()
                    .join(".");
                // Items are single segments, so a dotted spelling is exactly
                // the table key; a bare one resolves like every bare name.
                let def = if path.segments.len() == 1 {
                    self.lookup_type(&shown)
                } else {
                    self.table.lookup(&shown)
                };
                let Some(def) = def else {
                    return trap(expr.span, format!("`{shown}` is not a struct"));
                };
                let DefKind::Struct {
                    fields: declared, ..
                } = &self.table.def(def).kind
                else {
                    return trap(expr.span, format!("`{shown}` is not a struct"));
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
                Ok(Value::structure(def, ordered))
            }

            // Creation-time snapshot (design/0014 §2): the closure copies
            // everything visible, once, here. The checker already guaranteed
            // that what the body actually touches is CaptureSafe and not a
            // `var`, so copying the superset is observationally identical.
            ast::ExprKind::Lambda { params, body } => Ok(Value::Fn {
                params: Arc::new(params.iter().map(|p| p.name.name.clone()).collect()),
                body: Body::Expr(body),
                captured: Arc::new(env.snapshot()),
                is_async: false,
                home: self.current,
            }),

            ast::ExprKind::Error => trap(expr.span, "cannot run code the parser could not read"),
        }
    }

    // ----- names -----

    pub(super) fn path_value(
        &mut self,
        path: &'a ast::Path,
        span: Span,
        env: &mut Env<'a>,
    ) -> Eval<'a, Value<'a>> {
        let name = &path.segments[0].name;
        if let Some(value) = env.get(name) {
            return Ok(value.clone());
        }
        if find_const(self.current_module(), name).is_some() {
            return self.const_value(self.current, name, span);
        }
        if let Some(f) = find_fn(self.current_module(), name) {
            return Ok(Value::Fn {
                params: Arc::new(f.params.iter().map(|p| p.name.name.clone()).collect()),
                body: match &f.body {
                    Some(body) => Body::Block(body),
                    None => return trap(span, format!("`{name}` has no body")),
                },
                captured: Arc::new(Vec::new()),
                is_async: f.is_async,
                home: self.current,
            });
        }
        if let Some((def, variant)) = self.table.unqualified_variant(name) {
            let index = self.variant_index(def, &variant.name);
            if variant.payload.is_empty() {
                return Ok(Value::enumeration(def, index, Vec::new()));
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

    pub(super) fn variant_ref(
        &mut self,
        def: DefId,
        variant_name: &str,
        span: Span,
    ) -> Eval<'a, Value<'a>> {
        let Some(variant) = self.table.variant_named(def, variant_name) else {
            return trap(span, format!("no variant `{variant_name}`"));
        };
        let index = self.variant_index(def, variant_name);
        if variant.payload.is_empty() {
            Ok(Value::enumeration(def, index, Vec::new()))
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
            Value::Struct { def, fields } => {
                let DefKind::Struct {
                    fields: declared, ..
                } = &self.table.def(def).kind
                else {
                    return trap(span, "not a struct");
                };
                match declared.iter().position(|f| f.name == field) {
                    // D1: reading a field yields an independent value.
                    Some(index) => Ok(owned(fields).remove(index)),
                    None => trap(span, format!("no field `{field}`")),
                }
            }
            _ => trap(span, format!("`{field}` is not a field of this value")),
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
                // Enum.Variant, possibly module-qualified.
                if path.segments.len() < 2 {
                    return Ok(false);
                }
                let variant_ident = path.segments.last().expect("two or more segments");
                let type_name = path.segments[..path.segments.len() - 1]
                    .iter()
                    .map(|s| s.name.as_str())
                    .collect::<Vec<_>>()
                    .join(".");
                let def = if path.segments.len() == 2 {
                    self.lookup_type(&type_name)
                } else {
                    self.table.lookup(&type_name)
                };
                let Some(def) = def else {
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
                let Some(last) = path.segments.last() else {
                    return Ok(false);
                };
                let variant_name = &last.name;
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
