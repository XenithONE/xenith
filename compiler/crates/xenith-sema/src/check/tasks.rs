use xenith_diag::{DiagCode, Diagnostic, Span, TASK_PLAN_TEACH, Teach, TeachItem};
use xenith_syntax::ast;

use crate::def::GenericInfo;
use crate::ty::{EffectSet, Type};

use super::calls::Callee;
use super::patterns::pattern_names;
use super::resolve::QualifiedLookup;
use super::{Binding, Checker, JoinInfo, JoinState, ScopeFlight};

impl<'a> Checker<'a> {
    // ----- task structure (design/0015) -----

    /// `scope` / `spawn` / `.await` inside a closure body — one report per
    /// body, however many task constructs it holds (design/0015 §5: the
    /// implicit empty effect budget refuses `Task.spawn`, and the region
    /// forms follow it).
    pub(super) fn task_in_closure(&mut self, spelled: &str, span: Span) {
        let ctx = self
            .closures
            .last_mut()
            .expect("task-in-closure check outside a closure");
        if ctx.task_reported {
            return;
        }
        ctx.task_reported = true;
        let message = format!(
            "{spelled} cannot appear in a closure body: a closure performs no \
             effects, and `Task.spawn` is an effect"
        );
        self.diagnostics.push(
            Diagnostic::error(DiagCode::TaskInClosure, span, message)
                .with_teach_note(format!("; {TASK_PLAN_TEACH}")),
        );
    }

    /// Check a `scope { .. }` body with its own flight region on the stack.
    pub(super) fn in_scope_region<T>(&mut self, f: impl FnOnce(&mut Self) -> T) -> T {
        self.scope_depth += 1;
        self.flights.push(ScopeFlight::default());
        let out = f(self);
        self.flights.pop();
        self.scope_depth -= 1;
        out
    }

    /// Record a task the current region is now waiting on. A `let`-bound
    /// handle names its index; the statement form has none and is joined by
    /// the scope's exit instead (design/0017 §3).
    pub(super) fn note_spawned(&mut self, join: Option<usize>) {
        let Some(flight) = self.flights.last_mut() else {
            return; // spawn outside a scope: already XN6001
        };
        match join {
            Some(index) => flight.joins.push(index),
            None => flight.fired += 1,
        }
    }

    /// Is some task spawned by an enclosing region still unconsumed? A
    /// `Poisoned` handle is one a diagnostic already covered, so it does not
    /// keep the parent silent as well.
    fn tasks_in_flight(&self) -> bool {
        self.flights.iter().any(|flight| {
            flight.fired > 0
                || flight
                    .joins
                    .iter()
                    .any(|index| self.joins[*index].state == JoinState::Live)
        })
    }

    /// XN6011 (design/0017 §1): the parent performs no effect between the
    /// first `spawn` of a scope and the consumption of everything it
    /// created. Returns whether the call was refused here, in which case the
    /// declaration rule stays quiet — the repair is to move the effect, not
    /// to widen `uses`.
    ///
    /// The teach is the canonical design/0015 sentence, unchanged: it always
    /// said effects run in the parent *after* await; this rule is that
    /// sentence finally checked rather than merely taught.
    pub(super) fn refuse_effect_in_flight(&mut self, needed: &EffectSet, span: Span) -> bool {
        // A closure body has its own, stricter rule (XN4006), and cannot
        // contain task structure at all.
        if needed.is_empty() || !self.closures.is_empty() || !self.tasks_in_flight() {
            return false;
        }
        let Some(position) = self.flights.iter().rposition(|flight| {
            flight.fired > 0
                || flight
                    .joins
                    .iter()
                    .any(|i| self.joins[*i].state == JoinState::Live)
        }) else {
            return false;
        };
        if self.flights[position].reported {
            return true;
        }
        self.flights[position].reported = true;

        let listed: Vec<&str> = needed.iter().collect();
        let waiting = self.flights[position]
            .joins
            .iter()
            .find(|index| self.joins[**index].state == JoinState::Live)
            .map(|index| self.joins[*index].name.clone());
        let message = match waiting {
            Some(name) => format!(
                "this call uses {{{}}} while the task `{name}` is still in \
                 flight; await `{name}` first",
                listed.join(", ")
            ),
            None => format!(
                "this call uses {{{}}} while a task spawned in this scope is \
                 still in flight; the scope joins it at its closing brace, so \
                 move the effect after the scope",
                listed.join(", ")
            ),
        };
        self.diagnostics.push(
            Diagnostic::error(DiagCode::EffectWhileTasksInFlight, span, message)
                .with_teach_note(format!("; {TASK_PLAN_TEACH}")),
        );
        true
    }

    /// The per-handle states, by index — what a branch snapshot holds.
    pub(super) fn join_snapshot(&self) -> Vec<JoinState> {
        self.joins.iter().map(|j| j.state).collect()
    }

