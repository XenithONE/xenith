use xenith_diag::{DiagCode, Diagnostic, Edit, Fix, Span, TASK_PLAN_TEACH};
use xenith_syntax::ast;

use crate::def::{DefKind, Property};
use crate::ty::Type;

use super::patterns::pattern_names;
use super::resolve::QualifiedLookup;
use super::{Binding, Checker, JoinState};

impl<'a> Checker<'a> {
    /// XN5001: every value the scrutinee can hold must land on some arm.
    /// The witness is a concrete value no arm covers, in source syntax.
    fn check_exhaustiveness(&mut self, scrutinee: &Type, arms: &[ast::MatchArm], span: Span) {
        if let Some(found) = crate::exhaustive::missing_witness(self.defs, scrutinee, arms) {
            let message = format!("this `match` is not exhaustive: `{found}` is not covered");
            self.error(DiagCode::NonExhaustiveMatch, span, message);
        }
    }

    // ----- function body -----

    pub(super) fn check_fn(&mut self) {
        // Parsed for recovery, not shipped (design/0008 §1): `async` has no
        // effect rules yet, so an `async fn` cannot be checked honestly.
        if self.fn_ast.is_async {
            self.error(
                DiagCode::UnshippedConstruct,
                self.sig.name_span,
                "`async fn` is not part of the language yet; declare a plain `fn`",
            );
        }

        // Holes in the signature itself: collection lowered them to
        // placeholders; their goals are recorded here.
        for param in &self.fn_ast.params {
            self.type_goals_in(&param.ty);
        }
        if let Some(ret) = &self.fn_ast.return_type {
            self.type_goals_in(ret);
        }

        for (name, ty) in &self.sig.params {
            // Parameters are immutable; mutation goes through `var` rebinding.
            self.scopes[0].push(Binding {
                name: name.clone(),
                ty: ty.clone(),
                mutable: false,
                join: None,
            });
        }
        let Some(body) = &self.fn_ast.body else {
            return;
        };
        let ret = self.sig.ret.clone();
        self.check_block(body, &ret);
    }

    /// Check a block against an expected type: statements first, then the
    /// tail expression carries the value.
    fn check_block(&mut self, block: &ast::Block, expected: &Type) {
        self.scoped(|this| {
            for stmt in &block.stmts {
                this.stmt(stmt);
            }
            match &block.tail {
                Some(tail) => this.check(tail, expected),
                None => {
                    if !expected.is_compatible_with(&Type::Unit) {
                        // A trailing `return` already satisfied the type.
                        let diverges = matches!(
                            block.stmts.last().map(|s| &s.kind),
                            Some(ast::StmtKind::Return(_))
                        );
                        if !diverges {
                            let message = format!(
                                "this block must produce `{}`, but ends without a value; \
                                 add a tail expression (no trailing `;`)",
                                this.render(expected)
                            );
                            this.error(DiagCode::TypeMismatch, block.span, message);
                        }
                    }
                }
            }
            this.enforce_join_exit(block);
        });
    }

