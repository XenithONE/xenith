use std::sync::Arc;

use xenith_diag::Span;

use super::{Control, Eval, Interp, Value, trap};

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

// ------------------------------------------------------------- task threads

/// One unit of child work handed to the pool.
pub(super) type Job<'a> = Box<dyn FnOnce() + Send + 'a>;

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
pub(super) struct Pool<'a> {
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
    pub(super) fn start<'s>(
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
pub(super) struct Child<'a> {
    /// The callee as written, for trap attribution.
    name: String,
    /// The spawn site, so a child trap points where the sequential executor
    /// pointed.
    span: Span,
    outcome: std::sync::mpsc::Receiver<ChildOutcome<'a>>,
    /// Filled once committed, taken by `.await`.
    pub(super) value: Option<Value<'a>>,
}

impl<'a> Interp<'a> {
    // ----- the parallel executor (design/0017 §3) -----

    /// A safe point. Only a child ever has a cancel flag, so the parent
    /// walks straight through this.
    pub(super) fn safe_point(&self) -> Eval<'a, ()> {
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
    pub(super) fn submit_child(
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
    pub(super) fn commit_through(&mut self, upto: usize) -> Eval<'a, ()> {
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
    pub(super) fn drain_region(&mut self, start: usize) -> Eval<'a, ()> {
        if self.fate_sealed() || self.children.len() <= start {
            return Ok(());
        }
        self.commit_through(self.children.len() - 1)
    }
}
