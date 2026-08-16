//! The tree-walking interpreter.
//!
//! Values are values: binding, passing and returning copy them, which *is*
//! the kernel's value semantics — with no observable aliasing of unique
//! values, a copy and a move are indistinguishable, and the checker owns the
//! job of making wasteful patterns visible later.
//!
//! Control flow rides `Result`: the error side carries early `return`,
//! `break`, `continue`, and runtime traps. Traps are precise and carry a
//! span — a runtime error message with no position is a bug report nobody
//! can act on.

mod calls;
mod eval;
mod place;
mod tasks;
mod value;

use std::sync::Arc;

use xenith_diag::Span;
use xenith_sema::def::DefTable;
use xenith_sema::ty::DefId;
use xenith_syntax::ast;

pub use value::{Body, Value};

use tasks::{Child, Pool};

/// Why evaluation stopped early.
enum Control<'a> {
    Return(Value<'a>),
    Break,
    Continue,
    Trap {
        message: String,
        span: Span,
    },
    /// A child was told to stop: a sibling's trap already decided the
    /// program's fate, and this task's result can no longer matter
    /// (design/0017 §3). Only a child interpreter ever raises this; it never
    /// escapes the task it belongs to.
    Cancelled,
}

type Eval<'a, T> = Result<T, Control<'a>>;

fn trap<'a, T>(span: Span, message: impl Into<String>) -> Eval<'a, T> {
    Err(Control::Trap {
        message: message.into(),
        span,
    })
}

// ------------------------------------------------------------------ outcome

pub struct Outcome {
    /// 0 = `main` succeeded; 1 = `main` returned `Err`; 101 = a trap fired.
    pub exit: i32,
    pub stdout: Vec<u8>,
    /// The trap, when exit is 101.
    pub error: Option<(String, Span)>,
}

/// Which executor runs the children of a `scope` (design/0017).
///
/// `Sequential` is the pre-0017 engine — a child runs to completion at its
/// spawn point, on this thread. It is kept deliberately: it is the
/// differential oracle the parallel executor is tested against (design/0017
/// §5), not dead code. `Parallel` is the shipped default.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Executor {
    Parallel,
    Sequential,
}

impl Executor {
    /// The internal switch. `XENITH_EXECUTOR=sequential` picks the oracle;
    /// anything else (including absence) is the shipped executor.
    pub fn from_env() -> Executor {
        match std::env::var("XENITH_EXECUTOR").as_deref() {
            Ok("sequential") => Executor::Sequential,
            _ => Executor::Parallel,
        }
    }
}

/// Run `fn main` of a checked module. The caller is responsible for having
/// refused a module with diagnostics; holes type-check clean and are allowed
/// through — reaching one is a precise trap, which is the workflow.
pub fn run(module: &ast::Module, table: &DefTable) -> Outcome {
    run_with(module, table, Executor::from_env())
}

/// [`run`] with the executor named explicitly — the differential harness.
pub fn run_with(module: &ast::Module, table: &DefTable, executor: Executor) -> Outcome {
    let units = [(String::new(), module)];
    run_units(&units, table, 0, executor)
}

/// Run a project: the entry is the `main` module (design/0010 §2). A
/// project without one is a library — it checks, and running it is this
/// precise refusal rather than a hunt for a stray `fn main`.
pub fn run_project<'a>(modules: &'a [(String, &'a ast::Module)], table: &'a DefTable) -> Outcome {
    run_project_with(modules, table, Executor::from_env())
}

/// [`run_project`] with the executor named explicitly.
pub fn run_project_with<'a>(
    modules: &'a [(String, &'a ast::Module)],
    table: &'a DefTable,
    executor: Executor,
) -> Outcome {
    match modules.iter().position(|(path, _)| path == "main") {
        Some(entry) => run_units(modules, table, entry, executor),
        None => Outcome {
            exit: 101,
            stdout: Vec::new(),
            error: Some((
                "no `src/main.xn` — a library project checks but does not run".to_string(),
                Span::EMPTY,
            )),
        },
    }
}