    fn stmt(&mut self, stmt: &ast::Stmt) {
        match &stmt.kind {
            ast::StmtKind::Let {
                pattern,
                ty,
                init,
                mutable,
            } => {
                // `let j = spawn f(..);` — the one binding form for a task
                // handle (design/0015 §4).
                if let ast::ExprKind::Spawn { path, args } = &init.kind {
                    self.spawn_binding(pattern, ty.as_ref(), path, args, init.span, *mutable);
                    return;
                }
                let declared = ty.as_ref().map(|t| self.lower(t));
                // While the initializer runs, its own names have no value
                // yet. A closure created in it that mentions one is XN4007 —
                // definite initialization (design/0014 §2).
                let mut names = Vec::new();
                pattern_names(pattern, &mut names);
                let saved = std::mem::replace(&mut self.initializing, names);
                let value_ty = match &declared {
                    Some(expected) => {
                        self.check(init, expected);
                        expected.clone()
                    }
                    None => {
                        let ty = self.synth(init);
                        if matches!(init.kind, ast::ExprKind::Hole { .. }) {
                            // synth() already reported AnnotationRequired and
                            // recorded no goal; nothing more to add here.
                        }
                        ty
                    }
                };
                self.initializing = saved;
                self.bind_pattern(pattern, &value_ty, *mutable);
            }
            ast::StmtKind::Expr(expr) => {
                // `spawn f(..);` — the fire-and-forget statement form, for
                // children with nothing to hand back (design/0015 §4).
                if let ast::ExprKind::Spawn { path, args } = &expr.kind {
                    if let Some(result) = self.spawn_check(path, args, expr.span) {
                        // No handle to consume: this child is joined by the
                        // scope's exit, and keeps the region in flight until
                        // then (design/0017 §3).
                        self.note_spawned(None);
                        if !matches!(result, Type::Unit) && !result.is_unknown() {
                            let spelled = path
                                .segments
                                .iter()
                                .map(|s| s.name.as_str())
                                .collect::<Vec<_>>()
                                .join(".");
                            let message = format!(
                                "`{spelled}` returns `{}`; the statement form \
                                 discards it — bind the result: \
                                 `let j = spawn {spelled}(..);` and await `j`",
                                self.render(&result)
                            );
                            self.diagnostics.push(
                                Diagnostic::error(
                                    DiagCode::SpawnStatementNotUnit,
                                    expr.span,
                                    message,
                                )
                                .with_teach_note(format!("; {TASK_PLAN_TEACH}")),
                            );
                        }
                    }
                    return;
                }
                // Value discarded; no unused-result lint yet.
                let _ = self.synth(expr);
            }
            ast::StmtKind::Return(value) => {
                if !self.closures.is_empty() {
                    // A closure has no function to return from (design/0014
                    // §3). The operand is still walked for its own problems.
                    self.closure_early_exit(stmt.span, "`return`");
                    if let Some(value) = value {
                        let _ = self.synth(value);
                    }
                    return;
                }
                let ret = self.sig.ret.clone();
                match value {
                    Some(value) => self.check(value, &ret),
                    None => {
                        // Parser already reported the missing operand.
                    }
                }
            }
            ast::StmtKind::Break | ast::StmtKind::Continue => {
                // Legal inside a loop the closure itself contains; crossing
                // the closure boundary is not (design/0014 §3).
                if let Some(closure) = self.closures.last() {
                    if self.loop_depth == closure.entry_loop_depth {
                        let spelled = if matches!(stmt.kind, ast::StmtKind::Break) {
                            "`break`"
                        } else {
                            "`continue`"
                        };
                        self.closure_early_exit(stmt.span, spelled);
                    }
                }
            }
            ast::StmtKind::While { cond, body } => {
                // The condition re-runs every iteration, so for the
                // exactly-once dataflow it counts as inside the loop too.
                self.loop_depth += 1;
                self.check(cond, &Type::Bool);
                self.check_block(body, &Type::Unit);
                self.loop_depth -= 1;
            }
            ast::StmtKind::For {
                pattern,
                iter,
                body,
            } => {
                // Parsed for recovery, not shipped (design/0008 §1; iteration
                // is a future RFC). One diagnostic for the construct; the
                // body is still walked so its own problems and goals survive.
                self.error(
                    DiagCode::UnshippedConstruct,
                    stmt.span,
                    "`for` is not part of the language yet — iterate with \
                     `while` + `len()` + `get(index:)`",
                );
                let iter_ty = self.synth(iter);
                let element = match &iter_ty {
                    Type::Named { def, args } if *def == self.defs.list => args[0].clone(),
                    _ => Type::Error,
                };
                self.scoped(|this| {
                    this.bind_pattern(pattern, &element, false);
                    this.check_block(body, &Type::Unit);
                });
            }
            ast::StmtKind::Error => {}
        }
    }

    // ----- the two judgements -----

