//! The bidirectional checker, and the hole goals it exists to produce.
//!
//! `check` pushes an expected type down; `synth` reads one back up. The shape
//! is chosen for one reason: in checking mode the required type is present at
//! *every* position, so when a hole appears, its goal — required type, scope,
//! permitted effects — is simply the checker's current state, written down.
//! No separate machinery. See design/0006 §1.
//!
//! Report only between two concrete types. `Type::Error` is poison from an
//! already-reported failure and stays silent; `Type::Hole` is a legal gap and
//! becomes a goal. Conflating those two is how a checker either cascades or
//! goes mute — they are the same for *compatibility* and different for
//! *goal emission*.

use xenith_diag::{DiagCode, Diagnostic, Edit, Fix, Span};
use xenith_syntax::ast;

use crate::def::{self, DefKind, DefTable, FnSig, GenericInfo, Property, UsesInsertion};
use crate::ty::{EffectSet, HoleId, Type, TypeName};

/// Everything `xenith goals` needs about one hole, captured at the moment the
/// checker walked past it. Types are rendered to text immediately so the goal
/// outlives the tables that named them.
#[derive(Clone, Debug)]
pub struct Goal {
    pub name: Option<String>,
    pub span: Span,
    /// `"expr"` or `"type"`.
    pub kind: &'static str,
    pub expected: String,
    pub enclosing_function: String,
    /// Innermost bindings last; shadowed names already removed.
    pub in_scope: Vec<(String, String)>,
    pub allowed_effects: Vec<String>,
}

pub struct Analysis {
    pub diagnostics: Vec<Diagnostic>,
    pub goals: Vec<Goal>,
}

pub fn analyze(module: &ast::Module) -> Analysis {
    let (table, mut diagnostics) = def::collect(module);
    let mut goals = Vec::new();
    let mut next_hole = 0u32;

    for item in &module.items {
        let ast::ItemKind::Fn(f) = &item.kind else {
            continue;
        };
        let Some(sig) = table.fn_named(&f.name.name) else {
            continue;
        };
        let mut checker = Checker {
            defs: &table,
            sig,
            fn_ast: f,
            scopes: vec![Vec::new()],
            diagnostics: &mut diagnostics,
            goals: &mut goals,
            next_hole: &mut next_hole,
        };
        checker.check_fn();
    }

    goals.sort_by_key(|g| g.span.start);
    Analysis { diagnostics, goals }
}

struct Binding {
    name: String,
    ty: Type,
    mutable: bool,
}

struct Checker<'a> {
    defs: &'a DefTable,
    sig: &'a FnSig,
    fn_ast: &'a ast::FnItem,
    scopes: Vec<Vec<Binding>>,
    diagnostics: &'a mut Vec<Diagnostic>,
    goals: &'a mut Vec<Goal>,
    next_hole: &'a mut u32,
}

impl<'a> Checker<'a> {
    // ----- shared plumbing -----

    fn render(&self, ty: &Type) -> String {
        let name_of = |id| self.defs.name_of(id);
        TypeName {
            ty,
            name_of: &name_of,
        }
        .to_string()
    }

    fn error(&mut self, code: DiagCode, span: Span, message: impl Into<String>) {
        self.diagnostics
            .push(Diagnostic::error(code, span, message));
    }

    fn lookup(&self, name: &str) -> Option<&Binding> {
        self.scopes
            .iter()
            .rev()
            .flat_map(|scope| scope.iter().rev())
            .find(|b| b.name == name)
    }

    fn bind(&mut self, name: &str, ty: Type, mutable: bool) {
        if name.is_empty() {
            return; // parser recovery produced an empty ident
        }
        self.scopes
            .last_mut()
            .expect("at least one scope")
            .push(Binding {
                name: name.to_string(),
                ty,
                mutable,
            });
    }

    fn scoped<T>(&mut self, f: impl FnOnce(&mut Self) -> T) -> T {
        self.scopes.push(Vec::new());
        let out = f(self);
        self.scopes.pop();
        out
    }

    fn fresh_hole(&mut self) -> HoleId {
        let id = HoleId(*self.next_hole);
        *self.next_hole += 1;
        id
    }

    /// Snapshot the scope for a goal: innermost occurrence of each name wins.
    fn scope_snapshot(&self) -> Vec<(String, String)> {
        let mut seen = std::collections::HashSet::new();
        let mut out = Vec::new();
        for scope in self.scopes.iter().rev() {
            for binding in scope.iter().rev() {
                if seen.insert(binding.name.clone()) {
                    out.push((binding.name.clone(), self.render(&binding.ty)));
                }
            }
        }
        out.reverse();
        out
    }

    fn push_goal(&mut self, name: Option<String>, span: Span, kind: &'static str, expected: &Type) {
        let rendered = self.render(expected);
        self.push_goal_rendered(name, span, kind, rendered);
    }

