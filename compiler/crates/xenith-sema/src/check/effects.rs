use xenith_diag::{CLOSURE_PLAN_TEACH, DiagCode, Diagnostic, Edit, Fix, Span};

use crate::def::UsesInsertion;
use crate::ty::EffectSet;

use super::Checker;

impl<'a> Checker<'a> {
    /// The effect budget at the current position: the enclosing function's
    /// declared set — or, inside a closure body, the empty set, implicitly
    /// and always (pillar 1, design/0014 §1).
    pub(super) fn effect_budget(&self) -> EffectSet {
        if self.closures.is_empty() {
            self.sig.effects.clone()
        } else {
            EffectSet::empty()
        }
    }

    pub(super) fn allowed_effect_names(&self) -> Vec<String> {
        self.effect_budget().iter().map(String::from).collect()
    }

    /// Call-site effect discipline: what the callee performs must fit inside
    /// what this function declared. The fix edits this function's `uses`
    /// clause, because that is the one edit that is mechanically safe.
    ///
    /// Inside a closure body the budget is the implicit empty set (pillar 1,
    /// design/0014 §1): every effectful call is refused, whatever route the
    /// capability took — a method on a parameter, a named fn with a
    /// non-empty `uses`, a generic that turned out effectful. No fix is
    /// offered, because a `fn(..)` type has no clause to widen.
    pub(super) fn require_effects(&mut self, needed: &EffectSet, span: Span) {
        if self.refuse_effect_in_flight(needed, span) {
            return;
        }
        self.require_effects_declared(needed, span);
    }

    /// [`Checker::require_effects`] without the in-flight rule — the form
    /// `spawn` itself uses to charge `Task.spawn`. Spawning a second child
    /// while the first is in flight is the shape design/0017 §1 blesses, so
    /// the effect that opens a flight cannot be refused by it.
    pub(super) fn require_effects_declared(&mut self, needed: &EffectSet, span: Span) {
        if !self.closures.is_empty() {
            if !needed.is_empty() {
                let listed: Vec<&str> = needed.iter().collect();
                let message = format!(
                    "this call uses {{{}}}, but a closure body performs no effects",
                    listed.join(", ")
                );
                self.diagnostics.push(
                    Diagnostic::error(DiagCode::EffectInClosure, span, message)
                        .with_teach_note(format!("; {CLOSURE_PLAN_TEACH}")),
                );
            }
            return;
        }
        let missing = needed.missing_from(&self.sig.effects);
        if missing.is_empty() {
            return;
        }
        let listed = missing.join(", ");
        let shown = self.display_fn(&self.sig.name);
        let mut diagnostic = Diagnostic::error(
            DiagCode::EffectNotPermitted,
            span,
            format!("this call uses {{{listed}}}, which `{shown}` does not declare"),
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
}