    /// Push `expected` into the expression. This is where holes become goals:
    /// the required type is simply present.
    pub(super) fn check(&mut self, expr: &ast::Expr, expected: &Type) {
        // For `type-at`: a checked expression's type is what was required of
        // it. Inner expressions overwrite this with something smaller.
        self.maybe_probe(expr.span, expected);
        match &expr.kind {
            ast::ExprKind::Hole { name } => {
                self.fresh_hole();
                self.push_goal(name.clone(), expr.span, "expr", expected);
            }

            ast::ExprKind::If {
                cond,
                then_block,
                else_branch,
            } => {
                self.check(cond, &Type::Bool);
                let before = self.join_snapshot();
                self.check_block(then_block, expected);
                let after_then = self.join_snapshot();
                self.restore_joins(&before);
                match else_branch {
                    Some(branch) => self.check(branch, expected),
                    None => {
                        if !expected.is_compatible_with(&Type::Unit) {
                            let message = format!(
                                "an `if` used as a value of `{}` needs an `else` branch",
                                self.render(expected)
                            );
                            self.error(DiagCode::TypeMismatch, expr.span, message);
                        }
                    }
                }
                let after_else = self.join_snapshot();
                self.merge_joins(&before, &[after_then, after_else], expr.span);
            }

            ast::ExprKind::Match { scrutinee, arms } => {
                let scrutinee_ty = self.synth(scrutinee);
                let before = self.join_snapshot();
                let mut arm_states = Vec::new();
                for arm in arms {
                    self.restore_joins(&before);
                    self.scoped(|this| {
                        this.bind_pattern(&arm.pattern, &scrutinee_ty, false);
                        if let Some(guard) = &arm.guard {
                            let saved = std::mem::replace(&mut this.in_guard, true);
                            this.check(guard, &Type::Bool);
                            this.in_guard = saved;
                        }
                        this.check(&arm.body, expected);
                    });
                    arm_states.push(self.join_snapshot());
                }
                self.merge_joins(&before, &arm_states, expr.span);
                self.check_exhaustiveness(&scrutinee_ty, arms, expr.span);
            }

            ast::ExprKind::Block(block) => self.check_block(block, expected),

            // The task region (design/0015 §1): spawning becomes legal
            // inside, and every handle bound inside is consumed inside.
            ast::ExprKind::Scope(block) => {
                if !self.closures.is_empty() {
                    self.task_in_closure("`scope`", expr.span);
                    self.check_block(block, expected);
                } else {
                    self.in_scope_region(|this| this.check_block(block, expected));
                }
            }

            // A closure fits exactly one shape of expectation: a `fn(..)`
            // type, which only argument positions push (design/0014 §3).
            // Anything else concrete is the position rule, phrased for the
            // type that was wanted; poison stays silent.
            ast::ExprKind::Lambda { params, body } => match expected {
                Type::Fn {
                    params: param_types,
                    ret,
                    ..
                } => {
                    let param_types = param_types.clone();
                    let ret = (**ret).clone();
                    self.check_lambda(params, body, &param_types, &ret, expr.span, &mut Vec::new());
                }
                _ if expected.is_unknown() => {}
                other => {
                    let message = format!(
                        "a closure cannot produce `{}`; a closure is written only \
                         as a call argument for a `fn(..)` parameter — inline its \
                         body, or extract a named fn",
                        self.render(other)
                    );
                    self.error(DiagCode::ClosureOutsideCall, expr.span, message);
                }
            },

            // A list literal seeds its elements from the expected type:
            // `let xs: List<Int> = [];` is complete with no annotation on the
            // literal itself (design/0007 §3).
            ast::ExprKind::ListLit(elements) => match expected {
                Type::Named { def, args } if *def == self.defs.list => {
                    let element_ty = args[0].clone();
                    for element in elements {
                        self.check(element, &element_ty);
                    }
                }
                _ if expected.is_unknown() => {
                    for element in elements {
                        let _ = self.synth(element);
                    }
                }
                other => {
                    // Not a List position at all: one mismatch on the literal.
                    // The elements still get checked so their own problems and
                    // goals survive.
                    if let Some(first) = elements.first() {
                        let first_ty = self.synth(first);
                        for element in &elements[1..] {
                            self.check(element, &first_ty);
                        }
                        let found = Type::Named {
                            def: self.defs.list,
                            args: vec![first_ty],
                        };
                        self.require_compatible(&found, other, expr.span);
                    } else {
                        let message = format!(
                            "expected `{}`, found an empty list literal",
                            self.render(other)
                        );
                        self.error(DiagCode::TypeMismatch, expr.span, message);
                    }
                }
            },

            // A bare unit variant takes its enum's arguments from the expected
            // type, exactly as constructor calls do: `let o: Option<Int> =
            // None;` needs no further annotation.
            ast::ExprKind::Path(path) => {
                if let [single] = path.segments.as_slice() {
                    if self.lookup(&single.name).is_none() {
                        if let Some((def, variant)) = self.defs.unqualified_variant(&single.name) {
                            if variant.payload.is_empty() {
                                if let Type::Named {
                                    def: expected_def, ..
                                } = expected
                                {
                                    if *expected_def == def {
                                        return;
                                    }
                                }
                            }
                        }
                    }
                }
                let found = self.synth(expr);
                self.require_compatible(&found, expected, expr.span);
            }

            // A user struct literal takes its type arguments from the
            // expected type, exactly as constructor calls do:
            // `let p: Pair<Int> = Pair { a: 1, b: 2 };` binds `T` with no
            // further annotation.
            ast::ExprKind::StructLit { path, fields } => {
                let ty = self.struct_lit(path, fields, expr.span, Some(expected));
                self.maybe_probe(expr.span, &ty);
                self.require_compatible(&ty, expected, expr.span);
            }

            // `Wrap.Hollow` — a payload-less variant of a generic enum reads
            // the enum's arguments from context, the same seeding `None`
            // already gets. Ordinary field access is unaffected: only
            // `variant_ref` consults the expectation.
            ast::ExprKind::Field { receiver, name } => {
                let ty = self.field(receiver, name, expr.span, Some(expected));
                self.maybe_probe(expr.span, &ty);
                self.require_compatible(&ty, expected, expr.span);
            }

            // Constructors gain their type parameters from the expected type:
            // `check(Ok(x), Result<Player, ScoreError>)` binds T and E with no
            // annotation. This is the payoff of bidirectionality.
            ast::ExprKind::Call { callee, args } => {
                let ty = self.call(callee, args, Some(expected), expr.span);
                self.require_compatible(&ty, expected, expr.span);
            }

            // Qualified variant construction parses as a method call; in
            // checking position it deserves the expected type too, so a
            // generic enum's parameters come from context.
            ast::ExprKind::MethodCall {
                receiver,
                method,
                args,
            } => {
                let ty = if let Some(found) =
                    self.try_qualified_call(receiver, method, args, Some(expected), expr.span)
                {
                    found
                } else {
                    match self.qualified_variant_target(receiver, &method.name) {
                        Some(def) => {
                            self.call_variant(def, &method.name, args, Some(expected), expr.span)
                        }
                        None => self.method_call(receiver, method, args, expr.span),
                    }
                };
                self.require_compatible(&ty, expected, expr.span);
            }

            _ => {
                let found = self.synth(expr);
                self.require_compatible(&found, expected, expr.span);
            }
        }
    }