    pub(super) fn restore_joins(&mut self, snapshot: &[JoinState]) {
        for (join, state) in self.joins.iter_mut().zip(snapshot) {
            join.state = *state;
        }
        // Handles created after the snapshot belong to a finished branch;
        // their own block already enforced consumption.
        for join in self.joins.iter_mut().skip(snapshot.len()) {
            join.state = JoinState::Poisoned;
        }
    }

    /// Merge the handle states after a branching construct: every branch
    /// must have consumed exactly the same handles (design/0015 §4 — the
    /// branch-partial rule). Disagreement is XN6007, once, at the construct.
    pub(super) fn merge_joins(
        &mut self,
        before: &[JoinState],
        branches: &[Vec<JoinState>],
        span: Span,
    ) {
        if branches.is_empty() {
            self.restore_joins(before);
            return;
        }
        for index in 0..self.joins.len() {
            let merged = if index >= before.len() {
                // Created inside a branch: out of scope beyond it.
                JoinState::Poisoned
            } else {
                let states: Vec<JoinState> = branches
                    .iter()
                    .map(|b| b.get(index).copied().unwrap_or(JoinState::Poisoned))
                    .collect();
                if states.contains(&JoinState::Poisoned) {
                    JoinState::Poisoned
                } else if states.iter().all(|s| *s == states[0]) {
                    states[0]
                } else {
                    let name = self.joins[index].name.clone();
                    self.error(
                        DiagCode::JoinPartialAwait,
                        span,
                        format!(
                            "`{name}` is awaited on one path through this branch \
                             but not another; every path must await it exactly once"
                        ),
                    );
                    JoinState::Poisoned
                }
            };
            self.joins[index].state = merged;
        }
    }

    /// Normal exit of the block a handle is bound in: a non-Unit result
    /// still `Live` was silently dropped — XN6008. Early exits (`return`,
    /// `break`, `continue`, and the failing arm of `?`) discard instead,
    /// which is legal because the child was pure (design/0015 §3).
    pub(super) fn enforce_join_exit(&mut self, block: &ast::Block) {
        let diverges = block.tail.is_none()
            && matches!(
                block.stmts.last().map(|s| &s.kind),
                Some(ast::StmtKind::Return(_) | ast::StmtKind::Break | ast::StmtKind::Continue)
            );
        let indices: Vec<usize> = self
            .scopes
            .last()
            .into_iter()
            .flatten()
            .filter_map(|binding| binding.join)
            .collect();
        for index in indices {
            if self.joins[index].state == JoinState::Live
                && !diverges
                && !matches!(self.joins[index].result, Type::Unit)
                && !self.joins[index].result.is_unknown()
            {
                let name = self.joins[index].name.clone();
                let span = self.joins[index].binding_span;
                self.error(
                    DiagCode::JoinUnawaited,
                    span,
                    format!(
                        "`{name}` is never awaited; a task's result cannot be \
                         dropped on normal exit — await it, or make the child \
                         return Unit and use the statement form"
                    ),
                );
            }
            // Its binding dies with this block either way.
            self.joins[index].state = JoinState::Poisoned;
        }
    }