    fn push_goal_rendered(
        &mut self,
        name: Option<String>,
        span: Span,
        kind: &'static str,
        expected: String,
    ) {
        let goal = Goal {
            name,
            span,
            kind,
            expected,
            enclosing_function: self.sig.name.clone(),
            in_scope: self.scope_snapshot(),
            allowed_effects: self.sig.effects.iter().map(String::from).collect(),
        };
        self.goals.push(goal);
    }

    /// Record goals for every `??` inside a syntactic type. Signature types
    /// are lowered during collection, before any checker exists, so their
    /// holes are given goals here — where the enclosing function is known.
    fn type_goals_in(&mut self, ty: &ast::Type) {
        match &ty.kind {
            ast::TypeKind::Hole { name } => {
                self.fresh_hole();
                self.push_goal_rendered(name.clone(), ty.span, "type", "<type>".to_string());
            }
            ast::TypeKind::Named { args, .. } => {
                for arg in args {
                    self.type_goals_in(arg);
                }
            }
            ast::TypeKind::Fn { params, ret, .. } => {
                for param in params {
                    self.type_goals_in(param);
                }
                self.type_goals_in(ret);
            }
            ast::TypeKind::Unit | ast::TypeKind::Error => {}
        }
    }

    fn generic_names(&self) -> Vec<String> {
        self.sig.generics.iter().map(|g| g.name.clone()).collect()
    }

    fn lower(&mut self, ty: &ast::Type) -> Type {
        let generics = self.generic_names();
        let lowered = def::lower_type(ty, self.defs, &generics, self.diagnostics);
        if let Type::Hole(_) = lowered {
            // Type-position holes get a real id and a type goal here, where
            // the enclosing function is known.
            let id = self.fresh_hole();
            let name = match &ty.kind {
                ast::TypeKind::Hole { name } => name.clone(),
                _ => None,
            };
            self.push_goal_rendered(name, ty.span, "type", "<type>".to_string());
            return Type::Hole(id);
        }
        lowered
    }

    /// One mismatch, reported once, between concrete types only.
    fn require_compatible(&mut self, found: &Type, expected: &Type, span: Span) {
        if found.is_compatible_with(expected) {
            return;
        }
        let message = format!(
            "expected `{}`, found `{}`",
            self.render(expected),
            self.render(found)
        );
        self.error(DiagCode::TypeMismatch, span, message);
    }

    /// Call-site effect discipline: what the callee performs must fit inside
    /// what this function declared. The fix edits this function's `uses`
    /// clause, because that is the one edit that is mechanically safe.
    fn require_effects(&mut self, needed: &EffectSet, span: Span) {
        let missing = needed.missing_from(&self.sig.effects);
        if missing.is_empty() {
            return;
        }
        let listed = missing.join(", ");
        let mut diagnostic = Diagnostic::error(
            DiagCode::EffectNotPermitted,
            span,
            format!(
                "this call uses {{{listed}}}, which `{}` does not declare",
                self.sig.name
            ),
        );
        let addition = missing.join(", ");
        let fix = match self.sig.uses_insertion {
            UsesInsertion::Extend { before_close } => Some(Fix::single(
                format!("declare `uses {{.., {addition}}}`"),
                Edit::insert(before_close, format!(", {addition}")),
            )),
            UsesInsertion::Fill { before_close } => Some(Fix::single(
                format!("declare `uses {{{addition}}}`"),
                Edit::insert(before_close, addition.clone()),
            )),
            UsesInsertion::Create { before_body } => Some(Fix::single(
                format!("declare `uses {{{addition}}}`"),
                Edit::insert(before_body, format!("uses {{{addition}}} ")),
            )),
            UsesInsertion::Nowhere => None,
        };
        if let Some(fix) = fix {
            diagnostic = diagnostic.with_fix(fix);
        }
        self.diagnostics.push(diagnostic);
    }

    // ----- function body -----