    /// Read a type out of the expression with nothing pushed down.
    pub(super) fn synth(&mut self, expr: &ast::Expr) -> Type {
        let ty = self.synth_inner(expr);
        self.maybe_probe(expr.span, &ty);
        ty
    }

    fn synth_inner(&mut self, expr: &ast::Expr) -> Type {
        match &expr.kind {
            ast::ExprKind::Int(_) => Type::Int,
            ast::ExprKind::Float(_) => Type::Float,
            ast::ExprKind::Str(_) => Type::Str,
            ast::ExprKind::Char(_) => Type::Char,
            ast::ExprKind::Bool(_) => Type::Bool,
            ast::ExprKind::Unit => Type::Unit,

            // A hole with no expected type is a hard error, not an inference
            // variable. Local-only inference means we refuse to invent the
            // type from thin air (design/0006 §1-1) — the diagnostic asks for
            // the annotation instead.
            ast::ExprKind::Hole { name } => {
                let shown = name.as_deref().unwrap_or("");
                self.error(
                    DiagCode::AnnotationRequired,
                    expr.span,
                    format!(
                        "nothing determines the type of `??{shown}` here; \
                         annotate the surrounding binding or position"
                    ),
                );
                Type::Error
            }

            ast::ExprKind::Path(path) => self.synth_path(path, expr.span),

            ast::ExprKind::Unary { op, operand } => {
                let ty = self.synth(operand);
                match op {
                    ast::UnaryOp::Neg => {
                        if !matches!(ty, Type::Int | Type::Float | Type::Error | Type::Hole(_)) {
                            let message =
                                format!("`-` needs `Int` or `Float`, found `{}`", self.render(&ty));
                            self.error(DiagCode::TypeMismatch, operand.span, message);
                            return Type::Error;
                        }
                        ty
                    }
                    ast::UnaryOp::Not => {
                        self.require_compatible(&ty, &Type::Bool, operand.span);
                        Type::Bool
                    }
                }
            }

            ast::ExprKind::Binary { op, lhs, rhs } => self.binary(*op, lhs, rhs, expr.span),

            ast::ExprKind::Assign { target, op, value } => {
                self.assign(target, *op, value);
                Type::Unit
            }

            ast::ExprKind::Call { callee, args } => self.call(callee, args, None, expr.span),

            ast::ExprKind::MethodCall {
                receiver,
                method,
                args,
            } => self.method_call(receiver, method, args, expr.span),

            ast::ExprKind::Field { receiver, name } => self.field(receiver, name, expr.span, None),

            ast::ExprKind::Await(inner) => {
                if !self.closures.is_empty() {
                    self.task_in_closure("`.await`", expr.span);
                    // Poison the handle it names so the scope end does not
                    // stack an unawaited report on this one.
                    if let ast::ExprKind::Path(path) = &inner.kind {
                        if let [single] = path.segments.as_slice() {
                            let found = self.lookup(&single.name).and_then(|b| b.join);
                            if let Some(index) = found {
                                self.joins[index].state = JoinState::Poisoned;
                            }
                        }
                    }
                    return Type::Error;
                }
                // One canonical spelling: bind, then await. Awaiting the
                // spawn inline would be a second spelling of the same
                // program (design/0015 §4).
                if let ast::ExprKind::Spawn { args, .. } = &inner.kind {
                    return self.spawn_escape(
                        args,
                        expr.span,
                        "bind the result of `spawn` with `let`, then await the \
                         binding: `let j = spawn f(..); j.await`",
                    );
                }
                // The design/0015 carve-out: `.await` moves a task handle's
                // result out, exactly once.
                if let ast::ExprKind::Path(path) = &inner.kind {
                    if let [single] = path.segments.as_slice() {
                        let found = self.lookup(&single.name).and_then(|b| b.join);
                        if let Some(index) = found {
                            return self.consume_join(index, expr.span);
                        }
                    }
                }
                // Everything else stays out of the language (design/0008
                // §1). The operand is still synthesised so its own problems
                // and goals survive; poison stays silent, so an await of an
                // already-refused spawn binding reports once, not twice.
                let inner_ty = self.synth(inner);
                if inner_ty.is_unknown() {
                    return Type::Error;
                }
                self.error(
                    DiagCode::UnshippedConstruct,
                    expr.span,
                    "`.await` applies only to a task handle bound by \
                     `let j = spawn f(..);` inside `scope`",
                );
                Type::Error
            }

            ast::ExprKind::Try(inner) => self.try_op(inner, expr.span),

            ast::ExprKind::If {
                cond,
                then_block,
                else_branch,
            } => {
                // Without context, an if-value takes the then-branch's type.
                self.check(cond, &Type::Bool);
                let before = self.join_snapshot();
                let then_ty = self.synth_block(then_block);
                let after_then = self.join_snapshot();
                self.restore_joins(&before);
                let out = if let Some(branch) = else_branch {
                    self.check(branch, &then_ty);
                    then_ty
                } else {
                    Type::Unit
                };
                let after_else = self.join_snapshot();
                self.merge_joins(&before, &[after_then, after_else], expr.span);
                out
            }

            ast::ExprKind::Match { scrutinee, arms } => {
                let scrutinee_ty = self.synth(scrutinee);
                let before = self.join_snapshot();
                let mut result: Option<Type> = None;
                let mut arm_states = Vec::new();
                for arm in arms {
                    self.restore_joins(&before);
                    let expected = result.clone();
                    match expected {
                        Some(ty) => self.scoped(|this| {
                            this.bind_pattern(&arm.pattern, &scrutinee_ty, false);
                            if let Some(guard) = &arm.guard {
                                let saved = std::mem::replace(&mut this.in_guard, true);
                                this.check(guard, &Type::Bool);
                                this.in_guard = saved;
                            }
                            this.check(&arm.body, &ty);
                        }),
                        None => {
                            // The first arm names the type for the rest.
                            let ty = self.scoped(|this| {
                                this.bind_pattern(&arm.pattern, &scrutinee_ty, false);
                                if let Some(guard) = &arm.guard {
                                    let saved = std::mem::replace(&mut this.in_guard, true);
                                    this.check(guard, &Type::Bool);
                                    this.in_guard = saved;
                                }
                                this.synth(&arm.body)
                            });
                            result = Some(ty);
                        }
                    }
                    arm_states.push(self.join_snapshot());
                }
                self.merge_joins(&before, &arm_states, expr.span);
                self.check_exhaustiveness(&scrutinee_ty, arms, expr.span);
                result.unwrap_or(Type::Unit)
            }

            ast::ExprKind::Block(block) => self.synth_block(block),

            // The task region (design/0015 §1). Its value is the block's
            // tail — a scope is not a first-class thing, just a region.
            ast::ExprKind::Scope(block) => {
                if !self.closures.is_empty() {
                    self.task_in_closure("`scope`", expr.span);
                    return self.synth_block(block);
                }
                self.in_scope_region(|this| this.synth_block(block))
            }

            // A spawn anywhere other than `let j = spawn f(..);` or the
            // statement form is the handle escaping (design/0015 §4).
            ast::ExprKind::Spawn { args, .. } => {
                if !self.closures.is_empty() {
                    self.task_in_closure("`spawn`", expr.span);
                    for arg in args {
                        let _ = self.synth(&arg.value);
                    }
                    return Type::Error;
                }
                self.spawn_escape(
                    args,
                    expr.span,
                    "the result of `spawn` must be bound with `let` and \
                     awaited: `let j = spawn f(..); j.await`",
                )
            }

            // With nothing pushed down, the first element names the element
            // type and the rest are checked against it. An empty literal has
            // nothing to read a type from — same refusal as a bare hole
            // (design/0006 §1-1).
            ast::ExprKind::ListLit(elements) => match elements.first() {
                Some(first) => {
                    let element_ty = self.synth(first);
                    for element in &elements[1..] {
                        self.check(element, &element_ty);
                    }
                    Type::Named {
                        def: self.defs.list,
                        args: vec![element_ty],
                    }
                }
                None => {
                    self.error(
                        DiagCode::AnnotationRequired,
                        expr.span,
                        "nothing determines the element type of `[]` here; \
                         annotate the surrounding binding",
                    );
                    Type::Error
                }
            },

            ast::ExprKind::StructLit { path, fields } => {
                self.struct_lit(path, fields, expr.span, None)
            }

            // A closure with no `fn(..)` expectation has nowhere to take its
            // parameter types from — and nowhere legal to be (design/0014
            // §3: argument position only; no `let`, no `return`, no fields,
            // no containers). One diagnostic; the body is not descended
            // into, because without parameter types every name in it would
            // cascade.
            ast::ExprKind::Lambda { .. } => {
                self.error(
                    DiagCode::ClosureOutsideCall,
                    expr.span,
                    "a closure is written only as a call argument for a `fn(..)` \
                     parameter; inline its body, or extract a named fn",
                );
                Type::Error
            }

            ast::ExprKind::Error => Type::Error,
        }
    }

