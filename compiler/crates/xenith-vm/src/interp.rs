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

use std::sync::Arc;

use xenith_diag::Span;
use xenith_sema::def::{DefKind, DefTable};
use xenith_sema::ty::DefId;
use xenith_syntax::ast;

// ------------------------------------------------------------------- values

/// A runtime value.
///
/// Every aggregate arm holds its payload behind an [`Arc`], and every write
/// path goes through [`Arc::make_mut`] — copy-on-write (design/0017 §4).
/// This is the "implementation may share storage under the hood" clause of
/// spec/04 §1 taken up: a copy is O(1) until somebody writes, and the write
/// uniquifies **the whole path** it walks, so reading a value out of a
/// container still yields an independent value (D1). An implementation that
/// uniquified only the outermost node and then wrote through a shared inner
/// node would be a bug, not an optimisation.
///
/// The arms are also all `Send`, statically ([`VALUE_IS_SEND`]): design/0017
/// §3 runs children on real threads, and the type system — not a comment —
/// is what keeps `Rc` and interior mutability out.
#[derive(Clone, Debug)]
pub enum Value<'a> {
    Int(i64),
    Float(f64),
    Bool(bool),
    Str(Arc<String>),
    Char(char),
    Unit,
    /// A `List<T>` value. Reads copy (design/0007 D1); only `push`, `pop`
    /// and `replace` write through the receiver in place.
    List(Arc<Vec<Value<'a>>>),
    /// A `Map<K, V>` value in insertion order — the order is normative
    /// (design/0007 §3), so pairs beat a hash table at this scale.
    Map(Arc<Vec<(Value<'a>, Value<'a>)>>),
    /// A value of the opaque prelude `Error` type. The message exists for
    /// debug rendering; nothing in the language reads it back out.
    ErrorValue(Arc<String>),
    Struct {
        def: DefId,
        /// Field values in declaration order.
        fields: Arc<Vec<Value<'a>>>,
    },
    Enum {
        def: DefId,
        variant: usize,
        payload: Arc<Vec<Value<'a>>>,
    },
    /// A function value: a lambda, a named function, or reference thereto.
    Fn {
        params: Arc<Vec<String>>,
        body: Body<'a>,
        captured: Arc<Vec<(String, Value<'a>)>>,
        is_async: bool,
        /// Index of the module whose bare names the body resolves against.
        home: usize,
    },
    /// A variant constructor used as a value: `ScoreError.NotFound`.
    VariantCtor {
        def: DefId,
        variant: usize,
        arity: usize,
    },
    /// A capability handed to `main`. The name is the prelude type ("Io").
    Capability(&'static str),
    /// The result of calling an `async fn`, and the handle the sequential
    /// executor hands back from `spawn`: the body has already run, and
    /// `.await` unwraps.
    Task(Arc<Value<'a>>),
    /// The handle the parallel executor hands back from `spawn`: the
    /// child's position in the run's spawn order. `.await` commits every
    /// outcome up to and including it (design/0017 §3).
    Pending {
        index: usize,
    },
}

/// `Value` crosses thread boundaries in the parallel executor (design/0017
/// §3), so `Send` is a compile-time obligation, not a review note. An `Rc`
/// or a `Cell` smuggled into any arm breaks this line.
const VALUE_IS_SEND: () = {
    const fn assert_send<T: Send>() {}
    assert_send::<Value<'static>>();
};
const _: () = VALUE_IS_SEND;

impl<'a> Value<'a> {
    /// Wrap an owned `String` as a value. The `Arc` is the sharing, not the
    /// semantics: `String` has no mutating method in the language.
    fn str(text: impl Into<String>) -> Value<'a> {
        Value::Str(Arc::new(text.into()))
    }

    fn list(items: Vec<Value<'a>>) -> Value<'a> {
        Value::List(Arc::new(items))
    }

    fn map(entries: Vec<(Value<'a>, Value<'a>)>) -> Value<'a> {
        Value::Map(Arc::new(entries))
    }

    fn error_value(message: impl Into<String>) -> Value<'a> {
        Value::ErrorValue(Arc::new(message.into()))
    }

    fn structure(def: DefId, fields: Vec<Value<'a>>) -> Value<'a> {
        Value::Struct {
            def,
            fields: Arc::new(fields),
        }
    }

    fn enumeration(def: DefId, variant: usize, payload: Vec<Value<'a>>) -> Value<'a> {
        Value::Enum {
            def,
            variant,
            payload: Arc::new(payload),
        }
    }
}

/// Take the owned payload out of an `Arc`, copying only when it is shared.
/// The copy is what keeps D1 honest when a value is consumed by move.
fn owned<T: Clone>(shared: Arc<T>) -> T {
    Arc::try_unwrap(shared).unwrap_or_else(|shared| (*shared).clone())
}

#[derive(Clone, Debug)]
pub enum Body<'a> {
    Block(&'a ast::Block),
    Expr(&'a ast::Expr),
}

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

/// How many OS threads may run children at once.
///
/// A fixed pool, not one thread per `spawn` (design/0017 §3): the number of
/// simultaneously live children is already bounded lexically — a `Join`
/// cannot be stored or escape, so it is capped by the unconsumed `let`
/// bindings written in the source — but a cap that does not depend on the
/// program is the belt to that braces. Excess children queue in submission
/// order, which is also the order their outcomes commit in, so the cap
/// cannot change what a program does.
const MAX_TASK_THREADS: usize = 4;

/// Stack for a task thread. Deep recursion inside a child would otherwise
/// meet a smaller stack than the main thread's; host exhaustion is outside
/// the determinism promise (design/0017 §4), but there is no reason to make
/// a child fail where the same call chain succeeds in the parent.
const TASK_STACK_BYTES: usize = 16 * 1024 * 1024;

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

// ------------------------------------------------------------- task threads

/// One unit of child work handed to the pool.
type Job<'a> = Box<dyn FnOnce() + Send + 'a>;

/// What a child handed back. Its stdout is provably empty — a child declares
/// `uses {}` and no capability crosses the boundary (design/0015 §2) — but
/// it is carried and appended in commit order anyway, so that a checker gap
/// would produce a wrong answer deterministically rather than a racy one.
struct ChildOutcome<'a> {
    stdout: Vec<u8>,
    result: ChildResult<'a>,
}

enum ChildResult<'a> {
    Value(Value<'a>),
    /// Only the message: the trap is reported at the *spawn site*, which is
    /// where the sequential executor reports it too.
    Trap(String),
    Cancelled,
}

/// The submission side of the task pool.
#[derive(Clone)]
struct Pool<'a> {
    jobs: std::sync::mpsc::Sender<Job<'a>>,
    /// Set when a committed child trap has decided the program's fate. Every
    /// child polls it at its safe points and unwinds (design/0017 §3).
    stop: Arc<std::sync::atomic::AtomicBool>,
}

impl<'a> Pool<'a> {
    /// Start up to [`MAX_TASK_THREADS`] workers on `threads`. The returned
    /// sender is the shutdown handle: dropping every clone ends the workers.
    /// `None` means the host would not give us a single thread, and the
    /// caller falls back to the sequential executor.
    #[allow(clippy::type_complexity)]
    fn start<'s>(
        threads: &'s std::thread::Scope<'s, 'a>,
    ) -> Option<(Pool<'a>, std::sync::mpsc::Sender<Job<'a>>)>
    where
        'a: 's,
    {
        let (tx, rx) = std::sync::mpsc::channel::<Job<'a>>();
        let rx = Arc::new(std::sync::Mutex::new(rx));
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let mut started = 0;
        for _ in 0..MAX_TASK_THREADS {
            let rx = Arc::clone(&rx);
            let spawned = std::thread::Builder::new()
                .stack_size(TASK_STACK_BYTES)
                .spawn_scoped(threads, move || {
                    loop {
                        // The queue is FIFO and only one worker waits on it
                        // at a time, so jobs start in submission order —
                        // which is what makes the cap unobservable.
                        let job = rx.lock().expect("task queue").recv();
                        match job {
                            Ok(job) => job(),
                            Err(_) => break,
                        }
                    }
                });
            if spawned.is_err() {
                break;
            }
            started += 1;
        }
        if started == 0 {
            return None;
        }
        Some((
            Pool {
                jobs: tx.clone(),
                stop,
            },
            tx,
        ))
    }
}

/// One child submitted to the pool.
struct Child<'a> {
    /// The callee as written, for trap attribution.
    name: String,
    /// The spawn site, so a child trap points where the sequential executor
    /// pointed.
    span: Span,
    outcome: std::sync::mpsc::Receiver<ChildOutcome<'a>>,
    /// Filled once committed, taken by `.await`.
    value: Option<Value<'a>>,
}