    fn check_fn(&mut self) {
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
                    if expected.is_compatible_with(&Type::Unit) {
                        return;
                    }
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
                let declared = ty.as_ref().map(|t| self.lower(t));
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
                self.bind_pattern(pattern, &value_ty, *mutable);
            }
            ast::StmtKind::Expr(expr) => {
                // Value discarded; no unused-result lint yet.
                let _ = self.synth(expr);
            }
            ast::StmtKind::Return(value) => {
                let ret = self.sig.ret.clone();
                match value {
                    Some(value) => self.check(value, &ret),
                    None => {
                        // Parser already reported the missing operand.
                    }
                }
            }
            ast::StmtKind::Break | ast::StmtKind::Continue => {}
            ast::StmtKind::While { cond, body } => {
                self.check(cond, &Type::Bool);
                self.check_block(body, &Type::Unit);
            }
            ast::StmtKind::For {
                pattern,
                iter,
                body,
            } => {
                let iter_ty = self.synth(iter);
                let element = match &iter_ty {
                    Type::Named { def, args } if *def == self.defs.list => args[0].clone(),
                    Type::Error | Type::Hole(_) => Type::Error,
                    other => {
                        let message = format!(
                            "`for` iterates a `List<T>`; this is `{}`",
                            self.render(other)
                        );
                        self.error(DiagCode::TypeMismatch, iter.span, message);
                        Type::Error
                    }
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
    fn check(&mut self, expr: &ast::Expr, expected: &Type) {
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
                self.check_block(then_block, expected);
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
            }

            ast::ExprKind::Match { scrutinee, arms } => {
                let scrutinee_ty = self.synth(scrutinee);
                for arm in arms {
                    self.scoped(|this| {
                        this.bind_pattern(&arm.pattern, &scrutinee_ty, false);
                        if let Some(guard) = &arm.guard {
                            this.check(guard, &Type::Bool);
                        }
                        this.check(&arm.body, expected);
                    });
                }
            }

            ast::ExprKind::Block(block) => self.check_block(block, expected),

            // Constructors gain their type parameters from the expected type:
            // `check(Ok(x), Result<Player, ScoreError>)` binds T and E with no
            // annotation. This is the payoff of bidirectionality.
            ast::ExprKind::Call { callee, args } => {
                let ty = self.call(callee, args, Some(expected), expr.span);
                self.require_compatible(&ty, expected, expr.span);
            }

            _ => {
                let found = self.synth(expr);
                self.require_compatible(&found, expected, expr.span);
            }
        }
    }