    fn synth_block(&mut self, block: &ast::Block) -> Type {
        self.scoped(|this| {
            for stmt in &block.stmts {
                this.stmt(stmt);
            }
            let ty = match &block.tail {
                Some(tail) => this.synth(tail),
                None => Type::Unit,
            };
            this.enforce_join_exit(block);
            ty
        })
    }

    // ----- operators -----

    fn binary(&mut self, op: ast::BinaryOp, lhs: &ast::Expr, rhs: &ast::Expr, span: Span) -> Type {
        use ast::BinaryOp as B;
        match op {
            B::Add | B::Sub | B::Mul | B::Div | B::Rem => {
                let left = self.synth(lhs);
                self.check(rhs, &left);
                match left {
                    Type::Int | Type::Float | Type::Error | Type::Hole(_) => left,
                    other => {
                        let message = format!(
                            "`{}` needs `Int` or `Float` operands, found `{}`",
                            op.symbol(),
                            self.render(&other)
                        );
                        self.error(DiagCode::TypeMismatch, lhs.span, message);
                        Type::Error
                    }
                }
            }
            B::BitAnd | B::BitOr | B::BitXor | B::Shl | B::Shr => {
                self.check(lhs, &Type::Int);
                self.check(rhs, &Type::Int);
                Type::Int
            }
            B::And | B::Or => {
                self.check(lhs, &Type::Bool);
                self.check(rhs, &Type::Bool);
                Type::Bool
            }
            B::Eq | B::Ne => {
                let left = self.synth(lhs);
                self.check(rhs, &left);
                // `==` is structural equality and demands the sealed property.
                // The operator table and the property agree here — unlike
                // ordering, where Float compares but cannot be a sort key.
                if !self
                    .defs
                    .has_property(&left, Property::Eq, &self.sig.generics)
                {
                    let message = format!(
                        "`{}` cannot be compared with `{}`; `Eq` is not satisfied{}",
                        self.render(&left),
                        op.symbol(),
                        if matches!(&left, Type::Named { def, .. } if *def == self.defs.shared) {
                            " — compare `Shared` identity with `is`"
                        } else {
                            ""
                        }
                    );
                    self.error(DiagCode::PropertyNotSatisfied, span, message);
                }
                Type::Bool
            }
            B::Lt | B::Le | B::Gt | B::Ge => {
                let left = self.synth(lhs);
                self.check(rhs, &left);
                // Comparison *operators* accept Float (IEEE partial order is
                // well-defined at a use site); the sealed property `Ord` —
                // sort keys, Map keys, `T: Ord` bounds — still excludes it.
                let comparable = matches!(
                    left,
                    Type::Int | Type::Float | Type::Char | Type::Str | Type::Error | Type::Hole(_)
                ) || self.defs.has_property(
                    &left,
                    Property::Ord,
                    &self.sig.generics,
                );
                if !comparable {
                    let message = format!(
                        "`{}` cannot be ordered with `{}`",
                        self.render(&left),
                        op.symbol()
                    );
                    self.error(DiagCode::PropertyNotSatisfied, span, message);
                }
                Type::Bool
            }
            B::Identity => {
                let left = self.synth(lhs);
                self.check(rhs, &left);
                let is_shared = matches!(&left, Type::Named { def, .. } if *def == self.defs.shared)
                    || left.is_unknown();
                if !is_shared {
                    let message = format!(
                        "`is` compares the identity of `Shared` values; `{}` is a value type — use `==`",
                        self.render(&left)
                    );
                    self.error(DiagCode::TypeMismatch, span, message);
                }
                Type::Bool
            }
        }
    }

