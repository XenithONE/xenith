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

use xenith_diag::{
    DiagCode, Diagnostic, Edit, Fix, MAX_SIGNATURE_BYTES, MAX_TEACH_ITEMS, Span, Teach, TeachItem,
};
use xenith_syntax::ast;

use crate::def::{self, DefKind, DefTable, FnSig, GenericInfo, MethodSig, Property, UsesInsertion};
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
    /// Ranked scaffolds that would fit here, holes included. Empty for type
    /// goals and for holes whose expected type is unknown.
    pub candidates: Vec<crate::candidates::Candidate>,
    /// Symbols that produce the right type but are unusable here, with the
    /// reason — a model not told *why* repeats the mistake.
    pub blocked: Vec<String>,
}

pub struct Analysis {
    pub diagnostics: Vec<Diagnostic>,
    pub goals: Vec<Goal>,
}

/// What the checker knew about the innermost expression containing a probed
/// offset — the payload of `xenith query type-at`.
#[derive(Clone, Debug)]
pub struct Probe {
    pub span: Span,
    pub ty: String,
    pub enclosing_function: String,
    pub in_scope: Vec<(String, String)>,
    pub allowed_effects: Vec<String>,
}

pub fn analyze(module: &ast::Module) -> Analysis {
    analyze_at(module, None).0
}

/// Analyse, additionally capturing the checker's state at `offset`.
///
/// The probe rides the ordinary traversal — the same claim as holes: the
/// answer to "what is required here?" is the checker's current state, and a
/// query is just a hole the author did not have to write.
pub fn analyze_at(module: &ast::Module, offset: Option<u32>) -> (Analysis, Option<Probe>) {
    let (table, mut diagnostics) = def::collect(module);

    // A type that contains itself by value has no size; refuse it before
    // any body pretends otherwise (design/0010 §5).
    for cycle in crate::recursion::value_cycles(&table) {
        let first = &cycle[0];
        let span = module
            .items
            .iter()
            .find_map(|item| match &item.kind {
                ast::ItemKind::Struct(s) if s.name.name == *first => Some(s.name.span),
                ast::ItemKind::Enum(e) if e.name.name == *first => Some(e.name.span),
                _ => None,
            })
            .unwrap_or(Span::EMPTY);
        diagnostics.push(infinite_size_diagnostic(&cycle, span));
    }

    let mut goals = Vec::new();
    let mut next_hole = 0u32;
    let mut probe = None;
    let mut teach_budget = TeachBudget::new();

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
            probe_offset: offset,
            probe: &mut probe,
            teach_budget: &mut teach_budget,
            ctx: None,
        };
        checker.check_fn();
    }

    goals.sort_by_key(|g| g.span.start);
    (Analysis { diagnostics, goals }, probe)
}

/// Body checking for one project module: the walk `analyze` does, with the
/// module context wired in and goals discarded — project-mode goals are a
/// later slice (design/0010 §7).
pub(crate) fn check_module_bodies(
    table: &DefTable,
    module: &ast::Module,
    ctx: &ModuleCtx,
    teach_budget: &mut TeachBudget,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let mut goals = Vec::new();
    let mut next_hole = 0u32;
    let mut probe = None;
    for item in &module.items {
        let ast::ItemKind::Fn(f) = &item.kind else {
            continue;
        };
        let key = def::qualified(&ctx.prefix, &f.name.name);
        let Some(sig) = table.fn_named(&key) else {
            continue;
        };
        let mut checker = Checker {
            defs: table,
            sig,
            fn_ast: f,
            scopes: vec![Vec::new()],
            diagnostics,
            goals: &mut goals,
            next_hole: &mut next_hole,
            probe_offset: None,
            probe: &mut probe,
            teach_budget,
            ctx: Some(ctx),
        };
        checker.check_fn();
    }
}