/// The dotted names of a pure field chain, for module-path resolution.
fn expr_segments(expr: &ast::Expr) -> Option<Vec<String>> {
    match &expr.kind {
        ast::ExprKind::Path(path) => Some(path.segments.iter().map(|s| s.name.clone()).collect()),
        ast::ExprKind::Field { receiver, name } => {
            let mut segments = expr_segments(receiver)?;
            segments.push(name.name.clone());
            Some(segments)
        }
        _ => None,
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
    // ----- the parallel executor (design/0017 §3) -----

    /// A safe point. Only a child ever has a cancel flag, so the parent
    /// walks straight through this.
    fn safe_point(&self) -> Eval<'a, ()> {
        match &self.cancel {
            Some(flag) if flag.load(std::sync::atomic::Ordering::Relaxed) => {
                Err(Control::Cancelled)
            }
            _ => Ok(()),
        }
    }

    /// A committed trap ends the program, so every child still outstanding
    /// can stop where it is — including one that would otherwise never
    /// finish. This is not a language-level cancel (`P7` is still unshipped);
    /// it is how the executor makes the program's end reachable.
    fn cancel_children(&self) {
        if let Some(pool) = &self.pool {
            pool.stop.store(true, std::sync::atomic::Ordering::Relaxed);
        }
    }

    /// Hand a child to the pool. The handle it returns names a position in
    /// the current region; nothing about the child's progress is readable
    /// through it, which is what keeps `spawn` order the only order.
    fn submit_child(
        &mut self,
        callee: Value<'a>,
        args: Vec<Value<'a>>,
        name: String,
        span: Span,
    ) -> Eval<'a, Value<'a>> {
        let pool = self.pool.as_ref().expect("checked by the caller");
        let (tx, rx) = std::sync::mpsc::channel::<ChildOutcome<'a>>();
        let table = self.table;
        let modules = self.modules;
        let home = self.current;
        let stop = Arc::clone(&pool.stop);
        let job: Job<'a> = Box::new(move || {
            let cancelled = stop.load(std::sync::atomic::Ordering::Relaxed);
            let outcome = if cancelled {
                // Already decided before this one even started.
                ChildOutcome {
                    stdout: Vec::new(),
                    result: ChildResult::Cancelled,
                }
            } else {
                let mut child = Interp {
                    table,
                    modules,
                    current: home,
                    stdout: Vec::new(),
                    // A child cannot spawn: its `uses` set is empty, and
                    // spawning is an effect (design/0015 §2).
                    pool: None,
                    cancel: Some(stop),
                    children: Vec::new(),
                    committed: 0,
                    regions: Vec::new(),
                };
                let result = match child.apply(callee, args, span) {
                    Ok(value) | Err(Control::Return(value)) => ChildResult::Value(value),
                    Err(Control::Trap { message, .. }) => ChildResult::Trap(message),
                    Err(Control::Cancelled) => ChildResult::Cancelled,
                    Err(Control::Break) | Err(Control::Continue) => ChildResult::Trap(
                        "`break` or `continue` escaped every loop — checker gap".to_string(),
                    ),
                };
                ChildOutcome {
                    stdout: child.stdout,
                    result,
                }
            };
            // The parent may already have dropped the region; nobody is
            // waiting, and that is fine.
            let _ = tx.send(outcome);
        });
        if pool.jobs.send(job).is_err() {
            // Every worker is gone. Nothing can run the child, so say so
            // rather than wait for an answer that will never come.
            return trap(
                span,
                format!("task `{name}` could not start — executor gap"),
            );
        }
        let index = self.children.len();
        self.children.push(Child {
            name,
            span,
            outcome: rx,
            value: None,
        });
        Ok(Value::Pending { index })
    }

    /// Resolve the outcomes of children `committed..=upto`, in spawn order.
    ///
    /// Spawn order is the sequential executor's order of fate: sequentially,
    /// child *i* ran to completion at its spawn statement, which is before
    /// child *i+1* existed. So a diverging child 1 hangs the program even
    /// though child 2 already trapped — exactly as it hangs sequentially —
    /// and a trapping child 2 after a fine child 1 reports child 2's trap.
    /// Committing in arrival order instead is what would make the two
    /// executors disagree (design/0017 §3).
    fn commit_through(&mut self, upto: usize) -> Eval<'a, ()> {
        while self.committed <= upto {
            let index = self.committed;
            let received = self.children[index].outcome.recv();
            self.committed += 1;
            let (name, span) = (self.children[index].name.clone(), self.children[index].span);
            let outcome = match received {
                Ok(outcome) => outcome,
                // The worker vanished without answering. Never expected; a
                // precise trap beats a silent hang.
                Err(_) => {
                    self.cancel_children();
                    return trap(span, format!("task `{name}` never reported — executor gap"));
                }
            };
            // A child performs no effects, so this is empty; appending it in
            // commit order keeps it deterministic if that ever stops holding.
            self.stdout.extend_from_slice(&outcome.stdout);
            match outcome.result {
                ChildResult::Value(value) => self.children[index].value = Some(value),
                ChildResult::Trap(message) => {
                    // This trap ends the program: reclaim the siblings, and
                    // let nothing commit after it — a later child's trap must
                    // not overtake the one whose fate was sealed first.
                    self.cancel_children();
                    return Err(Control::Trap {
                        message: format!("task `{name}` trapped: {message}"),
                        span,
                    });
                }
                // Only reachable once a trap has committed, and that trap
                // stops every later commit — including this one.
                ChildResult::Cancelled => {
                    return trap(span, format!("task `{name}` was cancelled — executor gap"));
                }
            }
        }
        Ok(())
    }

    /// Has a committed child trap already decided the program?
    fn fate_sealed(&self) -> bool {
        self.pool
            .as_ref()
            .is_some_and(|pool| pool.stop.load(std::sync::atomic::Ordering::Relaxed))
    }

    /// Everything a region still owes, in spawn order — the scope's closing
    /// brace joins what nothing awaited (design/0017 §3). Once a child trap
    /// has committed the program is over and nothing is left to decide:
    /// draining then could only replace the first trap with a later one.
    fn drain_region(&mut self, start: usize) -> Eval<'a, ()> {
        if self.fate_sealed() || self.children.len() <= start {
            return Ok(());
        }
        self.commit_through(self.children.len() - 1)
    }

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

    // ----- blocks and statements -----

    fn block(&mut self, block: &'a ast::Block, env: &mut Env<'a>) -> Eval<'a, Value<'a>> {
        env.scopes.push(Vec::new());
        let result = self.block_inner(block, env);
        env.scopes.pop();
        result
    }

    fn block_inner(&mut self, block: &'a ast::Block, env: &mut Env<'a>) -> Eval<'a, Value<'a>> {
        for stmt in &block.stmts {
            self.stmt(stmt, env)?;
        }
        match &block.tail {
            Some(tail) => self.eval(tail, env),
            None => Ok(Value::Unit),
        }
    }

    fn stmt(&mut self, stmt: &'a ast::Stmt, env: &mut Env<'a>) -> Eval<'a, ()> {
        // Safe point (design/0017 §3): statement boundaries, loop iterations
        // and calls are where a cancelled child notices and unwinds. Every
        // way a Xenith program can diverge — `while`, recursion — passes
        // through one of the three, so a diverging sibling is reclaimable.
        self.safe_point()?;
        match &stmt.kind {
            ast::StmtKind::Let { pattern, init, .. } => {
                let value = self.eval(init, env)?;
                self.bind_pattern(pattern, value, env, stmt.span)?;
                Ok(())
            }
            ast::StmtKind::Expr(expr) => {
                self.eval(expr, env)?;
                Ok(())
            }
            ast::StmtKind::Return(value) => {
                let value = match value {
                    Some(value) => self.eval(value, env)?,
                    None => Value::Unit,
                };
                Err(Control::Return(value))
            }
            ast::StmtKind::Break => Err(Control::Break),
            ast::StmtKind::Continue => Err(Control::Continue),
            ast::StmtKind::While { cond, body } => {
                loop {
                    self.safe_point()?;
                    match self.eval(cond, env)? {
                        Value::Bool(true) => {}
                        Value::Bool(false) => break,
                        _ => return trap(cond.span, "`while` needs a Bool"),
                    }
                    match self.block(body, env) {
                        Ok(_) => {}
                        Err(Control::Break) => break,
                        Err(Control::Continue) => continue,
                        Err(other) => return Err(other),
                    }
                }
                Ok(())
            }
            ast::StmtKind::For { iter, .. } => {
                // Iteration syntax is deferred to a later RFC (design/0007
                // §2); until it lands, iteration is `while` + `len` + `get`.
                trap(
                    iter.span,
                    "`for` cannot run yet: iterate with `while` + `len` + `get`",
                )
            }
            ast::StmtKind::Error => Ok(()),
        }
    }

    // ----- expressions -----

    fn eval(&mut self, expr: &'a ast::Expr, env: &mut Env<'a>) -> Eval<'a, Value<'a>> {
        match &expr.kind {
            ast::ExprKind::Int(text) => {
                let cleaned: String = text.chars().filter(|c| *c != '_').collect();
                match cleaned.parse::<i64>() {
                    Ok(value) => Ok(Value::Int(value)),
                    Err(_) => trap(expr.span, "integer literal does not fit in 64 bits"),
                }
            }
            ast::ExprKind::Float(text) => {
                let cleaned: String = text.chars().filter(|c| *c != '_').collect();
                match cleaned.parse::<f64>() {
                    Ok(value) => Ok(Value::Float(value)),
                    Err(_) => trap(expr.span, "float literal does not parse"),
                }
            }
            ast::ExprKind::Str(raw) => Ok(Value::str(unescape(raw, expr.span)?)),
            ast::ExprKind::Char(raw) => {
                let text = unescape(raw, expr.span)?;
                let mut chars = text.chars();
                match (chars.next(), chars.next()) {
                    (Some(c), None) => Ok(Value::Char(c)),
                    _ => trap(expr.span, "character literal must hold one character"),
                }
            }
            ast::ExprKind::Bool(value) => Ok(Value::Bool(*value)),
            ast::ExprKind::Unit => Ok(Value::Unit),

            ast::ExprKind::Hole { name } => {
                let shown = name.as_deref().unwrap_or("");
                trap(
                    expr.span,
                    format!(
                        "reached hole ??{shown} — ask `xenith goals` what belongs here, then fill it"
                    ),
                )
            }

            ast::ExprKind::Path(path) => self.path_value(path, expr.span, env),

            ast::ExprKind::Unary { op, operand } => {
                let value = self.eval(operand, env)?;
                match (op, value) {
                    (ast::UnaryOp::Neg, Value::Int(v)) => match v.checked_neg() {
                        Some(v) => Ok(Value::Int(v)),
                        None => trap(expr.span, "integer overflow negating i64::MIN"),
                    },
                    (ast::UnaryOp::Neg, Value::Float(v)) => Ok(Value::Float(-v)),
                    (ast::UnaryOp::Not, Value::Bool(v)) => Ok(Value::Bool(!v)),
                    _ => trap(expr.span, "operand type does not fit this operator"),
                }
            }

            ast::ExprKind::Binary { op, lhs, rhs } => self.binary(*op, lhs, rhs, expr.span, env),

            ast::ExprKind::Assign { target, op, value } => {
                let mut new_value = self.eval(value, env)?;
                if let Some(op) = op {
                    let current = self.read_place(target, env)?;
                    new_value = arith(*op, &current, &new_value, expr.span)?;
                }
                self.write_place(target, new_value, env)?;
                Ok(Value::Unit)
            }

            ast::ExprKind::Call { callee, args } => self.call(callee, args, expr.span, env),

            ast::ExprKind::MethodCall {
                receiver,
                method,
                args,
            } => self.method_call(receiver, method, args, expr.span, env),

            ast::ExprKind::Field { receiver, name } => {
                // `Enum.Variant` before value field access, mirroring the
                // checker's resolution order.
                if let ast::ExprKind::Path(path) = &receiver.kind {
                    if let [single] = path.segments.as_slice() {
                        if env.get(&single.name).is_none() {
                            if let Some(def) = self.lookup_type(&single.name) {
                                return self.variant_ref(def, &name.name, expr.span);
                            }
                        }
                    }
                }
                // `game.player.Rank.Gold`, or a foreign function as a value.
                if let Some(mut segments) = expr_segments(receiver) {
                    if env.get(&segments[0]).is_none() {
                        segments.push(name.name.clone());
                        match self.runtime_ref(&segments) {
                            Some(RuntimeRef::Fn(home, bare)) => {
                                return self.fn_value(home, &bare, expr.span);
                            }
                            Some(RuntimeRef::Const(home, bare)) => {
                                return self.const_value(home, &bare, expr.span);
                            }
                            Some(RuntimeRef::Variant(def, variant)) => {
                                return self.variant_ref(def, &variant, expr.span);
                            }
                            None => {}
                        }
                    }
                }
                let value = self.eval(receiver, env)?;
                self.field_of(value, &name.name, expr.span)
            }

            ast::ExprKind::Await(inner) => match self.eval(inner, env)? {
                // The sequential executor's handle: the body already ran.
                Value::Task(value) => Ok(owned(value)),
                // The parallel executor's handle: commit every earlier
                // child of this region first, then take this one's value.
                Value::Pending { index } => {
                    self.commit_through(index)?;
                    match self.children[index].value.take() {
                        Some(value) => Ok(value),
                        // XN6006 refuses a second await; reaching this is a
                        // checker gap, reported as one.
                        None => trap(expr.span, "this task was already awaited — checker gap"),
                    }
                }
                _ => trap(expr.span, "`.await` needs a Task"),
            },

            // The task region (design/0015 §1). Under the parallel executor
            // it is also the join point: the closing brace resolves, in
            // spawn order, every child nothing awaited (design/0017 §3).
            ast::ExprKind::Scope(block) => {
                if self.pool.is_none() {
                    return self.block(block, env);
                }
                let start = self.children.len();
                self.regions.push(start);
                let body = self.block(block, env);
                let drained = self.drain_region(start);
                self.regions.pop();
                // Retire this region's children: a handle cannot outlive its
                // scope, so the list stays bounded even when the scope sits
                // inside a loop.
                self.children.truncate(start);
                self.committed = self.committed.min(start);
                // A child's fate was sealed at its spawn statement, which is
                // before anything the parent did afterwards — so a child
                // trap outranks whatever the parent was carrying out of the
                // block. That is what the sequential executor reports, and
                // reporting anything else here would be the difference.
                drained?;
                body
            }

            // `spawn f(args)`: evaluate the arguments here, in normal order,
            // exactly once (design/0015 §1). Then hand the child to the pool
            // — or, with no pool, run it to completion on the spot, which is
            // the sequential executor. A trap inside the child surfaces at
            // the spawn site either way, carrying the child's name.
            ast::ExprKind::Spawn { path, args } => {
                let segments: Vec<String> = path.segments.iter().map(|s| s.name.clone()).collect();
                let shown = segments.join(".");
                let callee = if let [single] = segments.as_slice() {
                    self.fn_value(self.current, single, path.span)?
                } else {
                    match self.runtime_ref(&segments) {
                        Some(RuntimeRef::Fn(home, bare)) => {
                            self.fn_value(home, &bare, path.span)?
                        }
                        _ => return trap(path.span, format!("no function `{shown}` at runtime")),
                    }
                };
                let mut evaluated = Vec::with_capacity(args.len());
                for arg in args {
                    evaluated.push(self.eval(&arg.value, env)?);
                }
                if self.pool.is_some() && !self.regions.is_empty() {
                    return self.submit_child(callee, evaluated, shown, expr.span);
                }
                match self.apply(callee, evaluated, expr.span) {
                    Ok(value) => Ok(Value::Task(Arc::new(value))),
                    Err(Control::Trap { message, .. }) => Err(Control::Trap {
                        message: format!("task `{shown}` trapped: {message}"),
                        span: expr.span,
                    }),
                    Err(other) => Err(other),
                }
            }

            ast::ExprKind::Try(inner) => {
                let value = self.eval(inner, env)?;
                match value {
                    Value::Enum {
                        def,
                        variant,
                        payload,
                    } if def == self.table.result => {
                        if variant == ok_index() {
                            Ok(owned(payload).remove(0))
                        } else {
                            // Propagate the whole Err to the caller.
                            Err(Control::Return(Value::Enum {
                                def,
                                variant,
                                payload,
                            }))
                        }
                    }
                    Value::Enum {
                        def,
                        variant,
                        payload,
                    } if def == self.table.option => {
                        if variant == some_index() {
                            Ok(owned(payload).remove(0))
                        } else {
                            Err(Control::Return(Value::enumeration(
                                def,
                                none_index(),
                                Vec::new(),
                            )))
                        }
                    }
                    _ => trap(expr.span, "`?` needs a Result or Option"),
                }
            }

            ast::ExprKind::If {
                cond,
                then_block,
                else_branch,
            } => match self.eval(cond, env)? {
                Value::Bool(true) => self.block(then_block, env),
                Value::Bool(false) => match else_branch {
                    Some(branch) => self.eval(branch, env),
                    None => Ok(Value::Unit),
                },
                _ => trap(cond.span, "`if` needs a Bool"),
            },

            ast::ExprKind::Match { scrutinee, arms } => {
                let value = self.eval(scrutinee, env)?;
                for arm in arms {
                    env.scopes.push(Vec::new());
                    let matched = self.try_pattern(&arm.pattern, &value, env, arm.span)?;
                    if matched {
                        if let Some(guard) = &arm.guard {
                            match self.eval(guard, env)? {
                                Value::Bool(true) => {}
                                Value::Bool(false) => {
                                    env.scopes.pop();
                                    continue;
                                }
                                _ => {
                                    env.scopes.pop();
                                    return trap(guard.span, "a guard needs a Bool");
                                }
                            }
                        }
                        let result = self.eval(&arm.body, env);
                        env.scopes.pop();
                        return result;
                    }
                    env.scopes.pop();
                }
                // XN5001 refuses non-exhaustive matches, and `run` refuses
                // files with diagnostics — so this is a checker gap, reported
                // as one rather than as undefined behaviour.
                trap(
                    expr.span,
                    "no `match` arm matched — XN5001 should have refused this program; checker gap",
                )
            }

            ast::ExprKind::Block(block) => self.block(block, env),

            ast::ExprKind::ListLit(elements) => {
                let mut items = Vec::with_capacity(elements.len());
                for element in elements {
                    items.push(self.eval(element, env)?);
                }
                Ok(Value::list(items))
            }

            ast::ExprKind::StructLit { path, fields } => {
                let shown = path
                    .segments
                    .iter()
                    .map(|s| s.name.as_str())
                    .collect::<Vec<_>>()
                    .join(".");
                // Items are single segments, so a dotted spelling is exactly
                // the table key; a bare one resolves like every bare name.
                let def = if path.segments.len() == 1 {
                    self.lookup_type(&shown)
                } else {
                    self.table.lookup(&shown)
                };
                let Some(def) = def else {
                    return trap(expr.span, format!("`{shown}` is not a struct"));
                };
                let DefKind::Struct {
                    fields: declared, ..
                } = &self.table.def(def).kind
                else {
                    return trap(expr.span, format!("`{shown}` is not a struct"));
                };
                // Evaluate in source order (kernel: strict left-to-right),
                // store in declaration order.
                let mut evaluated: Vec<(String, Value)> = Vec::new();
                for init in fields {
                    let value = self.eval(&init.value, env)?;
                    evaluated.push((init.name.name.clone(), value));
                }
                let mut ordered = Vec::with_capacity(declared.len());
                for field in declared {
                    match evaluated.iter().position(|(n, _)| *n == field.name) {
                        Some(index) => ordered.push(evaluated.remove(index).1),
                        None => {
                            return trap(
                                expr.span,
                                format!("field `{}` was never set", field.name),
                            );
                        }
                    }
                }
                Ok(Value::structure(def, ordered))
            }

            // Creation-time snapshot (design/0014 §2): the closure copies
            // everything visible, once, here. The checker already guaranteed
            // that what the body actually touches is CaptureSafe and not a
            // `var`, so copying the superset is observationally identical.
            ast::ExprKind::Lambda { params, body } => Ok(Value::Fn {
                params: Arc::new(params.iter().map(|p| p.name.name.clone()).collect()),
                body: Body::Expr(body),
                captured: Arc::new(env.snapshot()),
                is_async: false,
                home: self.current,
            }),

            ast::ExprKind::Error => trap(expr.span, "cannot run code the parser could not read"),
        }
    }

    // ----- names -----

    fn path_value(
        &mut self,
        path: &'a ast::Path,
        span: Span,
        env: &mut Env<'a>,
    ) -> Eval<'a, Value<'a>> {
        let name = &path.segments[0].name;
        if let Some(value) = env.get(name) {
            return Ok(value.clone());
        }
        if find_const(self.current_module(), name).is_some() {
            return self.const_value(self.current, name, span);
        }
        if let Some(f) = find_fn(self.current_module(), name) {
            return Ok(Value::Fn {
                params: Arc::new(f.params.iter().map(|p| p.name.name.clone()).collect()),
                body: match &f.body {
                    Some(body) => Body::Block(body),
                    None => return trap(span, format!("`{name}` has no body")),
                },
                captured: Arc::new(Vec::new()),
                is_async: f.is_async,
                home: self.current,
            });
        }
        if let Some((def, variant)) = self.table.unqualified_variant(name) {
            let index = self.variant_index(def, &variant.name);
            if variant.payload.is_empty() {
                return Ok(Value::enumeration(def, index, Vec::new()));
            }
            return Ok(Value::VariantCtor {
                def,
                variant: index,
                arity: variant.payload.len(),
            });
        }
        trap(span, format!("nothing named `{name}` at runtime"))
    }

    fn variant_index(&self, def: DefId, variant_name: &str) -> usize {
        match &self.table.def(def).kind {
            DefKind::Enum { variants } => variants
                .iter()
                .position(|v| v.name == variant_name)
                .unwrap_or(0),
            _ => 0,
        }
    }

    fn variant_ref(&mut self, def: DefId, variant_name: &str, span: Span) -> Eval<'a, Value<'a>> {
        let Some(variant) = self.table.variant_named(def, variant_name) else {
            return trap(span, format!("no variant `{variant_name}`"));
        };
        let index = self.variant_index(def, variant_name);
        if variant.payload.is_empty() {
            Ok(Value::enumeration(def, index, Vec::new()))
        } else {
            Ok(Value::VariantCtor {
                def,
                variant: index,
                arity: variant.payload.len(),
            })
        }
    }

    fn field_of(&self, value: Value<'a>, field: &str, span: Span) -> Eval<'a, Value<'a>> {
        match value {
            Value::Struct { def, fields } => {
                let DefKind::Struct {
                    fields: declared, ..
                } = &self.table.def(def).kind
                else {
                    return trap(span, "not a struct");
                };
                match declared.iter().position(|f| f.name == field) {
                    // D1: reading a field yields an independent value.
                    Some(index) => Ok(owned(fields).remove(index)),
                    None => trap(span, format!("no field `{field}`")),
                }
            }
            _ => trap(span, format!("`{field}` is not a field of this value")),
        }
    }

    // ----- calls -----

    fn call(
        &mut self,
        callee: &'a ast::Expr,
        args: &'a [ast::Arg],
        span: Span,
        env: &mut Env<'a>,
    ) -> Eval<'a, Value<'a>> {
        // Named functions and variant constructors resolve before locals do
        // not shadow them — same order as the checker.
        let callee_value = match &callee.kind {
            ast::ExprKind::Path(path) if path.segments.len() == 1 => {
                // The one prelude free function (design/0007 D4). A user
                // declaration of the same name is a duplicate-definition
                // error, so nothing real is shadowed here.
                let name = &path.segments[0].name;
                if name == "empty_map"
                    && env.get(name).is_none()
                    && find_fn(self.current_module(), name).is_none()
                {
                    for arg in args {
                        self.eval(&arg.value, env)?;
                    }
                    return Ok(Value::map(Vec::new()));
                }
                self.path_value(path, callee.span, env)?
            }
            ast::ExprKind::Field { receiver, name } => {
                if let ast::ExprKind::Path(path) = &receiver.kind {
                    if let [single] = path.segments.as_slice() {
                        if env.get(&single.name).is_none() {
                            if let Some(def) = self.lookup_type(&single.name) {
                                self.variant_ref(def, &name.name, callee.span)?
                            } else {
                                self.eval(callee, env)?
                            }
                        } else {
                            self.eval(callee, env)?
                        }
                    } else {
                        self.eval(callee, env)?
                    }
                } else {
                    self.eval(callee, env)?
                }
            }
            _ => self.eval(callee, env)?,
        };

        let mut evaluated = Vec::with_capacity(args.len());
        for arg in args {
            evaluated.push(self.eval(&arg.value, env)?);
        }

        self.apply(callee_value, evaluated, span)
    }

    fn apply(
        &mut self,
        callee: Value<'a>,
        args: Vec<Value<'a>>,
        span: Span,
    ) -> Eval<'a, Value<'a>> {
        self.safe_point()?;
        match callee {
            Value::Fn {
                params,
                body,
                captured,
                is_async,
                home,
            } => {
                if params.len() != args.len() {
                    return trap(span, "wrong number of arguments");
                }
                let mut env = Env::new();
                for (name, value) in captured.iter() {
                    env.bind(name, value.clone());
                }
                env.scopes.push(Vec::new());
                for (param, value) in params.iter().zip(args) {
                    env.bind(param, value);
                }
                // The callee's bare names live in its own module.
                let caller = self.current;
                self.current = home;
                let result = match body {
                    Body::Block(block) => self.block_inner(block, &mut env),
                    Body::Expr(expr) => self.eval(expr, &mut env),
                };
                self.current = caller;
                let value = match result {
                    Ok(value) | Err(Control::Return(value)) => value,
                    Err(other) => return Err(other),
                };
                if is_async {
                    Ok(Value::Task(Arc::new(value)))
                } else {
                    Ok(value)
                }
            }
            Value::VariantCtor {
                def,
                variant,
                arity,
            } => {
                if args.len() != arity {
                    return trap(span, "wrong number of constructor arguments");
                }
                Ok(Value::enumeration(def, variant, args))
            }
            _ => trap(span, "this value is not callable"),
        }
    }

    /// Built-in methods — the runtime half of the provisional prelude in
    /// `def.rs`. The two tables must agree; the examples exercise both.
    fn method_call(
        &mut self,
        receiver: &'a ast::Expr,
        method: &'a ast::Ident,
        args: &'a [ast::Arg],
        span: Span,
        env: &mut Env<'a>,
    ) -> Eval<'a, Value<'a>> {
        // `Grade.Pass(95)` parses as a method call; construct the variant.
        // Mirrors the checker's resolution order exactly.
        if let ast::ExprKind::Path(path) = &receiver.kind {
            if let [single] = path.segments.as_slice() {
                if env.get(&single.name).is_none() {
                    if let Some(def) = self.lookup_type(&single.name) {
                        if self.table.variant_named(def, &method.name).is_some() {
                            let ctor = self.variant_ref(def, &method.name, span)?;
                            let mut evaluated = Vec::with_capacity(args.len());
                            for arg in args {
                                evaluated.push(self.eval(&arg.value, env)?);
                            }
                            return self.apply(ctor, evaluated, span);
                        }
                    }
                }
            }
        }

        // `game.scores.best(..)` / `game.player.Rank.Gold(..)` — resolved
        // against the module set before anything else is evaluated.
        if let Some(receiver_segments) = expr_segments(receiver) {
            if env.get(&receiver_segments[0]).is_none() {
                let mut segments = receiver_segments;
                segments.push(method.name.clone());
                if let Some(reference) = self.runtime_ref(&segments) {
                    let mut evaluated = Vec::with_capacity(args.len());
                    for arg in args {
                        evaluated.push(self.eval(&arg.value, env)?);
                    }
                    return match reference {
                        RuntimeRef::Fn(home, bare) => {
                            let callee = self.fn_value(home, &bare, span)?;
                            self.apply(callee, evaluated, span)
                        }
                        // A const is not callable; the checker refused it.
                        RuntimeRef::Const(_, bare) => {
                            trap(span, format!("`{bare}` is a const, not a fn"))
                        }
                        RuntimeRef::Variant(def, variant) => {
                            let ctor = self.variant_ref(def, &variant, span)?;
                            self.apply(ctor, evaluated, span)
                        }
                    };
                }
            }
        }

        // The container mutators write through the receiver in place, so it
        // is resolved as a place — the same resolution `=` uses — rather than
        // evaluated to a copy. Arguments go first, as assignment evaluates
        // its right-hand side first, so the place borrow overlaps nothing.
        if matches!(
            method.name.as_str(),
            "push" | "pop" | "replace" | "insert" | "remove"
        ) {
            let mut evaluated = Vec::with_capacity(args.len());
            for arg in args {
                evaluated.push(self.eval(&arg.value, env)?);
            }
            let slot = self.resolve_place(receiver, env)?;
            // `resolve_place` already uniquified every node on the way here
            // (design/0017 §4); `make_mut` finishes the job at the leaf. A
            // shared node is copied before it is written, so a value read out
            // of this container earlier stays exactly as it was (D1).
            return match (&mut *slot, method.name.as_str()) {
                (Value::List(items), "push") => {
                    let Some(item) = evaluated.into_iter().next() else {
                        return trap(span, "push takes a value");
                    };
                    Arc::make_mut(items).push(item);
                    Ok(Value::Unit)
                }
                (Value::List(items), "pop") => {
                    let popped = Arc::make_mut(items).pop();
                    Ok(self.option_of(popped))
                }
                (Value::List(items), "replace") => {
                    let mut taken = evaluated.into_iter();
                    let (Some(Value::Int(index)), Some(value)) = (taken.next(), taken.next())
                    else {
                        return trap(span, "replace takes an index and a value");
                    };
                    // Out of range leaves the list untouched (0007 §3) — and
                    // must not copy it either, so the bounds test comes first.
                    let target = usize::try_from(index).ok().filter(|i| *i < items.len());
                    let old =
                        target.map(|i| std::mem::replace(&mut Arc::make_mut(items)[i], value));
                    Ok(self.option_of(old))
                }
                (Value::Map(entries), "insert") => {
                    let mut taken = evaluated.into_iter();
                    let (Some(key), Some(value)) = (taken.next(), taken.next()) else {
                        return trap(span, "insert takes a key and a value");
                    };
                    // An existing key keeps its position and its stored key;
                    // only the value moves (0007 §3 normative order).
                    let mut existing = None;
                    for (index, (stored, _)) in entries.iter().enumerate() {
                        if values_equal(stored, &key, span)? {
                            existing = Some(index);
                            break;
                        }
                    }
                    match existing {
                        Some(index) => {
                            let old =
                                std::mem::replace(&mut Arc::make_mut(entries)[index].1, value);
                            Ok(self.option_of(Some(old)))
                        }
                        None => {
                            Arc::make_mut(entries).push((key, value));
                            Ok(self.option_of(None))
                        }
                    }
                }
                (Value::Map(entries), "remove") => {
                    let Some(key) = evaluated.into_iter().next() else {
                        return trap(span, "remove takes a key");
                    };
                    let mut found = None;
                    for (index, (stored, _)) in entries.iter().enumerate() {
                        if values_equal(stored, &key, span)? {
                            found = Some(index);
                            break;
                        }
                    }
                    // Vec::remove shifts, so the survivors keep their order;
                    // a later re-insert of the key lands at the end. A miss
                    // writes nothing, so it does not copy either.
                    let removed = found.map(|index| Arc::make_mut(entries).remove(index).1);
                    Ok(self.option_of(removed))
                }
                _ => trap(
                    span,
                    format!("no runtime method `{}` for this value", method.name),
                ),
            };
        }

        let receiver_value = self.eval(receiver, env)?;
        let mut evaluated = Vec::with_capacity(args.len());
        for arg in args {
            evaluated.push(self.eval(&arg.value, env)?);
        }

        match (&receiver_value, method.name.as_str()) {
            (Value::Int(a), "checked_add") => {
                let Some(Value::Int(b)) = evaluated.first() else {
                    return trap(span, "checked_add takes an Int");
                };
                Ok(match a.checked_add(*b) {
                    Some(sum) => {
                        Value::enumeration(self.table.option, some_index(), vec![Value::Int(sum)])
                    }
                    None => Value::enumeration(self.table.option, none_index(), Vec::new()),
                })
            }
            (Value::Int(a), "to_text") => Ok(Value::str(a.to_string())),
            (Value::Str(a), "concat") => {
                let Some(Value::Str(b)) = evaluated.first() else {
                    return trap(span, "concat takes a String");
                };
                Ok(Value::str(format!("{a}{b}")))
            }
            // `len` counts Unicode scalar values, never bytes (D2).
            (Value::Str(a), "len") => Ok(Value::Int(a.chars().count() as i64)),
            (Value::Str(a), "split") => {
                let Some(Value::Str(sep)) = evaluated.first() else {
                    return trap(span, "split takes a String");
                };
                // Lossless by construction: `pieces.join(sep)` rebuilds the
                // input exactly, empty pieces included. The empty separator
                // is the `chars` replacement — one piece per scalar.
                let pieces: Vec<Value> = if sep.is_empty() {
                    a.chars().map(|c| Value::str(c.to_string())).collect()
                } else {
                    a.split(sep.as_str())
                        .map(|piece| Value::str(piece.to_string()))
                        .collect()
                };
                Ok(Value::list(pieces))
            }
            (Value::Str(a), "trim") => Ok(Value::str(
                a.trim_matches(|c: char| matches!(c, ' ' | '\t' | '\r' | '\n'))
                    .to_string(),
            )),
            (Value::Str(a), "try_to_int") => {
                // Accepted shape: ASCII whitespace, then [+-]?[0-9]+ (0007
                // §3). Everything else — separators, decimals, overflow — is
                // an Err value, never a trap.
                let trimmed = a.trim_matches(|c: char| matches!(c, ' ' | '\t' | '\r' | '\n'));
                Ok(match trimmed.parse::<i64>() {
                    Ok(value) => {
                        Value::enumeration(self.table.result, ok_index(), vec![Value::Int(value)])
                    }
                    Err(error) => {
                        let message = match error.kind() {
                            std::num::IntErrorKind::PosOverflow
                            | std::num::IntErrorKind::NegOverflow => "out of Int range",
                            _ => "not an integer",
                        };
                        Value::enumeration(
                            self.table.result,
                            err_index(),
                            vec![Value::error_value(message)],
                        )
                    }
                })
            }
            (Value::Str(a), "starts_with") => {
                let Some(Value::Str(prefix)) = evaluated.first() else {
                    return trap(span, "starts_with takes a String");
                };
                Ok(Value::Bool(a.starts_with(prefix.as_str())))
            }
            (Value::Str(a), "contains") => {
                let Some(Value::Str(sub)) = evaluated.first() else {
                    return trap(span, "contains takes a String");
                };
                Ok(Value::Bool(a.contains(sub.as_str())))
            }
            (Value::List(items), "len") => Ok(Value::Int(items.len() as i64)),
            (Value::List(items), "is_empty") => Ok(Value::Bool(items.is_empty())),
            // ----- the design/0014 §4 combinators -----
            //
            // The only way a closure is ever invoked: by std, left to right,
            // returning new values. The receiver is a copy and stays whole.
            (Value::List(items), "map") => {
                let Some(f) = evaluated.into_iter().next() else {
                    return trap(span, "map takes a closure");
                };
                let mut out = Vec::with_capacity(items.len());
                for item in items.iter() {
                    out.push(self.apply(f.clone(), vec![item.clone()], span)?);
                }
                Ok(Value::list(out))
            }
            (Value::List(items), "filter") => {
                let Some(f) = evaluated.into_iter().next() else {
                    return trap(span, "filter takes a closure");
                };
                let mut out = Vec::new();
                for item in items.iter() {
                    match self.apply(f.clone(), vec![item.clone()], span)? {
                        Value::Bool(true) => out.push(item.clone()),
                        Value::Bool(false) => {}
                        _ => return trap(span, "`filter` needs its closure to return a Bool"),
                    }
                }
                Ok(Value::list(out))
            }
            (Value::List(items), "fold") => {
                // Left fold: `fold(init: 0, f: |acc, x| ..)` — the named
                // argument rule fixed the order at the call site.
                let mut taken = evaluated.into_iter();
                let (Some(init), Some(f)) = (taken.next(), taken.next()) else {
                    return trap(span, "fold takes an initial value and a closure");
                };
                let mut acc = init;
                for item in items.iter() {
                    acc = self.apply(f.clone(), vec![acc, item.clone()], span)?;
                }
                Ok(acc)
            }
            (Value::List(items), "find") => {
                // Short-circuits: elements after the first hit are never
                // touched (design/0014 §4 — the contract, not an optimisation).
                let Some(f) = evaluated.into_iter().next() else {
                    return trap(span, "find takes a closure");
                };
                for item in items.iter() {
                    match self.apply(f.clone(), vec![item.clone()], span)? {
                        Value::Bool(true) => return Ok(self.option_of(Some(item.clone()))),
                        Value::Bool(false) => {}
                        _ => return trap(span, "`find` needs its closure to return a Bool"),
                    }
                }
                Ok(self.option_of(None))
            }
            (Value::List(items), "get") => {
                let Some(Value::Int(index)) = evaluated.first() else {
                    return trap(span, "get takes an Int");
                };
                // Negative and out-of-range are both None; the hit is a copy
                // of the element (D1).
                let item = usize::try_from(*index)
                    .ok()
                    .and_then(|i| items.get(i))
                    .cloned();
                Ok(self.option_of(item))
            }
            (Value::List(items), "contains") => {
                let Some(needle) = evaluated.first() else {
                    return trap(span, "contains takes a value");
                };
                let mut found = false;
                for item in items.iter() {
                    if values_equal(item, needle, span)? {
                        found = true;
                        break;
                    }
                }
                Ok(Value::Bool(found))
            }
            (Value::List(items), "sorted") => {
                // Insertion keeps the sort stable and lets a comparison trap
                // propagate, which `sort_by` cannot.
                let mut sorted = items.as_ref().clone();
                let mut i = 1;
                while i < sorted.len() {
                    let mut j = i;
                    while j > 0 {
                        let ordering = compare(&sorted[j - 1], &sorted[j], span)?;
                        if ordering != Some(std::cmp::Ordering::Greater) {
                            break;
                        }
                        sorted.swap(j - 1, j);
                        j -= 1;
                    }
                    i += 1;
                }
                Ok(Value::list(sorted))
            }
            (Value::List(items), "concat") => {
                let Some(Value::List(other)) = evaluated.first() else {
                    return trap(span, "concat takes a List");
                };
                let mut joined = items.as_ref().clone();
                joined.extend(other.iter().cloned());
                Ok(Value::list(joined))
            }
            (Value::List(items), "join") => {
                let Some(Value::Str(sep)) = evaluated.first() else {
                    return trap(span, "join takes a String");
                };
                let rendered: Vec<String> =
                    items.iter().map(|item| self.value_text(item)).collect();
                Ok(Value::str(rendered.join(sep.as_str())))
            }
            (Value::Map(entries), "len") => Ok(Value::Int(entries.len() as i64)),
            (Value::Map(entries), "is_empty") => Ok(Value::Bool(entries.is_empty())),
            (Value::Map(entries), "get") => {
                let Some(key) = evaluated.first() else {
                    return trap(span, "get takes a key");
                };
                let mut hit = None;
                for (stored, value) in entries.iter() {
                    if values_equal(stored, key, span)? {
                        // D1: the read is a copy of the value.
                        hit = Some(value.clone());
                        break;
                    }
                }
                Ok(self.option_of(hit))
            }
            (Value::Map(entries), "has_key") => {
                let Some(key) = evaluated.first() else {
                    return trap(span, "has_key takes a key");
                };
                let mut found = false;
                for (stored, _) in entries.iter() {
                    if values_equal(stored, key, span)? {
                        found = true;
                        break;
                    }
                }
                Ok(Value::Bool(found))
            }
            // Insertion-order snapshot: later mutation of the map must not
            // reach into a list already handed out (0007 §3).
            (Value::Map(entries), "keys") => Ok(Value::list(
                entries.iter().map(|(key, _)| key.clone()).collect(),
            )),
            (Value::Enum { def, variant, .. }, "to_result") if *def == self.table.option => {
                let error = evaluated.into_iter().next().unwrap_or(Value::Unit);
                let Value::Enum {
                    variant, payload, ..
                } = receiver_value
                else {
                    unreachable!("matched above");
                };
                Ok(if variant == some_index() {
                    Value::Enum {
                        def: self.table.result,
                        variant: ok_index(),
                        payload,
                    }
                } else {
                    Value::enumeration(self.table.result, err_index(), vec![error])
                })
            }
            (Value::Capability("Io"), "write") => {
                let Some(Value::Str(text)) = evaluated.first() else {
                    return trap(span, "write takes a String");
                };
                self.stdout.extend_from_slice(text.as_bytes());
                Ok(Value::enumeration(
                    self.table.result,
                    ok_index(),
                    vec![Value::Unit],
                ))
            }
            _ => trap(
                span,
                format!("no runtime method `{}` for this value", method.name),
            ),
        }
    }

    /// `Some(value)` / `None` from a Rust `Option`.
    fn option_of(&self, value: Option<Value<'a>>) -> Value<'a> {
        match value {
            Some(value) => Value::enumeration(self.table.option, some_index(), vec![value]),
            None => Value::enumeration(self.table.option, none_index(), Vec::new()),
        }
    }

    /// Total, deterministic rendering — the runtime face of the sealed `Text`
    /// property, which is total today (design/0006 §3-5). `String` renders
    /// verbatim; everything else the way a literal would be written.
    fn value_text(&self, value: &Value<'a>) -> String {
        match value {
            Value::Int(v) => v.to_string(),
            Value::Float(v) => v.to_string(),
            Value::Bool(v) => v.to_string(),
            Value::Str(v) => v.as_ref().clone(),
            Value::Char(v) => v.to_string(),
            Value::Unit => "unit".to_string(),
            Value::List(items) => {
                let parts: Vec<String> = items.iter().map(|item| self.value_text(item)).collect();
                format!("[{}]", parts.join(", "))
            }
            // Rendered in insertion order — deterministic by the normative
            // order rules, even though `==` ignores it.
            Value::Map(entries) => {
                let parts: Vec<String> = entries
                    .iter()
                    .map(|(key, value)| {
                        format!("{}: {}", self.value_text(key), self.value_text(value))
                    })
                    .collect();
                format!("{{{}}}", parts.join(", "))
            }
            Value::ErrorValue(message) => format!("Error({message})"),
            Value::Struct { def, fields } => {
                let name = self.table.name_of(*def);
                let DefKind::Struct { fields: declared } = &self.table.def(*def).kind else {
                    return name;
                };
                let parts: Vec<String> = declared
                    .iter()
                    .zip(fields.iter())
                    .map(|(field, value)| format!("{}: {}", field.name, self.value_text(value)))
                    .collect();
                format!("{name} {{ {} }}", parts.join(", "))
            }
            Value::Enum {
                def,
                variant,
                payload,
            } => {
                let name = match &self.table.def(*def).kind {
                    DefKind::Enum { variants } => variants
                        .get(*variant)
                        .map(|v| v.name.clone())
                        .unwrap_or_default(),
                    _ => String::new(),
                };
                if payload.is_empty() {
                    name
                } else {
                    let parts: Vec<String> =
                        payload.iter().map(|part| self.value_text(part)).collect();
                    format!("{name}({})", parts.join(", "))
                }
            }
            Value::Fn { .. } | Value::VariantCtor { .. } => "<fn>".to_string(),
            Value::Capability(name) => format!("<{name}>"),
            Value::Task(_) | Value::Pending { .. } => "<task>".to_string(),
        }
    }

    // ----- patterns -----

    fn bind_pattern(
        &mut self,
        pattern: &'a ast::Pattern,
        value: Value<'a>,
        env: &mut Env<'a>,
        span: Span,
    ) -> Eval<'a, ()> {
        if self.try_pattern(pattern, &value, env, span)? {
            Ok(())
        } else {
            trap(span, "`let` pattern did not match the value")
        }
    }

    /// Attempt a match, binding as it goes. Bindings from a failed attempt are
    /// discarded by the caller popping the scope.
    fn try_pattern(
        &mut self,
        pattern: &'a ast::Pattern,
        value: &Value<'a>,
        env: &mut Env<'a>,
        span: Span,
    ) -> Eval<'a, bool> {
        match &pattern.kind {
            ast::PatternKind::Wildcard | ast::PatternKind::Error => Ok(true),

            ast::PatternKind::Binding(ident) => {
                // Variant-of-the-scrutinee names match the variant, mirroring
                // the checker (a misspelt `None` must not become a catch-all).
                if let Value::Enum { def, variant, .. } = value {
                    if let Some(found) = self.table.variant_named(*def, &ident.name) {
                        let index = self.variant_index(*def, &found.name);
                        return Ok(index == *variant);
                    }
                }
                env.bind(&ident.name, value.clone());
                Ok(true)
            }

            ast::PatternKind::Literal(expr) => {
                let literal = self.eval(expr, env)?;
                values_equal(&literal, value, span)
            }

            ast::PatternKind::Path(path) => {
                // Enum.Variant, possibly module-qualified.
                if path.segments.len() < 2 {
                    return Ok(false);
                }
                let variant_ident = path.segments.last().expect("two or more segments");
                let type_name = path.segments[..path.segments.len() - 1]
                    .iter()
                    .map(|s| s.name.as_str())
                    .collect::<Vec<_>>()
                    .join(".");
                let def = if path.segments.len() == 2 {
                    self.lookup_type(&type_name)
                } else {
                    self.table.lookup(&type_name)
                };
                let Some(def) = def else {
                    return Ok(false);
                };
                let index = self.variant_index(def, &variant_ident.name);
                Ok(matches!(
                    value,
                    Value::Enum { def: d, variant, .. } if *d == def && *variant == index
                ))
            }

            ast::PatternKind::Variant { path, elements } => {
                let Value::Enum {
                    def,
                    variant,
                    payload,
                } = value
                else {
                    return Ok(false);
                };
                let Some(last) = path.segments.last() else {
                    return Ok(false);
                };
                let variant_name = &last.name;
                let index = self.variant_index(*def, variant_name);
                if index != *variant || elements.len() != payload.len() {
                    return Ok(false);
                }
                for (element, part) in elements.iter().zip(payload.iter()) {
                    if !self.try_pattern(element, part, env, span)? {
                        return Ok(false);
                    }
                }
                Ok(true)
            }

            ast::PatternKind::Struct { fields, .. } => {
                let Value::Struct { def, fields: parts } = value else {
                    return Ok(false);
                };
                let DefKind::Struct {
                    fields: declared, ..
                } = &self.table.def(*def).kind
                else {
                    return Ok(false);
                };
                for field in fields {
                    let Some(index) = declared.iter().position(|f| f.name == field.name.name)
                    else {
                        return Ok(false);
                    };
                    let part = &parts[index];
                    match &field.pattern {
                        Some(sub) => {
                            if !self.try_pattern(sub, part, env, span)? {
                                return Ok(false);
                            }
                        }
                        None => env.bind(&field.name.name, part.clone()),
                    }
                }
                Ok(true)
            }

            ast::PatternKind::Or(alternatives) => {
                for alternative in alternatives {
                    if self.try_pattern(alternative, value, env, span)? {
                        return Ok(true);
                    }
                }
                Ok(false)
            }
        }
    }

    // ----- places (assignment targets) -----

    fn read_place(&mut self, target: &'a ast::Expr, env: &mut Env<'a>) -> Eval<'a, Value<'a>> {
        self.eval(target, env)
    }

    fn write_place(
        &mut self,
        target: &'a ast::Expr,
        value: Value<'a>,
        env: &mut Env<'a>,
    ) -> Eval<'a, ()> {
        let slot = self.resolve_place(target, env)?;
        *slot = value;
        Ok(())
    }

    /// Resolve an assignment target to the slot it names.
    ///
    /// The recursion is the copy-on-write contract (design/0017 §4): a
    /// binding in the environment is already unshared, and every aggregate
    /// node the path descends through is uniquified with `Arc::make_mut`
    /// *before* the descent continues. Uniquifying only the outer node and
    /// then writing through a shared inner one is precisely the bug the RFC
    /// names — the write would be visible through a value somebody else
    /// already read out (D1).
    fn resolve_place<'e>(
        &self,
        target: &'a ast::Expr,
        env: &'e mut Env<'a>,
    ) -> Eval<'a, &'e mut Value<'a>> {
        match &target.kind {
            ast::ExprKind::Path(path) => {
                let name = &path.segments[0].name;
                match env.get_mut(name) {
                    Some(slot) => Ok(slot),
                    None => trap(target.span, format!("no binding named `{name}`")),
                }
            }
            ast::ExprKind::Field { receiver, name } => {
                let table = self.table;
                let base = self.resolve_place(receiver, env)?;
                let Value::Struct { def, fields } = base else {
                    return trap(target.span, "not a struct");
                };
                let DefKind::Struct {
                    fields: declared, ..
                } = &table.def(*def).kind
                else {
                    return trap(target.span, "not a struct");
                };
                match declared.iter().position(|f| f.name == name.name) {
                    Some(index) => Ok(&mut Arc::make_mut(fields)[index]),
                    None => trap(target.span, format!("no field `{}`", name.name)),
                }
            }
            _ => trap(target.span, "this expression cannot be assigned to"),
        }
    }

    // ----- operators -----

    fn binary(
        &mut self,
        op: ast::BinaryOp,
        lhs: &'a ast::Expr,
        rhs: &'a ast::Expr,
        span: Span,
        env: &mut Env<'a>,
    ) -> Eval<'a, Value<'a>> {
        use ast::BinaryOp as B;

        // Short-circuit first: the kernel names && and || as the only two
        // operators that do not evaluate both sides.
        match op {
            B::And => {
                return match self.eval(lhs, env)? {
                    Value::Bool(false) => Ok(Value::Bool(false)),
                    Value::Bool(true) => self.eval(rhs, env),
                    _ => trap(lhs.span, "`&&` needs Bool"),
                };
            }
            B::Or => {
                return match self.eval(lhs, env)? {
                    Value::Bool(true) => Ok(Value::Bool(true)),
                    Value::Bool(false) => self.eval(rhs, env),
                    _ => trap(lhs.span, "`||` needs Bool"),
                };
            }
            _ => {}
        }

        let left = self.eval(lhs, env)?;
        let right = self.eval(rhs, env)?;

        match op {
            B::Add | B::Sub | B::Mul | B::Div | B::Rem => arith(op, &left, &right, span),
            B::BitAnd | B::BitOr | B::BitXor | B::Shl | B::Shr => {
                let (Value::Int(a), Value::Int(b)) = (&left, &right) else {
                    return trap(span, "bitwise operators need Int");
                };
                let result = match op {
                    B::BitAnd => a & b,
                    B::BitOr => a | b,
                    B::BitXor => a ^ b,
                    B::Shl | B::Shr => {
                        if *b < 0 || *b >= 64 {
                            return trap(span, "shift amount out of range 0..64");
                        }
                        if matches!(op, B::Shl) {
                            match a.checked_shl(*b as u32) {
                                Some(v) => v,
                                None => return trap(span, "integer overflow in `<<`"),
                            }
                        } else {
                            a >> b
                        }
                    }
                    _ => unreachable!(),
                };
                Ok(Value::Int(result))
            }
            B::Eq => Ok(Value::Bool(values_equal(&left, &right, span)?)),
            B::Ne => Ok(Value::Bool(!values_equal(&left, &right, span)?)),
            B::Lt | B::Le | B::Gt | B::Ge => {
                let ordering = compare(&left, &right, span)?;
                let result = match (op, ordering) {
                    (B::Lt, Some(o)) => o.is_lt(),
                    (B::Le, Some(o)) => o.is_le(),
                    (B::Gt, Some(o)) => o.is_gt(),
                    (B::Ge, Some(o)) => o.is_ge(),
                    // IEEE: any comparison with NaN is false.
                    (_, None) => false,
                    _ => unreachable!(),
                };
                Ok(Value::Bool(result))
            }
            B::Identity => trap(span, "`is` needs Shared values, which cannot be built yet"),
            B::And | B::Or => unreachable!("short-circuited above"),
        }
    }
}