    /// The one legal producer of a handle: `spawn f(args)` in a blessed
    /// position. Checks structure (inside `scope`, outside closures), the
    /// callee contract (a named fn, empty `uses`, CaptureSafe parameters),
    /// the `Task.spawn` effect, and the argument shapes. `None` means the
    /// spawn was refused and poisons its position.
    pub(super) fn spawn_check(
        &mut self,
        path: &ast::Path,
        args: &[ast::Arg],
        span: Span,
    ) -> Option<Type> {
        if !self.closures.is_empty() {
            self.task_in_closure("`spawn`", span);
            for arg in args {
                let _ = self.synth(&arg.value);
            }
            return None;
        }
        if self.scope_depth == 0 {
            self.error(
                DiagCode::SpawnOutsideScope,
                span,
                "`spawn` is only legal inside a `scope { .. }` block",
            );
            for arg in args {
                let _ = self.synth(&arg.value);
            }
            return None;
        }

        let key = self.spawn_callee(path, args, span)?;

        let sig = self.defs.fn_named(&key).expect("resolved by spawn_callee");
        let shown = self.display_fn(&key);
        let param_names: Vec<String> = sig.params.iter().map(|(n, _)| n.clone()).collect();
        let param_types: Vec<Type> = sig.params.iter().map(|(_, t)| t.clone()).collect();
        let ret = sig.ret.clone();
        let effects = sig.effects.clone();
        let generics: Vec<GenericInfo> = sig
            .generics
            .iter()
            .map(|g| GenericInfo {
                name: g.name.clone(),
                bounds: g.bounds.clone(),
            })
            .collect();

        // The child's contract (design/0015 §2): capability effects do not
        // cross the task boundary, so the callee declares none. Refused at
        // the boundary, once — the parent is not also charged XN4001, and
        // the argument and result rules stay quiet behind it: the repair is
        // to restructure, not to patch three symptoms of one spawn.
        if !effects.is_empty() {
            let listed: Vec<&str> = effects.iter().collect();
            let message = format!(
                "`{shown}` declares `uses {{{}}}`; a spawned child performs no \
                 effects — its `uses` set must be empty",
                listed.join(", ")
            );
            self.diagnostics.push(
                Diagnostic::error(DiagCode::SpawnEffectfulCallee, span, message)
                    .with_teach_note(format!("; {TASK_PLAN_TEACH}")),
            );
            for arg in args {
                let _ = self.synth(&arg.value);
            }
            return None;
        }

        // Spawning is itself an effect (design/0015 §5): the enclosing fn
        // declares `Task.spawn`, and the ordinary machinery carries the fix.
        self.require_effects_declared(&EffectSet::new(["Task.spawn".to_string()]), span);

        let mut bindings: Vec<(String, Type)> = Vec::new();
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

        // Every argument crosses the task boundary as a copy, so every
        // parameter type must be CaptureSafe — the design/0014 inductive
        // rule, reused verbatim (design/0015 §2).
        for (index, (param_name, param_ty)) in param_names.iter().zip(&param_types).enumerate() {
            let concrete = param_ty.substitute(&bindings);
            if !concrete.is_unknown() && !self.defs.is_capture_safe(&concrete) {
                let rendered = self.render(&concrete);
                let at = args.get(index).map(|arg| arg.value.span).unwrap_or(span);
                let message = format!(
                    "`{param_name}: {rendered}` cannot cross the task boundary: \
                     `{rendered}` is not CaptureSafe"
                );
                self.diagnostics.push(
                    Diagnostic::error(DiagCode::SpawnArgumentNotCaptureSafe, at, message)
                        .with_teach_note(format!("; {TASK_PLAN_TEACH}")),
                );
            }
        }

        Some(ret.substitute(&bindings))
    }

    /// Resolve the spawned callee: a bare own-module fn or a fully
    /// qualified one — the same resolution ordinary calls use (design/0010).
    /// Anything else — a local value, a method receiver, a constructor — is
    /// XN6004, and an unknown name reports the way every unknown name does.
    fn spawn_callee(&mut self, path: &ast::Path, args: &[ast::Arg], span: Span) -> Option<String> {
        let refuse = |this: &mut Self, message: String| {
            this.error(DiagCode::SpawnCalleeNotFn, path.span, message);
            for arg in args {
                let _ = this.synth(&arg.value);
            }
            None
        };

        let segments: Vec<String> = path.segments.iter().map(|s| s.name.clone()).collect();
        if let [single] = segments.as_slice() {
            if self.lookup(single).is_some() {
                return refuse(
                    self,
                    format!("`{single}` is a value; `spawn` takes a named fn"),
                );
            }
            if let Some(key) = self.fn_key(single) {
                return Some(key);
            }
            if self.defs.unqualified_variant(single).is_some() {
                return refuse(
                    self,
                    format!("`{single}` is a constructor, not a fn; `spawn` takes a named fn"),
                );
            }
            // Unknown: the ordinary rich report (did-you-mean, use-fix).
            let _ = self.synth_path(path, span);
            for arg in args {
                let _ = self.synth(&arg.value);
            }
            return None;
        }

        if self.lookup(&segments[0]).is_some() {
            return refuse(
                self,
                format!(
                    "`{}` is a method call on `{}`; `spawn` takes a named fn — \
                     extract one and spawn that",
                    segments.join("."),
                    segments[0]
                ),
            );
        }
        if segments.len() == 2 && self.lookup_type_name(&segments[0]).is_some() {
            return refuse(
                self,
                format!(
                    "`{}` is a constructor, not a fn; `spawn` takes a named fn",
                    segments.join(".")
                ),
            );
        }
        if self.ctx.is_some() {
            match self.qualified_ref(&segments, path.span) {
                QualifiedLookup::Fn(key) => return Some(key),
                QualifiedLookup::Const(_)
                | QualifiedLookup::Variant(..)
                | QualifiedLookup::Type(_) => {
                    return refuse(
                        self,
                        format!(
                            "`{}` is not a fn; `spawn` takes a named fn",
                            segments.join(".")
                        ),
                    );
                }
                QualifiedLookup::Reported => {
                    for arg in args {
                        let _ = self.synth(&arg.value);
                    }
                    return None;
                }
                QualifiedLookup::NotModule => {}
            }
        }
        self.error(
            DiagCode::UnknownName,
            path.span,
            format!("nothing named `{}` is in scope", segments.join(".")),
        );
        for arg in args {
            let _ = self.synth(&arg.value);
        }
        None
    }

