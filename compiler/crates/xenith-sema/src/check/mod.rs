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

mod calls;
mod closures;
mod effects;
mod expr;
mod holes;
mod patterns;
mod resolve;
mod tasks;
mod teach;

use xenith_diag::{DiagCode, Diagnostic, Edit, Fix, Span};
use xenith_syntax::ast;

use crate::def::{self, DefTable, FnSig};
use crate::ty::{Type, TypeName};

pub(crate) use teach::TeachBudget;

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
            closures: Vec::new(),
            loop_depth: 0,
            initializing: Vec::new(),
            scope_depth: 0,
            flights: Vec::new(),
            joins: Vec::new(),
            in_guard: false,
        };
        checker.check_fn();
    }

    goals.sort_by_key(|g| g.span.start);
    (Analysis { diagnostics, goals }, probe)
}

/// A probe riding one module's body check: the offset asked about, and where
/// the answer lands. Project mode's `type_at` (design/0013 §1).
pub(crate) struct BodyProbe<'a> {
    pub offset: u32,
    pub out: &'a mut Option<Probe>,
}

/// Body checking for one project module: the walk `analyze` does, with the
/// module context wired in. Goals land in `goals`, sorted by span like the
/// single-file path — project-mode goals are how a confined server answers
/// `goals` honestly instead of falling back to one file (design/0013 §1).
pub(crate) fn check_module_bodies(
    table: &DefTable,
    module: &ast::Module,
    ctx: &ModuleCtx,
    teach_budget: &mut TeachBudget,
    diagnostics: &mut Vec<Diagnostic>,
    goals: &mut Vec<Goal>,
    probe: Option<BodyProbe>,
) {
    let mut next_hole = 0u32;
    let (probe_offset, mut probe_out) = match probe {
        Some(probe) => (Some(probe.offset), Some(probe.out)),
        None => (None, None),
    };
    let mut unprobed = None;
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
            goals,
            next_hole: &mut next_hole,
            probe_offset,
            probe: match &mut probe_out {
                Some(out) => out,
                None => &mut unprobed,
            },
            teach_budget,
            ctx: Some(ctx),
            closures: Vec::new(),
            loop_depth: 0,
            initializing: Vec::new(),
            scope_depth: 0,
            flights: Vec::new(),
            joins: Vec::new(),
            in_guard: false,
        };
        checker.check_fn();
    }
    goals.sort_by_key(|g| g.span.start);
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

struct Binding {
    name: String,
    ty: Type,
    mutable: bool,
    /// Index into [`Checker::joins`] when this binding holds a task handle
    /// (design/0015 §4). A Join is not a value: the only legal read is as
    /// the receiver of `.await`, and the dataflow states live in the side
    /// table so branches can snapshot and merge them.
    join: Option<usize>,
}

/// The consumption state of one task handle (design/0015 §4): `.await`
/// happens exactly once on every path.
#[derive(Clone, Copy, PartialEq, Eq)]
enum JoinState {
    /// Bound, not yet awaited.
    Live,
    /// Awaited; a second await is XN6006.
    Consumed,
    /// Already reported (escape, double await, partial await) — every later
    /// rule stays silent, the one-mistake-one-diagnostic discipline.
    Poisoned,
}

/// One task handle: what the child returns, where it was bound, and how far
/// the exactly-once dataflow has progressed.
struct JoinInfo {
    name: String,
    result: Type,
    state: JoinState,
    binding_span: Span,
    /// `Checker::loop_depth` at the `spawn` — an await at greater depth sits
    /// in a loop that may run it more than once.
    created_loop_depth: u32,
}

/// One `scope { .. }` region being checked, for the in-flight rule
/// (design/0017 §1).
///
/// A region is *in flight* from its first `spawn` until every task that
/// spawn created has been consumed: the handles bound in it are all
/// `Consumed`, and no statement-form spawn is outstanding. The statement
/// form binds no handle, so the scope's closing brace is the only thing that
/// joins it — which is why it keeps the region in flight to the end.
#[derive(Default)]
struct ScopeFlight {
    /// Indices into [`Checker::joins`] of the handles spawned in this
    /// region. Their consumption states live there, so branch snapshots and
    /// merges already maintain them — no second dataflow.
    joins: Vec<usize>,
    /// Statement-form spawns of this region, joined only at its exit.
    fired: u32,
    /// Already reported here — one mistake, one diagnostic (design/0009).
    reported: bool,
}

/// One closure body being checked (design/0014). The stack of these is what
/// makes the two pillars positional: any scope below `boundary` is outside
/// the closure, so referencing it is a capture, and a non-empty stack means
/// the effect budget is the empty set.
struct ClosureCtx {
    /// Index of the closure's parameter scope in `Checker::scopes`; every
    /// scope below it belongs to the enclosing function.
    boundary: usize,
    /// `Checker::loop_depth` at entry — a `break` at this depth would cross
    /// the closure boundary.
    entry_loop_depth: u32,
    /// Capture names already diagnosed, so one bad capture reports once
    /// however often the body mentions it.
    reported: Vec<String>,
    /// A task construct (`scope` / `spawn` / `.await`) was already refused
    /// in this body — the rest stay silent (design/0015 §5).
    task_reported: bool,
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
    /// Closure bodies currently being checked, innermost last (design/0014).
    closures: Vec<ClosureCtx>,
    /// `while` nesting depth, for the closure early-exit rule.
    loop_depth: u32,
    /// Names the `let` currently being checked will bind — referencing one
    /// from a closure is XN4007, definite initialization.
    initializing: Vec<String>,
    /// `scope { .. }` nesting depth (design/0015 §1) — `spawn` is legal only
    /// when this is non-zero.
    scope_depth: u32,
    /// The enclosing `scope { .. }` regions, innermost last — one entry per
    /// unit of `scope_depth`. Carries the in-flight state of design/0017 §1.
    flights: Vec<ScopeFlight>,
    /// Every task handle of this function, in creation order. Branch merges
    /// snapshot and restore the states by index.
    joins: Vec<JoinInfo>,
    /// Inside a `match` guard, which may run for several arms — an await
    /// there cannot be exactly-once (design/0015 §4).
    in_guard: bool,
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

    /// [`Checker::lookup`], also answering *which* scope holds the binding —
    /// the fact the capture rule turns on.
    fn lookup_indexed(&self, name: &str) -> Option<(usize, &Binding)> {
        for (index, scope) in self.scopes.iter().enumerate().rev() {
            if let Some(binding) = scope.iter().rev().find(|b| b.name == name) {
                return Some((index, binding));
            }
        }
        None
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
                join: None,
            });
    }

    fn scoped<T>(&mut self, f: impl FnOnce(&mut Self) -> T) -> T {
        self.scopes.push(Vec::new());
        let out = f(self);
        self.scopes.pop();
        out
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
}