    /// Read a type out of the expression with nothing pushed down.
    fn synth(&mut self, expr: &ast::Expr) -> Type {
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

            ast::ExprKind::Field { receiver, name } => self.field(receiver, name, expr.span),

            ast::ExprKind::Await(inner) => {
                let ty = self.synth(inner);
                match ty {
                    Type::Named { def, mut args } if def == self.defs.task => args.remove(0),
                    Type::Error | Type::Hole(_) => Type::Error,
                    other => {
                        let message = format!(
                            "`.await` needs a `Task<T>`, found `{}`",
                            self.render(&other)
                        );
                        self.error(DiagCode::TypeMismatch, inner.span, message);
                        Type::Error
                    }
                }
            }

            ast::ExprKind::Try(inner) => self.try_op(inner, expr.span),

            ast::ExprKind::If {
                cond,
                then_block,
                else_branch,
            } => {
                // Without context, an if-value takes the then-branch's type.
                self.check(cond, &Type::Bool);
                let then_ty = self.synth_block(then_block);
                if let Some(branch) = else_branch {
                    self.check(branch, &then_ty);
                    then_ty
                } else {
                    Type::Unit
                }
            }

            ast::ExprKind::Match { scrutinee, arms } => {
                let scrutinee_ty = self.synth(scrutinee);
                let mut result: Option<Type> = None;
                for arm in arms {
                    let expected = result.clone();
                    self.scoped(|this| {
                        this.bind_pattern(&arm.pattern, &scrutinee_ty, false);
                        if let Some(guard) = &arm.guard {
                            this.check(guard, &Type::Bool);
                        }
                        if let Some(ty) = &expected {
                            this.check(&arm.body, ty);
                        }
                    });
                    if result.is_none() {
                        // First arm sets the type; scoped() above skipped it.
                        let ty = self.scoped(|this| {
                            this.bind_pattern(&arm.pattern, &scrutinee_ty, false);
                            this.synth(&arm.body)
                        });
                        result = Some(ty);
                    }
                }
                result.unwrap_or(Type::Unit)
            }

            ast::ExprKind::Block(block) => self.synth_block(block),

            ast::ExprKind::StructLit { path, fields } => {
                self.struct_lit(path, fields, expr.span, None)
            }

            ast::ExprKind::Lambda { params, body, .. } => {
                let lowered: Vec<(String, Type)> = params
                    .iter()
                    .map(|p| (p.name.name.clone(), self.lower(&p.ty)))
                    .collect();
                let body_ty = self.scoped(|this| {
                    for (name, ty) in &lowered {
                        this.bind(name, ty.clone(), false);
                    }
                    this.synth(body)
                });
                Type::Fn {
                    params: lowered.into_iter().map(|(_, t)| t).collect(),
                    ret: Box::new(body_ty),
                    // Effects of the lambda body are checked against the
                    // enclosing function's budget at each call site inside it;
                    // the lambda type itself claims none yet. Honest once
                    // effect inference for closures lands (deferred, 0006 §5).
                    effects: EffectSet::empty(),
                }
            }

            ast::ExprKind::Error => Type::Error,
        }
    }

    fn synth_block(&mut self, block: &ast::Block) -> Type {
        self.scoped(|this| {
            for stmt in &block.stmts {
                this.stmt(stmt);
            }
            match &block.tail {
                Some(tail) => this.synth(tail),
                None => Type::Unit,
            }
        })
    }

    // ----- names -----

    fn synth_path(&mut self, path: &ast::Path, span: Span) -> Type {
        let name = match path.segments.as_slice() {
            [single] => single.name.as_str(),
            _ => {
                // The parser only builds multi-segment paths in `use` items
                // and patterns; an expression path is always one segment.
                return Type::Error;
            }
        };
        if name.is_empty() {
            return Type::Error; // parser recovery
        }

        if let Some(binding) = self.lookup(name) {
            return binding.ty.clone();
        }

        // A module function used as a value.
        if let Some(sig) = self.defs.fn_named(name) {
            if !sig.generics.is_empty() {
                self.error(
                    DiagCode::AnnotationRequired,
                    span,
                    format!(
                        "generic function `{name}` can only be called directly; \
                         wrap it in a lambda to pass it as a value"
                    ),
                );
                return Type::Error;
            }
            return Type::Fn {
                params: sig.params.iter().map(|(_, t)| t.clone()).collect(),
                ret: Box::new(if sig.is_async {
                    Type::Named {
                        def: self.defs.task,
                        args: vec![sig.ret.clone()],
                    }
                } else {
                    sig.ret.clone()
                }),
                effects: sig.effects.clone(),
            };
        }

        // Unqualified prelude variants: Some / None / Ok / Err. With no
        // payload and no expected type, the enum's parameters are unknowable —
        // that is AnnotationRequired, and check() handles the payload forms.
        if let Some((def, variant)) = self.defs.unqualified_variant(name) {
            if variant.payload.is_empty() {
                self.error(
                    DiagCode::AnnotationRequired,
                    span,
                    format!(
                        "`{name}` needs the surrounding type to be known; \
                         annotate the binding or position"
                    ),
                );
                let _ = def;
                return Type::Error;
            }
            self.error(
                DiagCode::WrongArgumentCount,
                span,
                format!("`{name}` is a constructor and must be applied: `{name}(..)`"),
            );
            return Type::Error;
        }

        self.error(
            DiagCode::UnknownName,
            span,
            format!("nothing named `{name}` is in scope"),
        );
        Type::Error
    }

    /// `receiver.name` — a struct field, or `Enum.Variant`.
    fn field(&mut self, receiver: &ast::Expr, name: &ast::Ident, span: Span) -> Type {
        // `Rank.Gold`: the receiver is a type name, not a value.
        if let ast::ExprKind::Path(path) = &receiver.kind {
            if let [single] = path.segments.as_slice() {
                if self.lookup(&single.name).is_none() {
                    if let Some(def) = self.defs.lookup(&single.name) {
                        return self.variant_ref(def, name, span);
                    }
                }
            }
        }

        let receiver_ty = self.synth(receiver);
        match &receiver_ty {
            Type::Error | Type::Hole(_) => Type::Error,
            Type::Named { def, args } => {
                let info = self.defs.def(*def);
                match &info.kind {
                    DefKind::Struct { fields } => {
                        if let Some(field) = fields.iter().find(|f| f.name == name.name) {
                            let bindings: Vec<(String, Type)> = info
                                .generics
                                .iter()
                                .cloned()
                                .zip(args.iter().cloned())
                                .collect();
                            return field.ty.substitute(&bindings);
                        }
                        let message = format!(
                            "`{}` has no field named `{}`",
                            self.render(&receiver_ty),
                            name.name
                        );
                        self.error(DiagCode::UnknownField, name.span, message);
                        Type::Error
                    }
                    _ => {
                        let message =
                            format!("`{}` has no fields to access", self.render(&receiver_ty));
                        self.error(DiagCode::UnknownField, name.span, message);
                        Type::Error
                    }
                }
            }
            other => {
                let message = format!("`{}` has no fields to access", self.render(other));
                self.error(DiagCode::UnknownField, name.span, message);
                Type::Error
            }
        }
    }

    /// `Enum.Variant` as a value: unit variants make the enum, payload
    /// variants make a constructor function.
    fn variant_ref(&mut self, def: crate::ty::DefId, name: &ast::Ident, span: Span) -> Type {
        let info = self.defs.def(def);
        let generic_count = info.generics.len();
        let Some(variant) = self.defs.variant_named(def, &name.name) else {
            let message = format!("`{}` has no variant named `{}`", info.name, name.name);
            self.error(DiagCode::UnknownVariant, name.span, message);
            return Type::Error;
        };

        if generic_count > 0 {
            // Rank-style enums are the ones referenced this way in practice;
            // generic ones need the expected type, which check() supplies at
            // constructor calls. Bare references stay conservative.
            self.error(
                DiagCode::AnnotationRequired,
                span,
                format!(
                    "`{}.{}` needs the enum's type arguments to be known here",
                    info.name, name.name
                ),
            );
            return Type::Error;
        }

        let enum_ty = Type::Named {
            def,
            args: Vec::new(),
        };
        if variant.payload.is_empty() {
            enum_ty
        } else {
            Type::Fn {
                params: variant.payload.clone(),
                ret: Box::new(enum_ty),
                effects: EffectSet::empty(),
            }
        }
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
                    if let DefKind::Struct { fields } = &self.defs.def(def).kind {
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

    // ----- calls -----

    /// Bind `Type::Param`s in `pattern` by structural matching against
    /// `actual`. Returns false on a genuine conflict.
    fn match_types(pattern: &Type, actual: &Type, bindings: &mut Vec<(String, Type)>) -> bool {
        if actual.is_unknown() {
            return true;
        }
        match (pattern, actual) {
            (Type::Param(name), _) => {
                if let Some((_, bound)) = bindings.iter().find(|(n, _)| n == name) {
                    bound.is_compatible_with(actual)
                } else {
                    bindings.push((name.clone(), actual.clone()));
                    true
                }
            }
            (Type::Named { def: a, args: xs }, Type::Named { def: b, args: ys }) => {
                a == b
                    && xs.len() == ys.len()
                    && xs
                        .iter()
                        .zip(ys)
                        .all(|(x, y)| Self::match_types(x, y, bindings))
            }
            (
                Type::Fn {
                    params: p1,
                    ret: r1,
                    ..
                },
                Type::Fn {
                    params: p2,
                    ret: r2,
                    ..
                },
            ) => {
                p1.len() == p2.len()
                    && p1
                        .iter()
                        .zip(p2)
                        .all(|(x, y)| Self::match_types(x, y, bindings))
                    && Self::match_types(r1, r2, bindings)
            }
            (a, b) => a.is_compatible_with(b),
        }
    }

    /// Shared argument discipline for functions, constructors and methods.
    ///
    /// `param_names` empty means positional (variant payloads, fn values).
    fn check_args(
        &mut self,
        callee_name: &str,
        param_names: &[String],
        param_types: &[Type],
        args: &[ast::Arg],
        bindings: &mut Vec<(String, Type)>,
        span: Span,
    ) {
        if args.len() != param_types.len() {
            let message = format!(
                "`{callee_name}` takes {} argument(s), {} given",
                param_types.len(),
                args.len()
            );
            self.error(DiagCode::WrongArgumentCount, span, message);
        }

        let named_required = !param_names.is_empty() && param_types.len() >= 2;

        for (index, arg) in args.iter().enumerate() {
            // Argument-name discipline (design/0002 §8): two or more
            // parameters means every argument is named, in declaration order.
            if let Some(declared) = param_names.get(index) {
                match &arg.name {
                    Some(given) if given.name != *declared => {
                        let message =
                            format!("this argument is `{declared}`, not `{}`", given.name);
                        self.diagnostics.push(
                            Diagnostic::error(DiagCode::ArgumentNameMismatch, given.span, message)
                                .with_fix(Fix::single(
                                    format!("name it `{declared}`"),
                                    Edit::replace(given.span, declared.clone()),
                                )),
                        );
                    }
                    None if named_required => {
                        let message = format!(
                            "calls with two or more arguments name each one: `{declared}: ..`"
                        );
                        self.diagnostics.push(
                            Diagnostic::error(DiagCode::NamedArgumentsRequired, arg.span, message)
                                .with_fix(Fix::single(
                                    format!("insert `{declared}:`"),
                                    Edit::insert(arg.span.start, format!("{declared}: ")),
                                )),
                        );
                    }
                    _ => {}
                }
            }

            let Some(param_ty) = param_types.get(index) else {
                // Extra argument: already reported; synth to keep goals alive.
                let _ = self.synth(&arg.value);
                continue;
            };

            let concrete = param_ty.substitute(bindings);
            if type_is_closed(&concrete) {
                self.check(&arg.value, &concrete);
            } else {
                // The parameter still mentions unbound generics: synthesise
                // the argument and let it pin them down.
                let actual = self.synth(&arg.value);
                if !Self::match_types(&concrete, &actual, bindings) {
                    let message = format!(
                        "expected `{}`, found `{}`",
                        self.render(&concrete),
                        self.render(&actual)
                    );
                    self.error(DiagCode::TypeMismatch, arg.value.span, message);
                }
            }
        }
    }

    fn call(
        &mut self,
        callee: &ast::Expr,
        args: &[ast::Arg],
        expected: Option<&Type>,
        span: Span,
    ) -> Type {
        // Named module function?
        if let ast::ExprKind::Path(path) = &callee.kind {
            if let [single] = path.segments.as_slice() {
                if self.lookup(&single.name).is_none() {
                    if self.defs.fn_named(&single.name).is_some() {
                        return self.call_named_fn(&single.name, args, expected, span);
                    }
                    if let Some((def, _)) = self.defs.unqualified_variant(&single.name) {
                        return self.call_variant(def, &single.name, args, expected, span);
                    }
                }
            }
        }
        // Qualified variant constructor: `ScoreError.NotFound(..)`.
        if let ast::ExprKind::Field { receiver, name } = &callee.kind {
            if let ast::ExprKind::Path(path) = &receiver.kind {
                if let [single] = path.segments.as_slice() {
                    if self.lookup(&single.name).is_none() {
                        if let Some(def) = self.defs.lookup(&single.name) {
                            if self.defs.variant_named(def, &name.name).is_some() {
                                return self.call_variant(def, &name.name, args, expected, span);
                            }
                        }
                    }
                }
            }
        }

        // A function value: a lambda in a binding, a constructor reference.
        let callee_ty = self.synth(callee);
        match callee_ty {
            Type::Fn {
                params,
                ret,
                effects,
            } => {
                self.check_args("this function", &[], &params, args, &mut Vec::new(), span);
                self.require_effects(&effects, span);
                *ret
            }
            Type::Error | Type::Hole(_) => {
                for arg in args {
                    let _ = self.synth(&arg.value);
                }
                Type::Error
            }
            other => {
                let message = format!("`{}` is not callable", self.render(&other));
                self.error(DiagCode::NotCallable, callee.span, message);
                Type::Error
            }
        }
    }

    fn call_named_fn(
        &mut self,
        name: &str,
        args: &[ast::Arg],
        expected: Option<&Type>,
        span: Span,
    ) -> Type {
        let sig = self.defs.fn_named(name).expect("checked by caller");
        let param_names: Vec<String> = sig.params.iter().map(|(n, _)| n.clone()).collect();
        let param_types: Vec<Type> = sig.params.iter().map(|(_, t)| t.clone()).collect();
        let ret = sig.ret.clone();
        let effects = sig.effects.clone();
        let is_async = sig.is_async;
        let generics: Vec<GenericInfo> = sig
            .generics
            .iter()
            .map(|g| GenericInfo {
                name: g.name.clone(),
                bounds: g.bounds.clone(),
            })
            .collect();

        let mut bindings: Vec<(String, Type)> = Vec::new();
        // Seed from the expected return type first — this is what lets
        // `check(Ok(x), Result<P, E>)`-shaped calls work without annotations.
        if let Some(expected) = expected {
            let _ = Self::match_types(&ret, expected, &mut bindings);
        }

        self.check_args(name, &param_names, &param_types, args, &mut bindings, span);

        // Everything still unbound is underdetermined: fail closed.
        for generic in &generics {
            if !bindings.iter().any(|(n, _)| *n == generic.name) {
                let message = format!(
                    "cannot determine `{}` for this call to `{name}`; \
                     annotate the surrounding binding",
                    generic.name
                );
                self.error(DiagCode::AnnotationRequired, span, message);
                bindings.push((generic.name.clone(), Type::Error));
            }
        }

        // Sealed-property bounds, verified against what was bound (0006 §3).
        for generic in &generics {
            let Some((_, concrete)) = bindings.iter().find(|(n, _)| *n == generic.name) else {
                continue;
            };
            for &bound in &generic.bounds {
                if !self.defs.has_property(concrete, bound, &self.sig.generics) {
                    let message = format!(
                        "`{name}` requires `{}: {}`, but `{}` does not satisfy it",
                        generic.name,
                        bound.name(),
                        self.render(concrete)
                    );
                    self.error(DiagCode::PropertyNotSatisfied, span, message);
                }
            }
        }

        self.require_effects(&effects, span);

        let ret = ret.substitute(&bindings);
        if is_async {
            Type::Named {
                def: self.defs.task,
                args: vec![ret],
            }
        } else {
            ret
        }
    }

    fn call_variant(
        &mut self,
        def: crate::ty::DefId,
        variant_name: &str,
        args: &[ast::Arg],
        expected: Option<&Type>,
        span: Span,
    ) -> Type {
        let info = self.defs.def(def);
        let enum_generics = info.generics.clone();
        let payload = self
            .defs
            .variant_named(def, variant_name)
            .expect("checked by caller")
            .payload
            .clone();

        let mut bindings: Vec<(String, Type)> = Vec::new();
        if let Some(Type::Named {
            def: expected_def,
            args: expected_args,
        }) = expected
        {
            if expected_def == &def {
                for (name, ty) in enum_generics.iter().zip(expected_args.iter()) {
                    bindings.push((name.clone(), ty.clone()));
                }
            }
        }

        // Variant payloads are positional by design: they are unnamed in the
        // declaration, so there is nothing to name them with (0006 review).
        self.check_args(variant_name, &[], &payload, args, &mut bindings, span);

        for name in &enum_generics {
            if !bindings.iter().any(|(n, _)| n == name) {
                let message = format!(
                    "cannot determine `{name}` of `{}` here; annotate the surrounding binding",
                    info.name
                );
                self.error(DiagCode::AnnotationRequired, span, message);
                bindings.push((name.clone(), Type::Error));
            }
        }

        Type::Named {
            def,
            args: enum_generics
                .iter()
                .map(|name| {
                    bindings
                        .iter()
                        .find(|(n, _)| n == name)
                        .map(|(_, t)| t.clone())
                        .unwrap_or(Type::Error)
                })
                .collect(),
        }
    }

    fn method_call(
        &mut self,
        receiver: &ast::Expr,
        method: &ast::Ident,
        args: &[ast::Arg],
        span: Span,
    ) -> Type {
        let receiver_ty = self.synth(receiver);
        if receiver_ty.is_unknown() {
            for arg in args {
                let _ = self.synth(&arg.value);
            }
            return Type::Error;
        }

        let methods = self.defs.methods_of(&receiver_ty);
        let Some(found) = methods.iter().find(|m| m.name == method.name) else {
            let message = format!(
                "`{}` has no method named `{}`",
                self.render(&receiver_ty),
                method.name
            );
            self.error(DiagCode::UnknownMethod, method.span, message);
            for arg in args {
                let _ = self.synth(&arg.value);
            }
            return Type::Error;
        };

        // Receiver type arguments bind the receiver's own parameters
        // (Option<Int> binds T = Int); the method's extra generics come from
        // its arguments.
        let mut bindings: Vec<(String, Type)> = Vec::new();
        if let Type::Named { def, args } = &receiver_ty {
            for (name, ty) in self.defs.def(*def).generics.iter().zip(args.iter()) {
                bindings.push((name.clone(), ty.clone()));
            }
        }

        let param_names: Vec<String> = found.params.iter().map(|(n, _)| n.to_string()).collect();
        let param_types: Vec<Type> = found.params.iter().map(|(_, t)| t.clone()).collect();
        let ret = found.ret.clone();
        let effects = found.effects.clone();
        let own_generics = found.own_generics;

        self.check_args(
            &method.name,
            &param_names,
            &param_types,
            args,
            &mut bindings,
            span,
        );

        for name in own_generics {
            if !bindings.iter().any(|(n, _)| n == name) {
                let message = format!(
                    "cannot determine `{name}` for `{}`; annotate the surrounding binding",
                    method.name
                );
                self.error(DiagCode::AnnotationRequired, span, message);
                bindings.push((name.to_string(), Type::Error));
            }
        }

        self.require_effects(&effects, span);
        ret.substitute(&bindings)
    }

    // ----- struct literals -----

    fn struct_lit(
        &mut self,
        path: &ast::Path,
        field_inits: &[ast::FieldInit],
        span: Span,
        expected: Option<&Type>,
    ) -> Type {
        let name = match path.segments.as_slice() {
            [single] => single.name.as_str(),
            _ => return Type::Error,
        };
        let Some(def) = self.defs.lookup(name) else {
            self.error(
                DiagCode::UnknownType,
                path.span,
                format!("`{name}` does not name a struct"),
            );
            for init in field_inits {
                let _ = self.synth(&init.value);
            }
            return Type::Error;
        };
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

        let mut bindings: Vec<(String, Type)> = Vec::new();
        if let Some(Type::Named {
            def: expected_def,
            args,
        }) = expected
        {
            if *expected_def == def {
                for (generic, ty) in generics.iter().zip(args.iter()) {
                    bindings.push((generic.clone(), ty.clone()));
                }
            }
        }
        if !generics.is_empty() && bindings.is_empty() {
            self.error(
                DiagCode::AnnotationRequired,
                span,
                format!("`{name}` is generic; annotate the surrounding binding"),
            );
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

    // ----- patterns -----

    fn bind_pattern(&mut self, pattern: &ast::Pattern, scrutinee: &Type, mutable: bool) {
        match &pattern.kind {
            ast::PatternKind::Wildcard | ast::PatternKind::Error => {}

            ast::PatternKind::Binding(ident) => {
                // A lowercase name that happens to be a variant of the
                // scrutinee's enum is a variant pattern, not a binding —
                // otherwise a misspelt `None` would silently match everything.
                if let Type::Named { def, .. } = scrutinee {
                    if let Some(variant) = self.defs.variant_named(*def, &ident.name) {
                        if !variant.payload.is_empty() {
                            let message = format!(
                                "variant `{}` carries a payload; match it as `{}(..)`",
                                ident.name, ident.name
                            );
                            self.error(DiagCode::WrongArgumentCount, ident.span, message);
                        }
                        return;
                    }
                }
                self.bind(&ident.name, scrutinee.clone(), mutable);
            }

            ast::PatternKind::Literal(expr) => {
                let ty = self.synth(expr);
                self.require_compatible(&ty, scrutinee, pattern.span);
            }

            ast::PatternKind::Path(path) => {
                // `Rank.Gold` — enum and variant named explicitly.
                let (Some(enum_ident), Some(variant_ident)) =
                    (path.segments.first(), path.segments.get(1))
                else {
                    return;
                };
                let Some(def) = self.defs.lookup(&enum_ident.name) else {
                    self.error(
                        DiagCode::UnknownType,
                        enum_ident.span,
                        format!("`{}` does not name a type", enum_ident.name),
                    );
                    return;
                };
                let pattern_ty = Type::Named {
                    def,
                    args: match scrutinee {
                        Type::Named { def: s, args } if *s == def => args.clone(),
                        _ => vec![],
                    },
                };
                self.require_compatible(&pattern_ty, scrutinee, pattern.span);
                if self.defs.variant_named(def, &variant_ident.name).is_none() {
                    let message = format!(
                        "`{}` has no variant named `{}`",
                        enum_ident.name, variant_ident.name
                    );
                    self.error(DiagCode::UnknownVariant, variant_ident.span, message);
                }
            }

            ast::PatternKind::Variant { path, elements } => {
                let (def, variant_name) = match path.segments.as_slice() {
                    [variant] => match scrutinee {
                        Type::Named { def, .. }
                            if self.defs.variant_named(*def, &variant.name).is_some() =>
                        {
                            (*def, variant.name.clone())
                        }
                        Type::Error | Type::Hole(_) => {
                            for element in elements {
                                self.bind_pattern(element, &Type::Error, mutable);
                            }
                            return;
                        }
                        _ => {
                            let message = format!(
                                "`{}` has no variant named `{}`",
                                self.render(scrutinee),
                                variant.name
                            );
                            self.error(DiagCode::UnknownVariant, variant.span, message);
                            for element in elements {
                                self.bind_pattern(element, &Type::Error, mutable);
                            }
                            return;
                        }
                    },
                    [enum_ident, variant_ident] => {
                        let Some(def) = self.defs.lookup(&enum_ident.name) else {
                            self.error(
                                DiagCode::UnknownType,
                                enum_ident.span,
                                format!("`{}` does not name a type", enum_ident.name),
                            );
                            return;
                        };
                        (def, variant_ident.name.clone())
                    }
                    _ => return,
                };

                let Some(variant) = self.defs.variant_named(def, &variant_name) else {
                    let message = format!(
                        "`{}` has no variant named `{variant_name}`",
                        self.defs.name_of(def)
                    );
                    self.error(DiagCode::UnknownVariant, path.span, message);
                    return;
                };
                let payload = variant.payload.clone();

                // Instantiate payload types from the scrutinee's arguments.
                let bindings: Vec<(String, Type)> = match scrutinee {
                    Type::Named { def: s, args } if *s == def => self
                        .defs
                        .def(def)
                        .generics
                        .iter()
                        .cloned()
                        .zip(args.iter().cloned())
                        .collect(),
                    _ => {
                        let pattern_ty = Type::Named {
                            def,
                            args: vec![Type::Error; self.defs.def(def).generics.len()],
                        };
                        self.require_compatible(&pattern_ty, scrutinee, pattern.span);
                        Vec::new()
                    }
                };

                if elements.len() != payload.len() {
                    let message = format!(
                        "`{variant_name}` carries {} value(s), this pattern names {}",
                        payload.len(),
                        elements.len()
                    );
                    self.error(DiagCode::WrongArgumentCount, pattern.span, message);
                }
                for (element, payload_ty) in elements.iter().zip(payload.iter()) {
                    self.bind_pattern(element, &payload_ty.substitute(&bindings), mutable);
                }
            }

            ast::PatternKind::Struct { path, fields } => {
                let Some(first) = path.segments.first() else {
                    return;
                };
                let Some(def) = self.defs.lookup(&first.name) else {
                    self.error(
                        DiagCode::UnknownType,
                        first.span,
                        format!("`{}` does not name a type", first.name),
                    );
                    return;
                };
                let DefKind::Struct {
                    fields: declared_fields,
                } = &self.defs.def(def).kind
                else {
                    self.error(
                        DiagCode::UnknownType,
                        first.span,
                        format!("`{}` is not a struct", first.name),
                    );
                    return;
                };
                let declared: Vec<(String, Type)> = declared_fields
                    .iter()
                    .map(|f| (f.name.clone(), f.ty.clone()))
                    .collect();

                let bindings: Vec<(String, Type)> = match scrutinee {
                    Type::Named { def: s, args } if *s == def => self
                        .defs
                        .def(def)
                        .generics
                        .iter()
                        .cloned()
                        .zip(args.iter().cloned())
                        .collect(),
                    _ => Vec::new(),
                };

                for field in fields {
                    let Some((_, field_ty)) = declared.iter().find(|(n, _)| *n == field.name.name)
                    else {
                        let message =
                            format!("`{}` has no field named `{}`", first.name, field.name.name);
                        self.error(DiagCode::UnknownField, field.name.span, message);
                        continue;
                    };
                    let concrete = field_ty.substitute(&bindings);
                    match &field.pattern {
                        Some(sub) => self.bind_pattern(sub, &concrete, mutable),
                        None => self.bind(&field.name.name, concrete, mutable),
                    }
                }
            }

            ast::PatternKind::Or(alternatives) => {
                // Every alternative must bind the same names for the arm body
                // to be well-scoped; checked shallowly here.
                for alternative in alternatives {
                    self.bind_pattern(alternative, scrutinee, mutable);
                }
            }
        }
    }
}

/// A type with no unbound `Type::Param` left in it.
fn type_is_closed(ty: &Type) -> bool {
    match ty {
        Type::Param(_) => false,
        Type::Named { args, .. } => args.iter().all(type_is_closed),
        Type::Fn { params, ret, .. } => params.iter().all(type_is_closed) && type_is_closed(ret),
        _ => true,
    }
}