/// Trapping integer arithmetic, IEEE float arithmetic.
fn arith<'a>(
    op: ast::BinaryOp,
    left: &Value<'a>,
    right: &Value<'a>,
    span: Span,
) -> Eval<'a, Value<'a>> {
    use ast::BinaryOp as B;
    match (left, right) {
        (Value::Int(a), Value::Int(b)) => {
            let result = match op {
                B::Add => a.checked_add(*b),
                B::Sub => a.checked_sub(*b),
                B::Mul => a.checked_mul(*b),
                B::Div => {
                    if *b == 0 {
                        return trap(span, "division by zero");
                    }
                    a.checked_div(*b)
                }
                B::Rem => {
                    if *b == 0 {
                        return trap(span, "remainder by zero");
                    }
                    a.checked_rem(*b)
                }
                _ => return trap(span, "not an arithmetic operator"),
            };
            match result {
                Some(value) => Ok(Value::Int(value)),
                // The kernel's rule: overflow traps, deterministically,
                // rather than wrapping (design/0003).
                None => trap(span, format!("integer overflow in `{}`", op.symbol())),
            }
        }
        (Value::Float(a), Value::Float(b)) => {
            let result = match op {
                B::Add => a + b,
                B::Sub => a - b,
                B::Mul => a * b,
                B::Div => a / b,
                B::Rem => a % b,
                _ => return trap(span, "not an arithmetic operator"),
            };
            Ok(Value::Float(result))
        }
        _ => trap(span, "arithmetic needs two Ints or two Floats"),
    }
}

