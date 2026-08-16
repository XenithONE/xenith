use xenith_diag::{CLOSURE_EXIT_TEACH, CLOSURE_PLAN_TEACH, DiagCode, Diagnostic, Span};
use xenith_syntax::ast;

use crate::ty::Type;

use super::calls::type_is_closed;
use super::{Binding, Checker, ClosureCtx};

impl<'a> Checker<'a> {
    /// Pillar 2 (design/0014 §1): a closure body referenced a binding from
    /// outside the closure. Values capture by copy at creation, so the copy
    /// must be honest: `var` bindings are refused outright (the snapshot
    /// would go stale invisibly), and everything else must be CaptureSafe.
    /// A violation poisons the reference so one bad capture reports once.
    pub(super) fn capture_check(
        &mut self,
        name: &str,
        ty: Type,
        mutable: bool,
        span: Span,
    ) -> Type {
        let already = self
            .closures
            .last()
            .expect("capture check outside a closure")
            .reported
            .iter()
            .any(|n| n == name);
        if mutable {
            if !already {
                self.remember_capture(name);
                let message = format!(
                    "`{name}` is a `var` binding and cannot be captured: a closure \
                     copies its captures when it is created, so updates after that \
                     snapshot are not visible — bind the current value to a `let` \
                     first and capture that"
                );
                self.diagnostics
                    .push(Diagnostic::error(DiagCode::CaptureOfVar, span, message));
            }
            return Type::Error;
        }
        if !self.defs.is_capture_safe(&ty) {
            if !already {
                self.remember_capture(name);
                let rendered = self.render(&ty);
                let message = format!(
                    "a closure cannot capture `{name}`: `{rendered}` is not \
                     CaptureSafe — capabilities, `Shared`, `Task` and unbounded \
                     type parameters have no honest snapshot copy"
                );
                self.diagnostics.push(
                    Diagnostic::error(DiagCode::CapabilityCapture, span, message)
                        .with_teach_note(format!("; {CLOSURE_PLAN_TEACH}")),
                );
            }
            return Type::Error;
        }
        ty
    }

    /// `?`, `return`, `break`, `continue` trying to cross a closure
    /// boundary (design/0014 §3). One code, one converged teach.
    pub(super) fn closure_early_exit(&mut self, span: Span, spelled: &str) {
        let message = format!("{spelled} cannot cross a closure boundary");
        self.diagnostics.push(
            Diagnostic::error(DiagCode::ClosureEarlyExit, span, message)
                .with_teach_note(format!("; {CLOSURE_EXIT_TEACH}")),
        );
    }

    fn remember_capture(&mut self, name: &str) {
        self.closures
            .last_mut()
            .expect("capture check outside a closure")
            .reported
            .push(name.to_string());
    }

    /// Check one closure against the `fn(..)` type of the parameter it is
    /// passed to — the whole of closure checking (design/0014).
    ///
    /// Parameter types are read from the fn type (there is no annotation
    /// syntax to disagree with). The body is checked against the declared
    /// return when it is closed; when it is an unbound generic — `map`'s `U`
    /// — the body's synthesised type binds it through `bindings`. The two
    /// pillars ride the ordinary traversal: [`ClosureCtx`] on the stack
    /// makes [`Checker::require_effects`] refuse every effect and
    /// [`Checker::capture_check`] police every reference below the boundary.
    pub(super) fn check_lambda(
        &mut self,
        params: &[ast::LambdaParam],
        body: &ast::Expr,
        param_types: &[Type],
        ret: &Type,
        span: Span,
        bindings: &mut Vec<(String, Type)>,
    ) {
        if params.len() != param_types.len() {
            let message = format!(
                "this closure takes {} parameter(s), but the `fn` type here takes {}",
                params.len(),
                param_types.len()
            );
            self.error(DiagCode::WrongArgumentCount, span, message);
        }

        self.scopes.push(Vec::new());
        for (index, param) in params.iter().enumerate() {
            if param.name.name == "_" || param.name.name.is_empty() {
                continue; // discarded, or parser recovery
            }
            let ty = param_types
                .get(index)
                .map(|t| t.substitute(bindings))
                .filter(type_is_closed)
                .unwrap_or(Type::Error);
            self.scopes.last_mut().expect("just pushed").push(Binding {
                name: param.name.name.clone(),
                ty,
                mutable: false,
                join: None,
            });
        }
        self.closures.push(ClosureCtx {
            boundary: self.scopes.len() - 1,
            entry_loop_depth: self.loop_depth,
            reported: Vec::new(),
            task_reported: false,
        });

        let ret_concrete = ret.substitute(bindings);
        if type_is_closed(&ret_concrete) {
            self.check(body, &ret_concrete);
        } else {
            let actual = self.synth(body);
            if actual.is_unknown() {
                // The body is poison — a capture violation, an effect refusal
                // — and pins nothing. Bind what it would have pinned to
                // poison too, so "cannot determine `U`" is not stacked on a
                // mistake already reported.
                bind_open_params(&ret_concrete, bindings);
            } else if !Self::match_types(&ret_concrete, &actual, bindings) {
                let message = format!(
                    "expected `{}`, found `{}`",
                    self.render(&ret_concrete),
                    self.render(&actual)
                );
                self.error(DiagCode::TypeMismatch, body.span, message);
            }
        }

        self.closures.pop();
        self.scopes.pop();
    }
}

/// Bind every still-open `Type::Param` in `ty` to poison. Used when a
/// closure body failed: the generics it would have pinned must not each earn
/// their own "cannot determine" on top of the reported mistake.
fn bind_open_params(ty: &Type, bindings: &mut Vec<(String, Type)>) {
    match ty {
        Type::Param(name) => {
            if !bindings.iter().any(|(n, _)| n == name) {
                bindings.push((name.clone(), Type::Error));
            }
        }
        Type::Named { args, .. } => args.iter().for_each(|a| bind_open_params(a, bindings)),
        Type::Fn { params, ret, .. } => {
            params.iter().for_each(|p| bind_open_params(p, bindings));
            bind_open_params(ret, bindings);
        }
        _ => {}
    }
}