    /// A spawn somewhere other than its two blessed positions — the general
    /// escape refusal (design/0015 §4).
    pub(super) fn spawn_escape(&mut self, args: &[ast::Arg], span: Span, message: &str) -> Type {
        self.diagnostics.push(
            Diagnostic::error(DiagCode::JoinEscape, span, message.to_string())
                .with_teach_note(format!("; {TASK_PLAN_TEACH}")),
        );
        for arg in args {
            let _ = self.synth(&arg.value);
        }
        Type::Error
    }

    /// `let name = spawn f(..);` — bind a task handle. The binding is bare
    /// on purpose: no `var` (the handle is consumed, never reassigned), no
    /// annotation (`Join` is not a written type), no pattern (a handle has
    /// no structure to take apart, and `_` would silence the result).
    pub(super) fn spawn_binding(
        &mut self,
        pattern: &ast::Pattern,
        annotation: Option<&ast::Type>,
        path: &ast::Path,
        args: &[ast::Arg],
        span: Span,
        mutable: bool,
    ) {
        let simple = matches!(&pattern.kind, ast::PatternKind::Binding(_));
        if annotation.is_some() || mutable || !simple {
            let message = if mutable {
                "a task handle is bound with `let`, not `var`: it is consumed \
                 by `.await`, not reassigned"
            } else if annotation.is_some() {
                "a task binding takes no type annotation — `Join` is not a \
                 written type; bind it bare: `let j = spawn f(..);`"
            } else {
                "bind the task to a name; a pattern or `_` would silence its \
                 result"
            };
            if let Some(ty) = annotation {
                // Lowered anyway, so the annotation's own problems and its
                // holes' goals survive this refusal.
                let _ = self.lower(ty);
            }
            self.diagnostics.push(
                Diagnostic::error(DiagCode::JoinEscape, span, message.to_string())
                    .with_teach_note(format!("; {TASK_PLAN_TEACH}")),
            );
            for arg in args {
                let _ = self.synth(&arg.value);
            }
            self.bind_pattern(pattern, &Type::Error, mutable);
            return;
        }

        let mut names = Vec::new();
        pattern_names(pattern, &mut names);
        let saved = std::mem::replace(&mut self.initializing, names);
        let result = self.spawn_check(path, args, span);
        self.initializing = saved;

        let ast::PatternKind::Binding(ident) = &pattern.kind else {
            unreachable!("checked above");
        };
        match result {
            Some(result) => {
                if ident.name.is_empty() {
                    return; // parser recovery
                }
                let index = self.joins.len();
                self.joins.push(JoinInfo {
                    name: ident.name.clone(),
                    result: result.clone(),
                    state: JoinState::Live,
                    binding_span: pattern.span,
                    created_loop_depth: self.loop_depth,
                });
                // The parent now has a child in flight (design/0017 §1).
                self.note_spawned(Some(index));
                self.scopes
                    .last_mut()
                    .expect("at least one scope")
                    .push(Binding {
                        name: ident.name.clone(),
                        ty: result,
                        mutable: false,
                        join: Some(index),
                    });
            }
            None => self.bind(&ident.name, Type::Error, false),
        }
    }

    /// `.await` of a live handle: move the result out, exactly once. Every
    /// route that could run it zero or several times is refused here.
    pub(super) fn consume_join(&mut self, index: usize, span: Span) -> Type {
        let result = self.joins[index].result.clone();
        let name = self.joins[index].name.clone();
        match self.joins[index].state {
            JoinState::Live if self.in_guard => {
                self.error(
                    DiagCode::JoinAwaitedTwice,
                    span,
                    format!(
                        "`{name}` is awaited inside a `match` guard, which may \
                         run for several arms; await it before the `match`"
                    ),
                );
                self.joins[index].state = JoinState::Poisoned;
            }
            JoinState::Live if self.loop_depth > self.joins[index].created_loop_depth => {
                self.error(
                    DiagCode::JoinAwaitedTwice,
                    span,
                    format!(
                        "`{name}` was created outside this loop, so the loop \
                         may await it more than once; await it after the loop"
                    ),
                );
                self.joins[index].state = JoinState::Poisoned;
            }
            JoinState::Live => self.joins[index].state = JoinState::Consumed,
            JoinState::Consumed => {
                self.error(
                    DiagCode::JoinAwaitedTwice,
                    span,
                    format!("`{name}` is already awaited; `.await` consumes the task exactly once"),
                );
                self.joins[index].state = JoinState::Poisoned;
            }
            JoinState::Poisoned => {}
        }
        result
    }
}