/// The most teaching blocks one check run may attach, first come first
/// served in diagnostic order (design/0009 §3: a total budget, so chained
/// failures cannot snowball into a wall of catalogues).
const MAX_TEACHES_PER_CHECK: usize = 5;

/// Teaching state shared across every function in the run — the budget
/// belongs to the check, not to one body.
pub(crate) struct TeachBudget {
    blocks_left: usize,
    /// Receiver types whose method catalogue has already been taught; one
    /// catalogue per type per run.
    catalogued: Vec<String>,
    /// `(receiver type, unknown member)` pairs whose module-call bridge has
    /// already been taught — the design/0012 §1 dedup key.
    module_called: Vec<(String, String)>,
}

impl TeachBudget {
    pub(crate) fn new() -> TeachBudget {
        TeachBudget {
            blocks_left: MAX_TEACHES_PER_CHECK,
            catalogued: Vec::new(),
            module_called: Vec::new(),
        }
    }
}

/// Everything body checking needs to know about the module it is inside
/// (design/0010): its own path, its declared dependencies, and the project's
/// public surface for the XN2002 use-fix. Absent in single-file mode.
pub struct ModuleCtx {
    /// Dotted path of this module ("main", "game.player").
    pub prefix: String,
    /// Declared `use`s, sorted by path — also the canonical insertion order
    /// for the use-fix (design/0010 §3). The span covers the whole item.
    pub uses: Vec<(String, Span)>,
    /// Modules consumed so far; signatures marked theirs during collection.
    pub used: std::cell::RefCell<std::collections::HashSet<String>>,
    /// Bare pub item name -> owning modules (sorted). Exact-match lookups
    /// only — candidates never enumerate the project (design/0010 §6).
    pub pub_index: std::collections::HashMap<String, Vec<String>>,
    /// Where a first `use` would go when the file has none yet.
    pub first_item_offset: u32,
}

impl ModuleCtx {
    pub fn is_used_module(&self, path: &str) -> bool {
        self.uses.iter().any(|(p, _)| p == path)
    }

    fn mark_used(&self, path: &str) {
        self.used.borrow_mut().insert(path.to_string());
    }

    /// The machine-applicable fix inserting `use path;` at the canonical
    /// position: among existing uses in dictionary order, or at the top.
    pub fn use_fix(&self, path: &str) -> Fix {
        let description = format!("insert `use {path};`");
        for (existing, span) in &self.uses {
            if existing.as_str() > path {
                return Fix::single(
                    description,
                    Edit::insert(span.start, format!("use {path};\n")),
                );
            }
        }
        match self.uses.last() {
            Some((_, span)) => Fix::single(
                description,
                Edit::insert(span.end, format!("\nuse {path};")),
            ),
            None => Fix::single(
                description,
                Edit::insert(self.first_item_offset, format!("use {path};\n\n")),
            ),
        }
    }
}

/// XN3011, spelled the same in single-file and project mode: the cycle in
/// order, closed back on its first member.
pub(crate) fn infinite_size_diagnostic(cycle: &[String], span: Span) -> Diagnostic {
    let first = &cycle[0];
    let mut chain = cycle.to_vec();
    chain.push(first.clone());
    Diagnostic::error(
        DiagCode::InfiniteSizeType,
        span,
        format!(
            "`{first}` contains itself by value ({}); box a link in the cycle \
             behind `Option`, `List` or `Map`",
            chain.join(" -> ")
        ),
    )
}

/// What a dotted chain of names turned out to be, once the module set had
/// its say. `NotModule` sends the caller back to its pre-module reading.
enum QualifiedLookup {
    NotModule,
    Fn(String),
    Variant(crate::ty::DefId, String),
    Type(crate::ty::DefId),
    /// A module reference that failed; the diagnostic is already out.
    Reported,
}