    fn assign(&mut self, target: &ast::Expr, op: Option<ast::BinaryOp>, value: &ast::Expr) {
        // Mutability: the root binding must be `var`, and a field written
        // through must itself be `var` in its struct.
        match &target.kind {
            ast::ExprKind::Path(path) => {
                if let [single] = path.segments.as_slice() {
                    if let Some(binding) = self.lookup(&single.name) {
                        if !binding.mutable {
                            let message = format!(
                                "`{}` is a `let` binding; declare it with `var` to assign to it",
                                single.name
                            );
                            self.error(DiagCode::AssignmentToImmutable, target.span, message);
                        }
                    }
                }
            }
            ast::ExprKind::Field { receiver, name } => {
                if let Type::Named { def, .. } = self.synth(receiver) {
                    if let Some(owner) = self.foreign_owner(def) {
                        // Writes stop at the module boundary, `var` field or
                        // not (design/0010 §4); mutation goes through the
                        // owner's pub API.
                        let message = format!(
                            "field `{}` of `{}` cannot be assigned from outside `{owner}`",
                            name.name,
                            self.defs.name_of(def)
                        );
                        self.error(DiagCode::CrossModuleAssignment, name.span, message);
                    } else if let DefKind::Struct { fields } = &self.defs.def(def).kind {
                        if let Some(field) = fields.iter().find(|f| f.name == name.name) {
                            if !field.mutable {
                                let message = format!(
                                    "field `{}` is immutable; declare it `var {}: ..` in the struct",
                                    name.name, name.name
                                );
                                self.error(DiagCode::AssignmentToImmutable, name.span, message);
                            }
                        }
                    }
                }
                if let ast::ExprKind::Path(path) = &receiver.kind {
                    if let [single] = path.segments.as_slice() {
                        if let Some(binding) = self.lookup(&single.name) {
                            if !binding.mutable {
                                let message = format!(
                                    "`{}` is a `let` binding; declare it with `var` to assign through it",
                                    single.name
                                );
                                self.error(DiagCode::AssignmentToImmutable, receiver.span, message);
                            }
                        }
                    }
                }
            }
            _ => {
                self.error(
                    DiagCode::TypeMismatch,
                    target.span,
                    "this expression cannot be assigned to",
                );
            }
        }

        let target_ty = self.synth(target);
        if let Some(op) = op {
            // Compound assignment is arithmetic; the operands must be numeric.
            if !matches!(
                target_ty,
                Type::Int | Type::Float | Type::Error | Type::Hole(_)
            ) {
                let message = format!(
                    "`{}=` needs `Int` or `Float`, found `{}`",
                    op.symbol(),
                    self.render(&target_ty)
                );
                self.error(DiagCode::TypeMismatch, target.span, message);
            }
        }
        self.check(value, &target_ty);
    }

