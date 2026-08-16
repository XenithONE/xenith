use xenith_diag::{DiagCode, Diagnostic, Edit, Fix, Span, Teach, TeachItem};
use xenith_syntax::ast;

use crate::def::{DefKind, GenericInfo};
use crate::ty::Type;

use super::Checker;
use super::teach::did_you_mean;

/// One callee at one call site: the name diagnostics use, its declared
/// parameters, and — when the callee is resolved — the signature teach that
/// rides the first argument-shape diagnostic (design/0009 §3).
pub(super) struct Callee<'c> {
    pub(super) name: &'c str,
    pub(super) param_names: &'c [String],
    pub(super) param_types: &'c [Type],
    pub(super) teach: Option<Teach>,
}

impl<'a> Checker<'a> {
    // ----- calls -----

    /// Bind `Type::Param`s in `pattern` by structural matching against
    /// `actual`. Returns false on a genuine conflict.
    pub(super) fn match_types(
        pattern: &Type,
        actual: &Type,
        bindings: &mut Vec<(String, Type)>,
    ) -> bool {
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
    /// Empty `param_names` means positional (variant payloads, fn values).
    /// The callee's teach, when present, rides the first argument-shape
    /// diagnostic; the rest of the call site does not repeat it.
    pub(super) fn check_args(
        &mut self,
        callee: Callee<'_>,
        args: &[ast::Arg],
        bindings: &mut Vec<(String, Type)>,
        span: Span,
    ) {
        let Callee {
            name: callee_name,
            param_names,
            param_types,
            mut teach,
        } = callee;

        if args.len() != param_types.len() {
            let message = format!(
                "`{callee_name}` takes {} argument(s), {} given",
                param_types.len(),
                args.len()
            );
            let mut diagnostic = Diagnostic::error(DiagCode::WrongArgumentCount, span, message);
            if teach.is_some() && self.claim_teach(None) {
                diagnostic = diagnostic.with_teach(teach.take().expect("checked above"));
            }
            self.diagnostics.push(diagnostic);
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
                        let mut diagnostic =
                            Diagnostic::error(DiagCode::ArgumentNameMismatch, given.span, message)
                                .with_fix(Fix::single(
                                    format!("name it `{declared}`"),
                                    Edit::replace(given.span, declared.clone()),
                                ));
                        if teach.is_some() && self.claim_teach(None) {
                            diagnostic =
                                diagnostic.with_teach(teach.take().expect("checked above"));
                        }
                        self.diagnostics.push(diagnostic);
                    }
                    None if named_required => {
                        let message = format!(
                            "calls with two or more arguments name each one: `{declared}: ..`"
                        );
                        let mut diagnostic =
                            Diagnostic::error(DiagCode::NamedArgumentsRequired, arg.span, message)
                                .with_fix(Fix::single(
                                    format!("insert `{declared}:`"),
                                    Edit::insert(arg.span.start, format!("{declared}: ")),
                                ));
                        if teach.is_some() && self.claim_teach(None) {
                            diagnostic =
                                diagnostic.with_teach(teach.take().expect("checked above"));
                        }
                        self.diagnostics.push(diagnostic);
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

            // A closure argument for a `fn(..)` parameter — the one legal
            // closure position (design/0014 §3). This must run before the
            // closed/open split: `map`'s `fn(T) -> U` is open in `U`, and
            // the closure's body is exactly what pins it down.
            if let (
                ast::ExprKind::Lambda {
                    params: lambda_params,
                    body,
                },
                Type::Fn {
                    params: fn_params,
                    ret,
                    ..
                },
            ) = (&arg.value.kind, &concrete)
            {
                let fn_params = fn_params.clone();
                let ret = (**ret).clone();
                self.check_lambda(
                    lambda_params,
                    body,
                    &fn_params,
                    &ret,
                    arg.value.span,
                    bindings,
                );
                continue;
            }

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

    pub(super) fn call(
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
                    if let Some(key) = self.fn_key(&single.name) {
                        return self.call_named_fn(&key, args, expected, span);
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
                        if let Some(def) = self.lookup_type_name(&single.name) {
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
                self.check_args(
                    Callee {
                        name: "this function",
                        param_names: &[],
                        param_types: &params,
                        teach: None,
                    },
                    args,
                    &mut Vec::new(),
                    span,
                );
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

    pub(super) fn call_named_fn(
        &mut self,
        name: &str,
        args: &[ast::Arg],
        expected: Option<&Type>,
        span: Span,
    ) -> Type {
        let sig = self.defs.fn_named(name).expect("checked by caller");
        let shown = self.display_fn(name);
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

        // The declared signature rides the first argument-shape diagnostic
        // (design/0009 §3): the callee is resolved, so this is the highest
        // precision teach there is.
        let teach = Some(Teach::call_signature(
            String::new(),
            TeachItem::new(
                shown.clone(),
                self.signature_text(&shown, &param_names, &param_types, &ret, &effects),
            ),
        ));

        self.check_args(
            Callee {
                name: &shown,
                param_names: &param_names,
                param_types: &param_types,
                teach,
            },
            args,
            &mut bindings,
            span,
        );

        // Everything still unbound is underdetermined: fail closed.
        for generic in &generics {
            if !bindings.iter().any(|(n, _)| *n == generic.name) {
                let message = format!(
                    "cannot determine `{}` for this call to `{shown}`; \
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
                        "`{shown}` requires `{}: {}`, but `{}` does not satisfy it",
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

    pub(super) fn call_variant(
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
        self.check_args(
            Callee {
                name: variant_name,
                param_names: &[],
                param_types: &payload,
                teach: None,
            },
            args,
            &mut bindings,
            span,
        );

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

    /// `Grade.Pass(95)` parses as a method call — `.name(` always does — so
    /// qualified variant construction arrives here, not at `call`. Detect it
    /// before synthesising the receiver, or the enum's name reports as an
    /// unknown value.
    pub(super) fn qualified_variant_target(
        &self,
        receiver: &ast::Expr,
        method: &str,
    ) -> Option<crate::ty::DefId> {
        let ast::ExprKind::Path(path) = &receiver.kind else {
            return None;
        };
        let [single] = path.segments.as_slice() else {
            return None;
        };
        if self.lookup(&single.name).is_some() {
            return None;
        }
        let def = self.lookup_type_name(&single.name)?;
        self.defs.variant_named(def, method).map(|_| def)
    }

    pub(super) fn method_call(
        &mut self,
        receiver: &ast::Expr,
        method: &ast::Ident,
        args: &[ast::Arg],
        span: Span,
    ) -> Type {
        if let Some(def) = self.qualified_variant_target(receiver, &method.name) {
            return self.call_variant(def, &method.name, args, None, span);
        }
        if let Some(found) = self.try_qualified_call(receiver, method, args, None, span) {
            return found;
        }

        let receiver_ty = self.synth(receiver);
        if receiver_ty.is_unknown() {
            for arg in args {
                let _ = self.synth(&arg.value);
            }
            return Type::Error;
        }

        let methods = self.defs.methods_of(&receiver_ty);
        let Some(found) = methods.iter().find(|m| m.name == method.name) else {
            let mut message = format!(
                "`{}` has no method named `{}`",
                self.render(&receiver_ty),
                method.name
            );
            if let Some(meant) =
                did_you_mean(&method.name, methods.iter().map(|m| m.name.to_string()))
            {
                message.push_str(&format!("; did you mean `{meant}`?"));
            }
            let mut diagnostic = Diagnostic::error(DiagCode::UnknownMethod, method.span, message);
            if let Some(owner) = self.module_owner_of(&receiver_ty) {
                // A module-owned type: steer the message itself away from the
                // method prior (design/0012 §1) — a body that stops at "has
                // no method" reinforces exactly the habit that failed — and
                // attach the rewrite bridge when the module offers one.
                let spelled = self.call_spelling(&owner, &method.name);
                diagnostic = diagnostic
                    .with_teach_note(format!("; module functions are called as `{spelled}(...)`"));
                if let Some(teach) = self.module_call_teach(&receiver_ty, &owner, &method.name) {
                    if self.claim_module_call_teach(&teach.type_name, &method.name) {
                        diagnostic = diagnostic.with_teach(teach);
                    }
                }
            } else if let Some(catalogue) = self.method_catalogue(&receiver_ty, &methods) {
                // The receiver's catalogue is the measured payload (0009 §6
                // step 0: XN2003 dominates unrepaired failures). An empty
                // catalogue teaches nothing and claims no budget.
                if self.claim_teach(Some(&catalogue.type_name)) {
                    diagnostic = diagnostic.with_teach(catalogue);
                }
            }
            self.diagnostics.push(diagnostic);
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
        let bounds = found.bounds;

        if found.mutates_receiver {
            self.require_mutable_receiver(receiver, &method.name);
        }

        // The taught signature shows the receiver's generics already bound:
        // `insert(key: String, value: Int)`, not the schematic form.
        let taught_types: Vec<Type> = param_types
            .iter()
            .map(|t| t.substitute(&bindings))
            .collect();
        let teach = Some(Teach::call_signature(
            self.render(&receiver_ty),
            TeachItem::new(
                method.name.clone(),
                self.signature_text(
                    &method.name,
                    &param_names,
                    &taught_types,
                    &ret.substitute(&bindings),
                    &effects,
                ),
            ),
        ));

        self.check_args(
            Callee {
                name: &method.name,
                param_names: &param_names,
                param_types: &param_types,
                teach,
            },
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

        // Sealed-property bounds, verified against what the receiver bound
        // (0006 §3): `sorted` needs `T: Ord`, which rejects `List<Float>`.
        for (name, property) in bounds {
            let Some((_, concrete)) = bindings.iter().find(|(n, _)| n == name) else {
                continue;
            };
            if !self
                .defs
                .has_property(concrete, *property, &self.sig.generics)
            {
                let message = format!(
                    "`{}` requires `{name}: {}`, but `{}` does not satisfy it",
                    method.name,
                    property.name(),
                    self.render(concrete)
                );
                self.error(DiagCode::PropertyNotSatisfied, span, message);
            }
        }

        self.require_effects(&effects, span);
        ret.substitute(&bindings)
    }

    /// The owning module of a user type when it is not the current one —
    /// the wall that field writes stop at (design/0010 §4).
    pub(super) fn foreign_owner(&self, def: crate::ty::DefId) -> Option<String> {
        let ctx = self.ctx?;
        let name = self.defs.name_of(def);
        let (owner, _) = name.rsplit_once('.')?;
        (owner != ctx.prefix).then(|| owner.to_string())
    }

    /// A mutating method writes through its receiver, so the receiver must be
    /// a mutable place — the same rule `=` enforces, phrased for the call.
    fn require_mutable_receiver(&mut self, receiver: &ast::Expr, method_name: &str) {
        match &receiver.kind {
            ast::ExprKind::Path(path) => {
                if let [single] = path.segments.as_slice() {
                    if let Some(binding) = self.lookup(&single.name) {
                        if !binding.mutable {
                            let message = format!(
                                "`{}` is a `let` binding; declare it with `var` to call `{method_name}` on it",
                                single.name
                            );
                            self.error(DiagCode::AssignmentToImmutable, receiver.span, message);
                        }
                    }
                }
            }
            ast::ExprKind::Field {
                receiver: base,
                name,
            } => {
                if let Type::Named { def, .. } = self.synth(base) {
                    if let Some(owner) = self.foreign_owner(def) {
                        let message = format!(
                            "`{method_name}` writes through field `{}` of `{}`, which cannot be mutated from outside `{owner}`",
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
                if let ast::ExprKind::Path(path) = &base.kind {
                    if let [single] = path.segments.as_slice() {
                        if let Some(binding) = self.lookup(&single.name) {
                            if !binding.mutable {
                                let message = format!(
                                    "`{}` is a `let` binding; declare it with `var` to call `{method_name}` through it",
                                    single.name
                                );
                                self.error(DiagCode::AssignmentToImmutable, base.span, message);
                            }
                        }
                    }
                }
            }
            _ => {
                let message = format!(
                    "`{method_name}` mutates its receiver; call it on a `var` binding, not a temporary value"
                );
                self.error(DiagCode::AssignmentToImmutable, receiver.span, message);
            }
        }
    }
}

/// A type with no unbound `Type::Param` left in it.
pub(super) fn type_is_closed(ty: &Type) -> bool {
    match ty {
        Type::Param(_) => false,
        Type::Named { args, .. } => args.iter().all(type_is_closed),
        Type::Fn { params, ret, .. } => params.iter().all(type_is_closed) && type_is_closed(ret),
        _ => true,
    }
}