/// Structural equality — the runtime twin of the sealed `Eq` property.
fn values_equal<'a>(a: &Value<'a>, b: &Value<'a>, span: Span) -> Eval<'a, bool> {
    match (a, b) {
        (Value::Int(x), Value::Int(y)) => Ok(x == y),
        // IEEE equality: NaN != NaN. The checker allows Float: Eq for exactly
        // this behaviour.
        (Value::Float(x), Value::Float(y)) => Ok(x == y),
        (Value::Bool(x), Value::Bool(y)) => Ok(x == y),
        (Value::Str(x), Value::Str(y)) => Ok(x == y),
        (Value::Char(x), Value::Char(y)) => Ok(x == y),
        (Value::Unit, Value::Unit) => Ok(true),
        (Value::List(xs), Value::List(ys)) => {
            if xs.len() != ys.len() {
                return Ok(false);
            }
            for (x, y) in xs.iter().zip(ys.iter()) {
                if !values_equal(x, y, span)? {
                    return Ok(false);
                }
            }
            Ok(true)
        }
        (Value::Map(xs), Value::Map(ys)) => {
            // Insertion order is display order, not identity: `==` is
            // key-value correspondence (0007 §3). Keys within a map are
            // unique, so equal lengths plus every pair found is a bijection.
            if xs.len() != ys.len() {
                return Ok(false);
            }
            for (key, value) in xs.iter() {
                let mut matched = false;
                for (other_key, other_value) in ys.iter() {
                    if values_equal(key, other_key, span)? {
                        matched = values_equal(value, other_value, span)?;
                        break;
                    }
                }
                if !matched {
                    return Ok(false);
                }
            }
            Ok(true)
        }
        (Value::ErrorValue(x), Value::ErrorValue(y)) => Ok(x == y),
        (
            Value::Struct {
                def: d1,
                fields: f1,
            },
            Value::Struct {
                def: d2,
                fields: f2,
            },
        ) => {
            if d1 != d2 || f1.len() != f2.len() {
                return Ok(false);
            }
            for (x, y) in f1.iter().zip(f2.iter()) {
                if !values_equal(x, y, span)? {
                    return Ok(false);
                }
            }
            Ok(true)
        }
        (
            Value::Enum {
                def: d1,
                variant: v1,
                payload: p1,
            },
            Value::Enum {
                def: d2,
                variant: v2,
                payload: p2,
            },
        ) => {
            if d1 != d2 || v1 != v2 || p1.len() != p2.len() {
                return Ok(false);
            }
            for (x, y) in p1.iter().zip(p2.iter()) {
                if !values_equal(x, y, span)? {
                    return Ok(false);
                }
            }
            Ok(true)
        }
        _ => trap(span, "these values cannot be compared with `==`"),
    }
}

