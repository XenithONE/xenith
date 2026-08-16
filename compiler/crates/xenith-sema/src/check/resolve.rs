use xenith_diag::{
    CLOSURE_PLAN_TEACH, DiagCode, Diagnostic, Fix, Span, TASK_PLAN_TEACH, Teach, TeachItem,
};
use xenith_syntax::ast;

use crate::def::{self, DefKind};
use crate::ty::{EffectSet, Type};

use super::teach::did_you_mean;
use super::{Checker, JoinState};

/// What a dotted chain of names turned out to be, once the module set had
/// its say. `NotModule` sends the caller back to its pre-module reading.
pub(super) enum QualifiedLookup {
    NotModule,
    Fn(String),
    /// A `pub const` of another module, with its type.
    Const(Type),
    Variant(crate::ty::DefId, String),
    Type(crate::ty::DefId),
    /// A module reference that failed; the diagnostic is already out.
    Reported,
}

impl<'a> Checker<'a> {
    /// Bare type names resolve to the current module first, then the
    /// prelude — identical to plain lookup in single-file mode.
    pub(super) fn lookup_type_name(&self, name: &str) -> Option<crate::ty::DefId> {
        if let Some(ctx) = self.ctx {
            if !ctx.prefix.is_empty() {
                if let Some(def) = self.defs.lookup(&def::qualified(&ctx.prefix, name)) {
                    return Some(def);
                }
            }
        }
        self.defs.lookup(name)
    }

    /// The type of a bare `const` name: this module's own first, exactly as
    /// bare function and type names resolve.
    fn const_type(&self, bare: &str) -> Option<Type> {
        if let Some(ctx) = self.ctx {
            if !ctx.prefix.is_empty() {
                if let Some(info) = self.defs.const_named(&def::qualified(&ctx.prefix, bare)) {
                    return Some(info.ty.clone());
                }
            }
        }
        self.defs.const_named(bare).map(|info| info.ty.clone())
    }

    /// The table key for a bare function name: the current module's own
    /// function shadows nothing — prelude functions stay reachable because
    /// modules cannot redeclare them.
    pub(super) fn fn_key(&self, bare: &str) -> Option<String> {
        if let Some(ctx) = self.ctx {
            if !ctx.prefix.is_empty() {
                let key = def::qualified(&ctx.prefix, bare);
                if self.defs.fn_named(&key).is_some() {
                    return Some(key);
                }
            }
        }
        self.defs.fn_named(bare).map(|_| bare.to_string())
    }

    /// A function key as this module spells it: its own items bare,
    /// everything else fully qualified.
    pub(super) fn display_fn(&self, key: &str) -> String {
        if let Some(ctx) = self.ctx {
            if !ctx.prefix.is_empty() {
                if let Some(bare) = key.strip_prefix(&format!("{}.", ctx.prefix)) {
                    return bare.to_string();
                }
            }
        }
        key.to_string()
    }

    /// The dotted names of a pure field chain (`game.player.Player`), for
    /// module-path resolution. Anything not name-shaped answers `None`.
    fn expr_segments(expr: &ast::Expr) -> Option<Vec<String>> {
        match &expr.kind {
            ast::ExprKind::Path(path) => {
                Some(path.segments.iter().map(|s| s.name.clone()).collect())
            }
            ast::ExprKind::Field { receiver, name } => {
                let mut segments = Self::expr_segments(receiver)?;
                segments.push(name.name.clone());
                Some(segments)
            }
            _ => None,
        }
    }

