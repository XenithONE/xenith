use xenith_diag::Span;
use xenith_syntax::ast;

use crate::ty::{HoleId, Type};

use super::{Checker, Goal, Probe};

impl<'a> Checker<'a> {
    pub(super) fn fresh_hole(&mut self) -> HoleId {
        let id = HoleId(*self.next_hole);
        *self.next_hole += 1;
        id
    }

    /// Capture the checker's state for `type-at`, smallest containing span
    /// wins. Runs on every expression; a `None` probe offset makes it free.
    pub(super) fn maybe_probe(&mut self, span: Span, ty: &Type) {
        let Some(offset) = self.probe_offset else {
            return;
        };
        if !span.contains(offset) {
            return;
        }
        let better = match self.probe.as_ref() {
            Some(existing) => span.len() <= existing.span.len(),
            None => true,
        };
        if better {
            *self.probe = Some(Probe {
                span,
                ty: self.render(ty),
                enclosing_function: self.sig.name.clone(),
                in_scope: self.scope_snapshot(),
                allowed_effects: self.allowed_effect_names(),
            });
        }
    }

    /// Snapshot the scope for a goal: innermost occurrence of each name wins.
    fn scope_snapshot(&self) -> Vec<(String, String)> {
        self.scope_types()
            .into_iter()
            .map(|(name, ty, _)| {
                let rendered = self.render(&ty);
                (name, rendered)
            })
            .collect()
    }

    /// The same snapshot with real types and mutability, for candidate
    /// generation — a mutating method is only offered on a `var` binding.
    ///
    /// Task handles are excluded (design/0015 §4): a Join is not a value,
    /// so neither a goal's scope listing nor a candidate expression may
    /// offer one — and being shadowed-out beats being described falsely.
    fn scope_types(&self) -> Vec<(String, Type, bool)> {
        let mut seen = std::collections::HashSet::new();
        let mut out = Vec::new();
        for scope in self.scopes.iter().rev() {
            for binding in scope.iter().rev() {
                if binding.join.is_some() {
                    seen.insert(binding.name.clone());
                    continue;
                }
                if seen.insert(binding.name.clone()) {
                    out.push((binding.name.clone(), binding.ty.clone(), binding.mutable));
                }
            }
        }
        out.reverse();
        out
    }

    pub(super) fn push_goal(
        &mut self,
        name: Option<String>,
        span: Span,
        kind: &'static str,
        expected: &Type,
    ) {
        let rendered = self.render(expected);
        let used_paths: Vec<String> = self
            .ctx
            .map(|ctx| ctx.uses.iter().map(|(path, _)| path.clone()).collect())
            .unwrap_or_default();
        let view = crate::candidates::CandidateView {
            enclosing: &self.sig.name,
            hole_name: name.as_deref(),
            module: self
                .ctx
                .map(|ctx| (ctx.prefix.as_str(), used_paths.as_slice())),
        };
        let budget = self.effect_budget();
        let (candidates, blocked) = crate::candidates::candidates_for(
            self.defs,
            expected,
            &self.scope_types(),
            &budget,
            &self.sig.generics,
            &view,
        );
        let goal = Goal {
            name,
            span,
            kind,
            expected: rendered,
            enclosing_function: self.sig.name.clone(),
            in_scope: self.scope_snapshot(),
            allowed_effects: self.allowed_effect_names(),
            candidates,
            blocked,
        };
        self.goals.push(goal);
    }

    /// A goal with no meaningful expected type — type holes. No candidates.
    pub(super) fn push_goal_rendered(
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
            allowed_effects: self.allowed_effect_names(),
            candidates: Vec::new(),
            blocked: Vec::new(),
        };
        self.goals.push(goal);
    }

    /// Record goals for every `??` inside a syntactic type. Signature types
    /// are lowered during collection, before any checker exists, so their
    /// holes are given goals here — where the enclosing function is known.
    pub(super) fn type_goals_in(&mut self, ty: &ast::Type) {
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
                    self.type_goals_in(&param.ty);
                }
                self.type_goals_in(ret);
            }
            ast::TypeKind::Unit | ast::TypeKind::Error => {}
        }
    }
}