    fn try_op(&mut self, inner: &ast::Expr, span: Span) -> Type {
        if !self.closures.is_empty() {
            // `?` is an early return, and a closure has no caller of its own
            // to return to (design/0014 §3). The operand is still walked.
            let _ = self.synth(inner);
            self.closure_early_exit(span, "`?`");
            return Type::Error;
        }
        let ty = self.synth(inner);
        match &ty {
            Type::Error | Type::Hole(_) => Type::Error,
            Type::Named { def, args } if *def == self.defs.result => {
                let (ok, err) = (args[0].clone(), args[1].clone());
                match &self.sig.ret {
                    Type::Named { def, args } if *def == self.defs.result => {
                        self.require_compatible(&err, &args[1], span);
                    }
                    other => {
                        let message = format!(
                            "`?` propagates the error, so `{}` must return `Result<_, {}>`; it returns `{}`",
                            self.sig.name,
                            self.render(&err),
                            self.render(other)
                        );
                        self.error(DiagCode::TypeMismatch, span, message);
                    }
                }
                ok
            }
            Type::Named { def, args } if *def == self.defs.option => {
                match &self.sig.ret {
                    Type::Named { def, .. } if *def == self.defs.option => {}
                    other => {
                        let message = format!(
                            "`?` on an `Option` needs `{}` to return `Option<_>`; it returns `{}`",
                            self.sig.name,
                            self.render(other)
                        );
                        self.error(DiagCode::TypeMismatch, span, message);
                    }
                }
                args[0].clone()
            }
            other => {
                let message = format!(
                    "`?` needs a `Result` or `Option`, found `{}`",
                    self.render(other)
                );
                self.error(DiagCode::TypeMismatch, inner.span, message);
                Type::Error
            }
        }
    }

    // ----- struct literals -----