/// Might any function in the set spawn? `spawn` is the effect `Task.spawn`,
/// which its enclosing function must declare (design/0015 §5), and `run`
/// refuses a program with diagnostics — so a program with no such clause
/// cannot reach a `spawn`, and never gets a thread.
///
/// A false negative would cost nothing but speed: a `spawn` with no pool
/// falls back to running the child in place, which is the sequential
/// executor.
fn may_spawn(modules: &[(String, &ast::Module)]) -> bool {
    modules.iter().any(|(_, module)| {
        module.items.iter().any(|item| match &item.kind {
            ast::ItemKind::Fn(f) => f.effects.as_ref().is_some_and(|set| {
                set.effects
                    .iter()
                    .any(|path| path.segments.first().is_some_and(|s| s.name == "Task"))
            }),
            _ => false,
        })
    })
}

fn run_units<'a>(
    modules: &'a [(String, &'a ast::Module)],
    table: &'a DefTable,
    entry: usize,
    executor: Executor,
) -> Outcome {
    if executor == Executor::Sequential || !may_spawn(modules) {
        return enter_main(modules, table, entry, None);
    }
    // One thread scope for the whole run: the workers borrow the syntax tree
    // and the definition table, both of which outlive it.
    std::thread::scope(|threads| {
        let Some((pool, workers)) = Pool::start(threads) else {
            return enter_main(modules, table, entry, None);
        };
        let outcome = enter_main(modules, table, entry, Some(pool));
        // Dropping the last sender is the shutdown signal; the scope joins
        // the workers on the way out. A child still running because nobody
        // cancelled it keeps the process alive — which is exactly what the
        // sequential executor does when a child diverges.
        drop(workers);
        outcome
    })
}

fn enter_main<'a>(
    modules: &'a [(String, &'a ast::Module)],
    table: &'a DefTable,
    entry: usize,
    pool: Option<Pool<'a>>,
) -> Outcome {
    let mut interp = Interp {
        table,
        modules,
        current: entry,
        stdout: Vec::new(),
        pool,
        cancel: None,
        children: Vec::new(),
        committed: 0,
        regions: Vec::new(),
    };

    let Some(main) = find_fn(modules[entry].1, "main") else {
        return Outcome {
            exit: 101,
            stdout: Vec::new(),
            error: Some((
                "no `fn main` to run — a program starts there".to_string(),
                Span::EMPTY,
            )),
        };
    };

    // main's parameters are its capabilities; there is nowhere else a
    // capability can come from.
    let mut env = Env::new();
    for param in &main.params {
        let capability = match capability_name(&param.ty) {
            Some(name) => Value::Capability(name),
            None => {
                return Outcome {
                    exit: 101,
                    stdout: interp.stdout,
                    error: Some((
                        format!(
                            "`main` takes capabilities only; `{}` is not one",
                            param.name.name
                        ),
                        param.span,
                    )),
                };
            }
        };
        env.bind(&param.name.name, capability);
    }

    let result = match &main.body {
        Some(body) => interp.block(body, &mut env),
        None => Ok(Value::Unit),
    };

    match result {
        Ok(value) | Err(Control::Return(value)) => {
            let exit = match &value {
                Value::Enum { def, variant, .. }
                    if *def == table.result && *variant == err_index() =>
                {
                    1
                }
                _ => 0,
            };
            Outcome {
                exit,
                stdout: interp.stdout,
                error: None,
            }
        }
        Err(Control::Trap { message, span }) => Outcome {
            exit: 101,
            stdout: interp.stdout,
            error: Some((message, span)),
        },
        Err(Control::Break) | Err(Control::Continue) => Outcome {
            exit: 101,
            stdout: interp.stdout,
            error: Some((
                "`break` or `continue` escaped every loop — checker gap".to_string(),
                Span::EMPTY,
            )),
        },
        // Only a child is ever cancelled, and a child's `Cancelled` is
        // consumed by the job that ran it.
        Err(Control::Cancelled) => Outcome {
            exit: 101,
            stdout: interp.stdout,
            error: Some((
                "the main program was cancelled — interpreter gap".to_string(),
                Span::EMPTY,
            )),
        },
    }
}