/// `None` models IEEE's unordered comparisons (NaN).
fn compare<'a>(a: &Value<'a>, b: &Value<'a>, span: Span) -> Eval<'a, Option<std::cmp::Ordering>> {
    match (a, b) {
        (Value::Int(x), Value::Int(y)) => Ok(Some(x.cmp(y))),
        (Value::Float(x), Value::Float(y)) => Ok(x.partial_cmp(y)),
        (Value::Str(x), Value::Str(y)) => Ok(Some(x.cmp(y))),
        (Value::Char(x), Value::Char(y)) => Ok(Some(x.cmp(y))),
        // Bool and Unit satisfy `Ord` structurally, so `sorted` must order
        // them; false < true.
        (Value::Bool(x), Value::Bool(y)) => Ok(Some(x.cmp(y))),
        (Value::Unit, Value::Unit) => Ok(Some(std::cmp::Ordering::Equal)),
        _ => trap(span, "these values cannot be ordered"),
    }
}

/// Strip quotes and resolve the closed escape set. The lexer accepted the
/// literal, so anything unexpected here is a lexer bug worth trapping loudly.
fn unescape<'a>(raw: &str, span: Span) -> Eval<'a, String> {
    let inner = raw
        .strip_prefix('"')
        .and_then(|r| r.strip_suffix('"'))
        .or_else(|| raw.strip_prefix('\'').and_then(|r| r.strip_suffix('\'')))
        .unwrap_or(raw);

    let mut out = String::with_capacity(inner.len());
    let mut chars = inner.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('r') => out.push('\r'),
            Some('t') => out.push('\t'),
            Some('0') => out.push('\0'),
            Some('\\') => out.push('\\'),
            Some('"') => out.push('"'),
            Some('\'') => out.push('\''),
            other => {
                return trap(
                    span,
                    format!("unrecognised escape `\\{}`", other.unwrap_or(' ')),
                );
            }
        }
    }
    Ok(out)
}