/// One callee at one call site: the name diagnostics use, its declared
/// parameters, and — when the callee is resolved — the signature teach that
/// rides the first argument-shape diagnostic (design/0009 §3).
struct Callee<'c> {
    name: &'c str,
    param_names: &'c [String],
    param_types: &'c [Type],
    teach: Option<Teach>,
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
    /// Byte offset being queried by `type-at`, if any.
    probe_offset: Option<u32>,
    probe: &'a mut Option<Probe>,
    teach_budget: &'a mut TeachBudget,
    /// The module being checked, in project mode. `None` is single-file
    /// mode, where nothing below changes behaviour.
    ctx: Option<&'a ModuleCtx>,
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

    /// Capture the checker's state for `type-at`, smallest containing span
    /// wins. Runs on every expression; a `None` probe offset makes it free.
    fn maybe_probe(&mut self, span: Span, ty: &Type) {
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
                allowed_effects: self.sig.effects.iter().map(String::from).collect(),
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
    fn scope_types(&self) -> Vec<(String, Type, bool)> {
        let mut seen = std::collections::HashSet::new();
        let mut out = Vec::new();
        for scope in self.scopes.iter().rev() {
            for binding in scope.iter().rev() {
                if seen.insert(binding.name.clone()) {
                    out.push((binding.name.clone(), binding.ty.clone(), binding.mutable));
                }
            }
        }
        out.reverse();
        out
    }

    fn push_goal(&mut self, name: Option<String>, span: Span, kind: &'static str, expected: &Type) {
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
        let (candidates, blocked) = crate::candidates::candidates_for(
            self.defs,
            expected,
            &self.scope_types(),
            &self.sig.effects,
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
            allowed_effects: self.sig.effects.iter().map(String::from).collect(),
            candidates,
            blocked,
        };
        self.goals.push(goal);
    }

    /// A goal with no meaningful expected type — type holes. No candidates.
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
            candidates: Vec::new(),
            blocked: Vec::new(),
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
        let uses_paths: Vec<String>;
        let resolver;
        let resolve = match self.ctx {
            Some(ctx) => {
                uses_paths = ctx.uses.iter().map(|(path, _)| path.clone()).collect();
                resolver = def::ResolveCtx {
                    prefix: &ctx.prefix,
                    uses: &uses_paths,
                    used: Some(&ctx.used),
                };
                Some(&resolver)
            }
            None => None,
        };
        let lowered = def::lower_type(ty, self.defs, &generics, self.diagnostics, resolve);
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

    /// Claim budget for one teaching block. `catalogue_of` names the
    /// receiver type for a method catalogue, which appears at most once per
    /// run however many diagnostics ask for it (design/0009 §3).
    fn claim_teach(&mut self, catalogue_of: Option<&str>) -> bool {
        if self.teach_budget.blocks_left == 0 {
            return false;
        }
        if let Some(type_name) = catalogue_of {
            if self.teach_budget.catalogued.iter().any(|t| t == type_name) {
                return false;
            }
            self.teach_budget.catalogued.push(type_name.to_string());
        }
        self.teach_budget.blocks_left -= 1;
        true
    }

    /// Claim budget for one module-call bridge. The dedup key is the pair
    /// `(receiver type, unknown member)` (design/0012 §1): the same wrong
    /// member on the same type teaches once per run, a different member
    /// earns its own bridge.
    fn claim_module_call_teach(&mut self, type_name: &str, member: &str) -> bool {
        if self.teach_budget.blocks_left == 0 {
            return false;
        }
        let key = (type_name.to_string(), member.to_string());
        if self.teach_budget.module_called.contains(&key) {
            return false;
        }
        self.teach_budget.module_called.push(key);
        self.teach_budget.blocks_left -= 1;
        true
    }

    /// `name(param: Type, ..) -> Ret uses {..}` — the spelling the
    /// producers query uses, so a taught signature and a queried one agree.
    fn signature_text(
        &self,
        name: &str,
        param_names: &[String],
        param_types: &[Type],
        ret: &Type,
        effects: &EffectSet,
    ) -> String {
        let params: Vec<String> = param_names
            .iter()
            .zip(param_types)
            .map(|(param, ty)| format!("{param}: {}", self.render(ty)))
            .collect();
        let mut text = format!("{name}({}) -> {}", params.join(", "), self.render(ret));
        if !effects.is_empty() {
            text.push_str(&format!(" uses {effects}"));
        }
        text
    }

    /// The receiver's methods with its generics substituted, in declaration
    /// order — `insert(key: String, value: Int) -> Option<Int>`, not the
    /// schematic form. `None` when there is nothing to teach.
    fn method_catalogue(&self, receiver: &Type, methods: &[MethodSig]) -> Option<Teach> {
        if methods.is_empty() {
            return None;
        }
        let bindings: Vec<(String, Type)> = match receiver {
            Type::Named { def, args } => self
                .defs
                .def(*def)
                .generics
                .iter()
                .cloned()
                .zip(args.iter().cloned())
                .collect(),
            _ => Vec::new(),
        };
        let items = methods
            .iter()
            .map(|method| {
                let names: Vec<String> = method.params.iter().map(|(n, _)| n.to_string()).collect();
                let types: Vec<Type> = method
                    .params
                    .iter()
                    .map(|(_, t)| t.substitute(&bindings))
                    .collect();
                TeachItem::new(
                    method.name,
                    self.signature_text(
                        method.name,
                        &names,
                        &types,
                        &method.ret.substitute(&bindings),
                        &method.effects,
                    ),
                )
            })
            .collect();
        Some(Teach::available_methods(self.render(receiver), items))
    }

    /// The defining module of a nominal receiver type, when there is one —
    /// the gate for the module-call teach (design/0012 §1). Prelude types
    /// are bare-named and answer `None`; so do type variables and poison,
    /// which never reach here as `Type::Named`.
    fn module_owner_of(&self, receiver: &Type) -> Option<String> {
        let Type::Named { def, .. } = receiver else {
            return None;
        };
        let name = self.defs.name_of(*def);
        name.rsplit_once('.').map(|(owner, _)| owner.to_string())
    }

    /// A function key spelled the way this module calls it: the current
    /// module's own items bare, everything else fully qualified. Unlike
    /// `display_fn` this strips only an exact module match, so a sibling
    /// nested module never renders half-qualified.
    fn call_spelling(&self, owner: &str, bare: &str) -> String {
        match self.ctx {
            Some(ctx) if ctx.prefix == owner => bare.to_string(),
            _ => format!("{owner}.{bare}"),
        }
    }

    /// The module-call bridge for an unknown member on a module-owned type
    /// (design/0012 §1): the defining module's `pub` functions whose input
    /// parameters take the receiver type directly. Return-only matches are
    /// excluded — the producers lesson (design/0009 §7) carried through.
    ///
    /// Ranking is deterministic: a function whose name equals the unknown
    /// member comes first (and therefore always fits the unchanged budget —
    /// the displacement invariant), then first-parameter matches, then other
    /// input positions, ties broken by fully-qualified name. Candidates are
    /// included or omitted whole: a signature over the byte budget drops the
    /// candidate rather than cutting the signature, and `total_items` keeps
    /// the omission structural.
    fn module_call_teach(&self, receiver: &Type, owner: &str, member: &str) -> Option<Teach> {
        let Type::Named {
            def: receiver_def, ..
        } = receiver
        else {
            return None;
        };
        // (tier, fully-qualified name, fn index, fitting input positions).
        let mut ranked: Vec<(u8, String, usize, Vec<usize>)> = Vec::new();
        for (index, sig) in self.defs.fns.iter().enumerate() {
            let Some((fn_owner, bare)) = sig.name.rsplit_once('.') else {
                continue;
            };
            if fn_owner != owner || !sig.is_pub {
                continue;
            }
            let fitting: Vec<usize> = sig
                .params
                .iter()
                .enumerate()
                .filter(|(_, (_, ty))| matches!(ty, Type::Named { def, .. } if def == receiver_def))
                .map(|(position, _)| position)
                .collect();
            if fitting.is_empty() {
                continue; // return-only or unrelated: no bridge to offer.
            }
            let tier = if bare == member {
                0
            } else if fitting[0] == 0 {
                1
            } else {
                2
            };
            ranked.push((tier, sig.name.clone(), index, fitting));
        }
        if ranked.is_empty() {
            return None;
        }
        ranked.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));

        let total_items = ranked.len();
        let mut items = Vec::new();
        for (_, key, index, fitting) in &ranked {
            if items.len() == MAX_TEACH_ITEMS {
                break;
            }
            let sig = &self.defs.fns[*index];
            let bare = key.rsplit_once('.').map(|(_, b)| b).unwrap_or(key);
            let spelled = self.call_spelling(owner, bare);
            let names: Vec<String> = sig.params.iter().map(|(n, _)| n.clone()).collect();
            let types: Vec<Type> = sig.params.iter().map(|(_, t)| t.clone()).collect();
            let text = self.signature_text(&spelled, &names, &types, &sig.ret, &sig.effects);
            if text.len() > MAX_SIGNATURE_BYTES {
                continue; // omit the whole candidate, never cut its signature.
            }
            let mut item = TeachItem::new(key.clone(), text);
            if let [only] = fitting.as_slice() {
                // Exactly one input position fits, so the rewrite is honest.
                // Several fitting positions stay signature-only: guessing
                // which one takes the receiver would be mis-guidance.
                item.receiver_parameter = Some(names[*only].clone());
                let args: Vec<String> = names
                    .iter()
                    .enumerate()
                    .map(|(position, name)| {
                        if position == *only {
                            format!("{name}: <receiver>")
                        } else {
                            format!("{name}: ...")
                        }
                    })
                    .collect();
                item.rewrite = Some(format!("{spelled}({})", args.join(", ")));
            }
            items.push(item);
        }
        if items.is_empty() {
            // Every candidate was over the byte budget: an empty block
            // teaches nothing and claims none (the 0009 catalogue rule).
            return None;
        }
        Some(Teach::module_call(
            self.render(receiver),
            items,
            total_items,
        ))
    }

    /// Bare type names resolve to the current module first, then the
    /// prelude — identical to plain lookup in single-file mode.
    fn lookup_type_name(&self, name: &str) -> Option<crate::ty::DefId> {
        if let Some(ctx) = self.ctx {
            if !ctx.prefix.is_empty() {
                if let Some(def) = self.defs.lookup(&def::qualified(&ctx.prefix, name)) {
                    return Some(def);
                }
            }
        }
        self.defs.lookup(name)
    }

    /// The table key for a bare function name: the current module's own
    /// function shadows nothing — prelude functions stay reachable because
    /// modules cannot redeclare them.
    fn fn_key(&self, bare: &str) -> Option<String> {
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
    fn display_fn(&self, key: &str) -> String {
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
    fn qualified_ref(&mut self, segments: &[String], span: Span) -> QualifiedLookup {
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
    fn try_qualified_call(
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

    /// XN5001: every value the scrutinee can hold must land on some arm.
    /// The witness is a concrete value no arm covers, in source syntax.
    fn check_exhaustiveness(&mut self, scrutinee: &Type, arms: &[ast::MatchArm], span: Span) {
        if let Some(found) = crate::exhaustive::missing_witness(self.defs, scrutinee, arms) {
            let message = format!("this `match` is not exhaustive: `{found}` is not covered");
            self.error(DiagCode::NonExhaustiveMatch, span, message);
        }
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

    // ----- function body -----

    fn check_fn(&mut self) {
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
    fn check(&mut self, expr: &ast::Expr, expected: &Type) {
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
                self.check_exhaustiveness(&scrutinee_ty, arms, expr.span);
            }

            ast::ExprKind::Block(block) => self.check_block(block, expected),

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
    fn synth(&mut self, expr: &ast::Expr) -> Type {
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

            ast::ExprKind::Field { receiver, name } => self.field(receiver, name, expr.span),

            ast::ExprKind::Await(inner) => {
                // Parsed for recovery, not shipped (design/0008 §1). The
                // operand is still synthesised so its own problems and goals
                // survive.
                let _ = self.synth(inner);
                self.error(
                    DiagCode::UnshippedConstruct,
                    expr.span,
                    "`.await` is not part of the language yet",
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
                self.check_exhaustiveness(&scrutinee_ty, arms, expr.span);
                result.unwrap_or(Type::Unit)
            }

            ast::ExprKind::Block(block) => self.synth_block(block),

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

            // Parsed for recovery, not shipped (design/0008 §1): a lambda's
            // effect set cannot be stated honestly until closure effect rules
            // land, and a dishonest one lets a captured capability escape as
            // a pure value. One diagnostic; the body is not descended into.
            ast::ExprKind::Lambda { .. } => {
                self.error(
                    DiagCode::UnshippedConstruct,
                    expr.span,
                    "closures are not part of the language yet; use a named function",
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
        if let Some(key) = self.fn_key(name) {
            let sig = self.defs.fn_named(&key).expect("key just resolved");
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

    /// `receiver.name` — a struct field, or `Enum.Variant`.
    fn field(&mut self, receiver: &ast::Expr, name: &ast::Ident, span: Span) -> Type {
        // `Rank.Gold`: the receiver is a type name, not a value.
        if let ast::ExprKind::Path(path) = &receiver.kind {
            if let [single] = path.segments.as_slice() {
                if self.lookup(&single.name).is_none() {
                    if let Some(def) = self.lookup_type_name(&single.name) {
                        return self.variant_ref(def, name, span);
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
                            let sig = self.defs.fn_named(&key).expect("resolved");
                            if !sig.generics.is_empty() {
                                self.error(
                                    DiagCode::AnnotationRequired,
                                    span,
                                    format!(
                                        "generic function `{key}` can only be called directly; \
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
                        QualifiedLookup::Variant(def, variant) => {
                            let ident = ast::Ident::new(variant, name.span);
                            return self.variant_ref(def, &ident, span);
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
    /// Empty `param_names` means positional (variant payloads, fn values).
    /// The callee's teach, when present, rides the first argument-shape
    /// diagnostic; the rest of the call site does not repeat it.
    fn check_args(
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

    fn call_named_fn(
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
    fn qualified_variant_target(
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

    fn method_call(
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
    fn foreign_owner(&self, def: crate::ty::DefId) -> Option<String> {
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
                // `type-at` on a binding name answers with the bound type —
                // the most natural question to ask about a `let`.
                self.maybe_probe(ident.span, scrutinee);
                self.bind(&ident.name, scrutinee.clone(), mutable);
            }

            ast::PatternKind::Literal(expr) => {
                let ty = self.synth(expr);
                self.require_compatible(&ty, scrutinee, pattern.span);
            }

            ast::PatternKind::Path(path) => {
                // `game.player.Rank.Gold` — the module prefix resolves
                // first, the enum and variant follow as before.
                if path.segments.len() >= 3 && self.ctx.is_some() {
                    let names: Vec<String> = path.segments.iter().map(|s| s.name.clone()).collect();
                    match self.qualified_ref(&names, pattern.span) {
                        QualifiedLookup::Variant(def, _) => {
                            let pattern_ty = Type::Named {
                                def,
                                args: match scrutinee {
                                    Type::Named { def: s, args } if *s == def => args.clone(),
                                    _ => vec![],
                                },
                            };
                            self.require_compatible(&pattern_ty, scrutinee, pattern.span);
                            return;
                        }
                        QualifiedLookup::Reported => return,
                        _ => {}
                    }
                }
                // `Rank.Gold` — enum and variant named explicitly.
                let (Some(enum_ident), Some(variant_ident)) =
                    (path.segments.first(), path.segments.get(1))
                else {
                    return;
                };
                let Some(def) = self.lookup_type_name(&enum_ident.name) else {
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
                        let Some(def) = self.lookup_type_name(&enum_ident.name) else {
                            self.error(
                                DiagCode::UnknownType,
                                enum_ident.span,
                                format!("`{}` does not name a type", enum_ident.name),
                            );
                            return;
                        };
                        (def, variant_ident.name.clone())
                    }
                    segments => {
                        // `game.player.Rank.Gold(payload)`.
                        let names: Vec<String> = segments.iter().map(|s| s.name.clone()).collect();
                        match self.qualified_ref(&names, pattern.span) {
                            QualifiedLookup::Variant(def, variant) => (def, variant),
                            QualifiedLookup::Reported => {
                                for element in elements {
                                    self.bind_pattern(element, &Type::Error, mutable);
                                }
                                return;
                            }
                            _ => return,
                        }
                    }
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
                let def = if path.segments.len() >= 2 && self.ctx.is_some() {
                    // `game.player.Player { .. }` in pattern position.
                    let names: Vec<String> = path.segments.iter().map(|s| s.name.clone()).collect();
                    match self.qualified_ref(&names, pattern.span) {
                        QualifiedLookup::Type(def) => def,
                        QualifiedLookup::Reported => return,
                        _ => {
                            self.error(
                                DiagCode::UnknownType,
                                pattern.span,
                                format!("`{}` does not name a type", names.join(".")),
                            );
                            return;
                        }
                    }
                } else if let Some(def) = self.lookup_type_name(&first.name) {
                    def
                } else {
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

/// The unique nearest name within two edits of `written`, or nothing.
///
/// A tie is silence: suggesting one of two equally close names is a coin
/// toss presented as knowledge (design/0009 §3, the cursor minimal form).
/// Duplicate candidates collapse first so a name shadowed in two scopes
/// cannot tie with itself.
fn did_you_mean<I>(written: &str, candidates: I) -> Option<String>
where
    I: IntoIterator<Item = String>,
{
    let mut seen: Vec<String> = Vec::new();
    let mut best: Option<(usize, String)> = None;
    let mut tied = false;
    for candidate in candidates {
        if seen.contains(&candidate) {
            continue;
        }
        seen.push(candidate.clone());
        let distance = edit_distance(written, &candidate);
        match &best {
            Some((held, _)) if distance > *held => {}
            Some((held, _)) if distance == *held => tied = true,
            _ => {
                best = Some((distance, candidate));
                tied = false;
            }
        }
    }
    match best {
        Some((distance, name)) if distance <= 2 && !tied => Some(name),
        _ => None,
    }
}

/// Restricted Damerau-Levenshtein (optimal string alignment): insert,
/// delete, substitute, and adjacent transposition each cost one. Counted in
/// characters, case not folded — the naming rules make case meaningful.
fn edit_distance(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    // Three rolling rows; the row before last is what prices transposition.
    let mut before_prev: Vec<usize> = vec![0; b.len() + 1];
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut current: Vec<usize> = vec![0; b.len() + 1];
    for i in 1..=a.len() {
        current[0] = i;
        for j in 1..=b.len() {
            let cost = usize::from(a[i - 1] != b[j - 1]);
            current[j] = (prev[j] + 1)
                .min(current[j - 1] + 1)
                .min(prev[j - 1] + cost);
            if i > 1 && j > 1 && a[i - 1] == b[j - 2] && a[i - 2] == b[j - 1] {
                current[j] = current[j].min(before_prev[j - 2] + 1);
            }
        }
        std::mem::swap(&mut before_prev, &mut prev);
        std::mem::swap(&mut prev, &mut current);
    }
    prev[b.len()]
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