    fn struct_lit(
        &mut self,
        path: &ast::Path,
        field_inits: &[ast::FieldInit],
        span: Span,
        expected: Option<&Type>,
    ) -> Type {
        let (def, shown) = match path.segments.as_slice() {
            [single] => match self.lookup_type_name(&single.name) {
                Some(def) => (def, single.name.clone()),
                None => {
                    self.error(
                        DiagCode::UnknownType,
                        path.span,
                        format!("`{}` does not name a struct", single.name),
                    );
                    for init in field_inits {
                        let _ = self.synth(&init.value);
                    }
                    return Type::Error;
                }
            },
            segments => {
                // `game.player.Player { .. }` — module path plus item.
                let names: Vec<String> = segments.iter().map(|s| s.name.clone()).collect();
                let dotted = names.join(".");
                match self.qualified_ref(&names, path.span) {
                    QualifiedLookup::Type(def) => (def, dotted),
                    QualifiedLookup::Reported => {
                        for init in field_inits {
                            let _ = self.synth(&init.value);
                        }
                        return Type::Error;
                    }
                    _ => {
                        self.error(
                            DiagCode::UnknownType,
                            path.span,
                            format!("`{dotted}` does not name a struct"),
                        );
                        for init in field_inits {
                            let _ = self.synth(&init.value);
                        }
                        return Type::Error;
                    }
                }
            }
        };
        let name = shown.as_str();
        let info = self.defs.def(def);
        let generics = info.generics.clone();
        let DefKind::Struct { fields } = &info.kind else {
            self.error(
                DiagCode::UnknownType,
                path.span,
                format!("`{name}` is not a struct"),
            );
            return Type::Error;
        };
        let declared: Vec<(String, Type, Span)> = fields
            .iter()
            .map(|f| (f.name.clone(), f.ty.clone(), Span::EMPTY))
            .collect();

        // The expected type seeds the type arguments — the bidirectional
        // payoff constructor calls already take (design/0006 §1), extended
        // to struct literals so `let p: Pair<Int> = Pair { .. };` checks.
        let mut bindings: Vec<(String, Type)> = Vec::new();
        let mut seeded = false;
        if let Some(Type::Named {
            def: expected_def,
            args,
        }) = expected
        {
            if *expected_def == def && args.len() == generics.len() {
                for (generic, ty) in generics.iter().zip(args.iter()) {
                    bindings.push((generic.clone(), ty.clone()));
                }
                seeded = true;
            }
        }
        // Report only where the position offered no expectation at all. A
        // concrete one naming a different type makes this a mismatch the
        // checking caller reports — "annotate the binding" on top of that
        // would name the wrong repair — and a hole or poison expectation is
        // not a mistake to report at all (design/0006 §2).
        if !generics.is_empty() && !seeded && expected.is_none() {
            let undetermined = generics
                .iter()
                .map(|g| format!("`{g}`"))
                .collect::<Vec<_>>()
                .join(", ");
            self.error(
                DiagCode::AnnotationRequired,
                span,
                format!(
                    "nothing here determines {undetermined} of `{name}`; annotate \
                     the surrounding binding, as `let x: {name}<..> = {name} {{ .. }};`"
                ),
            );
        }
        // Whatever stayed unbound becomes poison, so the fields report
        // nothing further: one missing annotation is one diagnostic, not one
        // per field (design/0006 §2).
        for generic in &generics {
            if !bindings.iter().any(|(n, _)| n == generic) {
                bindings.push((generic.clone(), Type::Error));
            }
        }

        // Every declared field, exactly once.
        for (field_name, field_ty, _) in &declared {
            let Some(init) = field_inits.iter().find(|i| i.name.name == *field_name) else {
                let insert_at = span.end.saturating_sub(1);
                self.diagnostics.push(
                    Diagnostic::error(
                        DiagCode::MissingField,
                        span,
                        format!("`{name}` is missing the field `{field_name}`"),
                    )
                    .with_fix(Fix::single(
                        format!("add `{field_name}: ??`"),
                        Edit::insert(insert_at, format!("{field_name}: ??, ")),
                    )),
                );
                continue;
            };
            let concrete = field_ty.substitute(&bindings);
            self.check(&init.value, &concrete);
        }
        for init in field_inits {
            if !declared.iter().any(|(n, _, _)| *n == init.name.name) {
                let message = format!("`{name}` has no field named `{}`", init.name.name);
                self.error(DiagCode::UnknownField, init.name.span, message);
                let _ = self.synth(&init.value);
            }
        }

        Type::Named {
            def,
            args: generics
                .iter()
                .map(|g| {
                    bindings
                        .iter()
                        .find(|(n, _)| n == g)
                        .map(|(_, t)| t.clone())
                        .unwrap_or(Type::Error)
                })
                .collect(),
        }
    }
}