    /// Resolve a dotted chain against the module set: longest module prefix
    /// wins, the `use` gate applies, and privacy is checked here so every
    /// caller reports the same way (design/0010 §1, §4).
    pub(super) fn qualified_ref(&mut self, segments: &[String], span: Span) -> QualifiedLookup {
        let Some(ctx) = self.ctx else {
            return QualifiedLookup::NotModule;
        };
        for split in (1..segments.len()).rev() {
            let module = segments[..split].join(".");
            if ctx.is_used_module(&module) {
                ctx.mark_used(&module);
                let rest = &segments[split..];
                match rest {
                    [item] => {
                        let dotted = format!("{module}.{item}");
                        if let Some(sig) = self.defs.fn_named(&dotted) {
                            if !sig.is_pub {
                                self.error(
                                    DiagCode::PrivateItemAccess,
                                    span,
                                    format!("`{dotted}` is private to `{module}`"),
                                );
                                return QualifiedLookup::Reported;
                            }
                            return QualifiedLookup::Fn(dotted);
                        }
                        if let Some(info) = self.defs.const_named(&dotted) {
                            let ty = info.ty.clone();
                            if !info.is_pub {
                                self.error(
                                    DiagCode::PrivateItemAccess,
                                    span,
                                    format!("`{dotted}` is private to `{module}`"),
                                );
                                return QualifiedLookup::Reported;
                            }
                            return QualifiedLookup::Const(ty);
                        }
                        if let Some(def) = self.defs.lookup(&dotted) {
                            if !self.defs.def(def).is_pub {
                                self.error(
                                    DiagCode::PrivateItemAccess,
                                    span,
                                    format!("`{dotted}` is private to `{module}`"),
                                );
                                return QualifiedLookup::Reported;
                            }
                            return QualifiedLookup::Type(def);
                        }
                        self.error(
                            DiagCode::UnknownName,
                            span,
                            format!("`{module}` has no item named `{item}`"),
                        );
                        return QualifiedLookup::Reported;
                    }
                    [enum_name, variant] => {
                        let dotted = format!("{module}.{enum_name}");
                        // `game.limits.CEILING.to_text()` — a const followed
                        // by a method, not an enum followed by a variant.
                        // Hand it back to the ordinary reading, where the
                        // const resolves as a value and the method applies
                        // to it.
                        if self.defs.const_named(&dotted).is_some() {
                            return QualifiedLookup::NotModule;
                        }
                        let Some(def) = self.defs.lookup(&dotted) else {
                            self.error(
                                DiagCode::UnknownName,
                                span,
                                format!("`{module}` has no item named `{enum_name}`"),
                            );
                            return QualifiedLookup::Reported;
                        };
                        if !self.defs.def(def).is_pub {
                            self.error(
                                DiagCode::PrivateItemAccess,
                                span,
                                format!("`{dotted}` is private to `{module}`"),
                            );
                            return QualifiedLookup::Reported;
                        }
                        if self.defs.variant_named(def, variant).is_none() {
                            self.error(
                                DiagCode::UnknownVariant,
                                span,
                                format!("`{dotted}` has no variant named `{variant}`"),
                            );
                            return QualifiedLookup::Reported;
                        }
                        return QualifiedLookup::Variant(def, variant.clone());
                    }
                    _ => {
                        self.error(
                            DiagCode::UnknownName,
                            span,
                            format!(
                                "items are single names; `{}` nests too deep",
                                segments.join(".")
                            ),
                        );
                        return QualifiedLookup::Reported;
                    }
                }
            }
            if self.defs.module_exists(&module) {
                self.error(
                    DiagCode::UnknownModule,
                    span,
                    format!("module `{module}` is not `use`d in this file; add `use {module};`"),
                );
                return QualifiedLookup::Reported;
            }
        }
        QualifiedLookup::NotModule
    }

    /// A dotted call target (`game.scores.best(..)` or a qualified variant
    /// constructor), resolved before the receiver is synthesised so the
    /// module name never reports as an unknown value. `None` falls back.
    pub(super) fn try_qualified_call(
        &mut self,
        receiver: &ast::Expr,
        method: &ast::Ident,
        args: &[ast::Arg],
        expected: Option<&Type>,
        span: Span,
    ) -> Option<Type> {
        self.ctx?;
        let mut segments = Self::expr_segments(receiver)?;
        if self.lookup(&segments[0]).is_some() {
            // A local binding shadows any module spelling, mirroring how
            // variant construction already defers to locals.
            return None;
        }
        segments.push(method.name.clone());
        match self.qualified_ref(&segments, span) {
            QualifiedLookup::NotModule => None,
            QualifiedLookup::Fn(key) => Some(self.call_named_fn(&key, args, expected, span)),
            QualifiedLookup::Variant(def, variant) => {
                Some(self.call_variant(def, &variant, args, expected, span))
            }
            QualifiedLookup::Const(_) => {
                let message = format!("`{}` is a const, not a fn", segments.join("."));
                self.error(DiagCode::NotCallable, span, message);
                for arg in args {
                    let _ = self.synth(&arg.value);
                }
                Some(Type::Error)
            }
            QualifiedLookup::Type(def) => {
                let message = format!(
                    "`{}` is a type; construct it with `{{ .. }}`",
                    self.defs.name_of(def)
                );
                self.error(DiagCode::NotCallable, span, message);
                for arg in args {
                    let _ = self.synth(&arg.value);
                }
                Some(Type::Error)
            }
            QualifiedLookup::Reported => {
                for arg in args {
                    let _ = self.synth(&arg.value);
                }
                Some(Type::Error)
            }
        }
    }

    // ----- names -----