fn find_fn<'a>(module: &'a ast::Module, name: &str) -> Option<&'a ast::FnItem> {
    module.items.iter().find_map(|item| match &item.kind {
        ast::ItemKind::Fn(f) if f.name.name == name => Some(f),
        _ => None,
    })
}

/// A `const` item of `module`. Its value is the initializer expression,
/// evaluated here like any other literal: the checker already proved it is a
/// literal or arithmetic over literals, and folded the integer part to catch
/// overflow, so this evaluation cannot fail on a checked program.
fn find_const<'a>(module: &'a ast::Module, name: &str) -> Option<&'a ast::ConstItem> {
    module.items.iter().find_map(|item| match &item.kind {
        ast::ItemKind::Const(c) if c.name.name == name => Some(c),
        _ => None,
    })
}

fn capability_name(ty: &ast::Type) -> Option<&'static str> {
    if let ast::TypeKind::Named { path, args } = &ty.kind {
        if args.is_empty() && path.segments.len() == 1 {
            // The runtime knows how to service exactly these.
            return match path.segments[0].name.as_str() {
                "Io" => Some("Io"),
                _ => None,
            };
        }
    }
    None
}

/// `Ok` is variant 0, `Err` is 1, `Some` is 0, `None` is 1 — fixed by the
/// prelude's declaration order in `def.rs`.
fn ok_index() -> usize {
    0
}
fn err_index() -> usize {
    1
}
fn some_index() -> usize {
    0
}
fn none_index() -> usize {
    1
}

// -------------------------------------------------------------- environment

struct Env<'a> {
    scopes: Vec<Vec<(String, Value<'a>)>>,
}

impl<'a> Env<'a> {
    fn new() -> Env<'a> {
        Env {
            scopes: vec![Vec::new()],
        }
    }

    fn bind(&mut self, name: &str, value: Value<'a>) {
        if name.is_empty() {
            return;
        }
        self.scopes
            .last_mut()
            .expect("one scope")
            .push((name.to_string(), value));
    }

    fn get(&self, name: &str) -> Option<&Value<'a>> {
        self.scopes
            .iter()
            .rev()
            .flat_map(|scope| scope.iter().rev())
            .find(|(n, _)| n == name)
            .map(|(_, v)| v)
    }

    fn get_mut(&mut self, name: &str) -> Option<&mut Value<'a>> {
        self.scopes
            .iter_mut()
            .rev()
            .flat_map(|scope| scope.iter_mut().rev())
            .find(|(n, _)| n == name)
            .map(|(_, v)| v)
    }

    /// Everything visible, innermost occurrence winning — a lambda captures
    /// this by value.
    fn snapshot(&self) -> Vec<(String, Value<'a>)> {
        let mut seen = std::collections::HashSet::new();
        let mut out = Vec::new();
        for scope in self.scopes.iter().rev() {
            for (name, value) in scope.iter().rev() {
                if seen.insert(name.clone()) {
                    out.push((name.clone(), value.clone()));
                }
            }
        }
        out.reverse();
        out
    }
}

// ------------------------------------------------------------- interpreter

struct Interp<'a> {
    table: &'a DefTable,
    /// (module path, ast) — one entry, path "", in single-file mode.
    modules: &'a [(String, &'a ast::Module)],
    /// The module whose bare names are currently in effect; `apply` swaps
    /// it to the callee's home for the duration of the call.
    current: usize,
    stdout: Vec<u8>,
    /// Where children run. `None` is the sequential executor: a child runs
    /// to completion at its spawn point, on this thread.
    pool: Option<Pool<'a>>,
    /// Set in a child interpreter only. Polled at safe points; when it is
    /// raised a sibling's trap has already decided the program, and this
    /// task unwinds so a diverging child can be reclaimed (design/0017 §3).
    cancel: Option<Arc<std::sync::atomic::AtomicBool>>,
    /// Every child submitted and not yet retired, in **spawn order** — one
    /// list for the whole interpreter, not one per region, because spawn
    /// order is global. A child spawned in an outer scope was spawned before
    /// one spawned in a nested scope, and sequentially its fate was sealed
    /// first; committing per region would let the inner one's trap overtake
    /// it (design/0017 §3).
    children: Vec<Child<'a>>,
    /// `children[..committed]` have had their fate resolved.
    committed: usize,
    /// Open `scope { .. }` regions, innermost last: where each one's own
    /// children start in `children`.
    regions: Vec<usize>,
}

/// What a dotted chain resolved to at runtime. No `use` gate here: the
/// checker enforced it, and `run` refuses files with diagnostics.
enum RuntimeRef {
    Fn(usize, String),
    Const(usize, String),
    Variant(DefId, String),
}

impl<'a> Interp<'a> {
    fn current_module(&self) -> &'a ast::Module {
        self.modules[self.current].1
    }

    /// Bare type names resolve to the current module first, then the
    /// prelude — the runtime twin of the checker's rule.
    fn lookup_type(&self, name: &str) -> Option<DefId> {
        let prefix = self.modules[self.current].0.as_str();
        if !prefix.is_empty() {
            if let Some(def) = self.table.lookup(&format!("{prefix}.{name}")) {
                return Some(def);
            }
        }
        self.table.lookup(name)
    }

    /// Longest-module-prefix resolution of a dotted chain: a foreign
    /// function or a qualified enum variant.
    fn runtime_ref(&self, segments: &[String]) -> Option<RuntimeRef> {
        for split in (1..segments.len()).rev() {
            let module = segments[..split].join(".");
            if let Some(index) = self.modules.iter().position(|(path, _)| *path == module) {
                let rest = &segments[split..];
                return match rest {
                    [item] => find_fn(self.modules[index].1, item)
                        .map(|_| RuntimeRef::Fn(index, item.clone()))
                        .or_else(|| {
                            find_const(self.modules[index].1, item)
                                .map(|_| RuntimeRef::Const(index, item.clone()))
                        }),
                    [enum_name, variant] => {
                        let def = self.table.lookup(&format!("{module}.{enum_name}"))?;
                        self.table
                            .variant_named(def, variant)
                            .map(|_| RuntimeRef::Variant(def, variant.clone()))
                    }
                    _ => None,
                };
            }
        }
        None
    }

    /// A named function of `home` as a value, ready for `apply`.
    fn fn_value(&mut self, home: usize, name: &str, span: Span) -> Eval<'a, Value<'a>> {
        let Some(f) = find_fn(self.modules[home].1, name) else {
            return trap(span, format!("no function `{name}` at runtime"));
        };
        let Some(body) = &f.body else {
            return trap(span, format!("`{name}` has no body"));
        };
        Ok(Value::Fn {
            params: Arc::new(f.params.iter().map(|p| p.name.name.clone()).collect()),
            body: Body::Block(body),
            captured: Arc::new(Vec::new()),
            is_async: f.is_async,
            home,
        })
    }

    /// A `const` of `home` as a value. Constant expressions name nothing, so
    /// the initializer evaluates in an empty environment and the module it
    /// was written in never has to be swapped in.
    fn const_value(&mut self, home: usize, name: &str, span: Span) -> Eval<'a, Value<'a>> {
        let Some(item) = find_const(self.modules[home].1, name) else {
            return trap(span, format!("no const `{name}` at runtime"));
        };
        let mut env = Env::new();
        self.eval(&item.value, &mut env)
    }
}