    pub(super) fn synth_path(&mut self, path: &ast::Path, span: Span) -> Type {
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

        if let Some((depth, binding)) = self.lookup_indexed(name) {
            let ty = binding.ty.clone();
            let mutable = binding.mutable;
            let join = binding.join;
            // A task handle does exactly one thing — it is awaited — and
            // `.await` never routes its receiver through here. Any read
            // that does arrive is the handle escaping: a copy, a container,
            // a return, an argument, a capture (design/0015 §4).
            if let Some(index) = join {
                let message = format!(
                    "`{name}` is a task handle: it can only be awaited \
                     (`{name}.await`) — it cannot be copied, stored, \
                     returned, or passed"
                );
                self.diagnostics.push(
                    Diagnostic::error(DiagCode::JoinEscape, span, message)
                        .with_teach_note(format!("; {TASK_PLAN_TEACH}")),
                );
                self.joins[index].state = JoinState::Poisoned;
                return Type::Error;
            }
            // A reference reaching below the innermost closure boundary is a
            // capture (design/0014 §1, free-variable rule (a): values copy
            // at creation — so the copy must be honest).
            if let Some(closure) = self.closures.last() {
                if depth < closure.boundary {
                    return self.capture_check(name, ty, mutable, span);
                }
            }
            return ty;
        }

        // A `const` of this module. Module-level, so no capture rule applies
        // — the value is a literal decided at check time, not something a
        // closure could hold a stale copy of.
        if let Some(ty) = self.const_type(name) {
            return ty;
        }

        // A module function used as a value. Named functions are resolved,
        // never captured or passed (design/0014 §1 rule (b), §5: no fn-value
        // spelling for named fns) — an effectful one riding into `map` would
        // run outside every effect check.
        if let Some(key) = self.fn_key(name) {
            let shown = self.display_fn(&key);
            self.error(
                DiagCode::UnshippedConstruct,
                span,
                format!(
                    "`{shown}` is a function, not a value; call it, or wrap the \
                     call in a closure"
                ),
            );
            return Type::Error;
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

        // Definite initialization (design/0014 §2): the closure is created
        // while this very `let` is still computing its value, so there is
        // nothing to capture yet. A dedicated rule, not XN2002 — the name
        // will exist one statement later, and "unknown" would misdirect.
        if !self.closures.is_empty() && self.initializing.iter().any(|n| n == name) {
            let message = format!(
                "`{name}` is the binding this `let` is initializing; it has no \
                 value for the closure to capture yet — recursion belongs in a \
                 named fn"
            );
            self.diagnostics.push(
                Diagnostic::error(DiagCode::ClosureSelfReference, span, message)
                    .with_teach_note(format!("; {CLOSURE_PLAN_TEACH}")),
            );
            return Type::Error;
        }

        let mut message = format!("nothing named `{name}` is in scope");
        let mut use_fix: Option<Fix> = None;
        let mut use_teach: Option<Teach> = None;
        let exact: Vec<String> = self
            .ctx
            .and_then(|ctx| ctx.pub_index.get(name).cloned())
            .unwrap_or_default();
        match exact.as_slice() {
            // A unique exact pub match earns the machine fix; anything less
            // certain does not (design/0010 §6).
            [owner] => {
                message.push_str(&format!("; `use {owner};` would provide it"));
                use_fix = Some(
                    self.ctx
                        .expect("an exact match implies a project")
                        .use_fix(owner),
                );
            }
            [] => {
                // Bindings and reachable functions are the names a typo
                // could have meant; foreign modules are not in scope, so
                // they do not compete here.
                let own_prefix = self.ctx.map(|c| c.prefix.as_str()).unwrap_or("");
                let candidates =
                    self.scopes
                        .iter()
                        .flat_map(|scope| scope.iter().map(|binding| binding.name.clone()))
                        .chain(self.defs.fns.iter().filter_map(
                            |f| match f.name.rsplit_once('.') {
                                None => Some(f.name.clone()),
                                Some((owner, bare)) if owner == own_prefix => {
                                    Some(bare.to_string())
                                }
                                Some(_) => None,
                            },
                        ));
                if let Some(meant) = did_you_mean(name, candidates) {
                    message.push_str(&format!("; did you mean `{meant}`?"));
                }
            }
            owners => {
                // Several modules export the name: list them in canonical
                // order, fix nothing.
                if self.claim_teach(None) {
                    let items = owners
                        .iter()
                        .map(|owner| TeachItem::new(owner.clone(), format!("use {owner};")))
                        .collect();
                    use_teach = Some(Teach::use_candidates(name, items));
                }
            }
        }
        let mut diagnostic = Diagnostic::error(DiagCode::UnknownName, span, message);
        if let Some(fix) = use_fix {
            diagnostic = diagnostic.with_fix(fix);
        }
        if let Some(teach) = use_teach {
            diagnostic = diagnostic.with_teach(teach);
        }
        self.diagnostics.push(diagnostic);
        Type::Error
    }

    /// `receiver.name` — a struct field, or `Enum.Variant`. `expected` is
    /// the checking-position type when there is one; only the variant
    /// reading consults it (a generic enum's arguments come from context).
    pub(super) fn field(
        &mut self,
        receiver: &ast::Expr,
        name: &ast::Ident,
        span: Span,
        expected: Option<&Type>,
    ) -> Type {
        // `Rank.Gold`: the receiver is a type name, not a value.
        if let ast::ExprKind::Path(path) = &receiver.kind {
            if let [single] = path.segments.as_slice() {
                if self.lookup(&single.name).is_none() {
                    if let Some(def) = self.lookup_type_name(&single.name) {
                        return self.variant_ref(def, name, span, expected);
                    }
                }
            }
        }

        // Module-qualified value references: `game.player.Rank.Gold`, or a
        // foreign function held without calling it.
        if self.ctx.is_some() {
            if let Some(mut segments) = Self::expr_segments(receiver) {
                if self.lookup(&segments[0]).is_none() {
                    segments.push(name.name.clone());
                    match self.qualified_ref(&segments, span) {
                        QualifiedLookup::NotModule => {}
                        QualifiedLookup::Fn(key) => {
                            // Named functions are resolved, never passed
                            // (design/0014 §5) — same refusal as the bare
                            // spelling.
                            self.error(
                                DiagCode::UnshippedConstruct,
                                span,
                                format!(
                                    "`{key}` is a function, not a value; call it, or \
                                     wrap the call in a closure"
                                ),
                            );
                            return Type::Error;
                        }
                        // `game.limits.CEILING` — a `pub const` of another
                        // module reads as the value it names.
                        QualifiedLookup::Const(ty) => return ty,
                        QualifiedLookup::Variant(def, variant) => {
                            let ident = ast::Ident::new(variant, name.span);
                            return self.variant_ref(def, &ident, span, expected);
                        }
                        QualifiedLookup::Type(_) => {
                            self.error(
                                DiagCode::UnknownName,
                                span,
                                format!("`{}` is a type, not a value", segments.join(".")),
                            );
                            return Type::Error;
                        }
                        QualifiedLookup::Reported => return Type::Error,
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
    /// variants make a constructor function. `expected` is the checking
    /// position's type, which is where a generic enum's arguments come from.
    fn variant_ref(
        &mut self,
        def: crate::ty::DefId,
        name: &ast::Ident,
        span: Span,
        expected: Option<&Type>,
    ) -> Type {
        let info = self.defs.def(def);
        let generic_count = info.generics.len();
        let Some(variant) = self.defs.variant_named(def, &name.name) else {
            let message = format!("`{}` has no variant named `{}`", info.name, name.name);
            self.error(DiagCode::UnknownVariant, name.span, message);
            return Type::Error;
        };

        if generic_count > 0 {
            let payload_less = variant.payload.is_empty();
            // `let e: Wrap<Int> = Wrap.Hollow;` — a payload-less variant
            // takes the enum's arguments from the expectation, the same
            // seeding `None` gets (design/0006 §1).
            if payload_less {
                if let Some(Type::Named {
                    def: expected_def,
                    args,
                }) = expected
                {
                    if *expected_def == def && args.len() == generic_count {
                        return Type::Named {
                            def,
                            args: args.clone(),
                        };
                    }
                }
            }
            // Nothing fixed the arguments. For a payload-less variant,
            // report only where the position offered no expectation at all:
            // a concrete one of another type makes this a mismatch the
            // caller reports, and a hole or poison is not a mistake to
            // report at all (design/0006 §2). A payload-carrying variant
            // read as a value is never usable, so it always reports. Name
            // what is undetermined rather than restate that it is generic.
            if expected.is_none() || !payload_less {
                let undetermined = info
                    .generics
                    .iter()
                    .map(|g| format!("`{g}`"))
                    .collect::<Vec<_>>()
                    .join(", ");
                self.error(
                    DiagCode::AnnotationRequired,
                    span,
                    format!(
                        "nothing here determines {undetermined} of `{}`; annotate \
                         the surrounding binding, as `let x: {}<..> = {}.{};`",
                        info.name, info.name, info.name, name.name
                    ),
                );
            }
            // A poisoned instantiation rather than bare poison, so a
            // wrong-type position still reports its mismatch against a named
            // type. A payload-carrying variant read this way would be a
            // function value, which does not ship, so it stays poison.
            return if payload_less {
                Type::Named {
                    def,
                    args: vec![Type::Error; generic_count],
                }
            } else {
                Type::Error
            };
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
}
