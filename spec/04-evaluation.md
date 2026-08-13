# 04 — Evaluation

*Draft. Checked against `xenith 0.0.0`. The execution engine is a tree-walking interpreter,
deliberately: the benchmark needs execution that is correct and deterministic, not fast
(design/0007, design/0013). Peak performance is a non-goal.*

## 1. Values are values

The observable semantics is **value semantics** (design/0007 D1). Binding, assignment,
argument passing, returning, pattern extraction and closure capture all behave as copies;
reads from containers return copies of the stored value. There are no references, borrows or
views, and unique values have no observable aliasing:

```xenith
fn main(io: Io) -> Result<Unit, Error> uses {Io.write} {
    var original = [1, 2, 3];
    var copy = original;
    copy.push(item: 4);
    io.write(text: original.len().to_text())?;
    io.write(text: " ")?;
    io.write(text: copy.len().to_text())?;
    return Ok(unit);
}
```

```console
$ xenith run copies.xn
3 4
```

An implementation may share storage under the hood (copy-on-write, reference counting), but
the observed result must be indistinguishable from deep copies. The only in-place writes in
the language are assignment and the mutating prelude methods, and both require a `var` place
([07 §2](07-std-prelude.md#2-mutation-discipline)).

Values have no identity. `==` is structural equality where `Eq` holds
([02 §6](02-types-and-inference.md#6-generics-and-sealed-properties)); the identity operator
`is` is reserved for `Shared`, which cannot be constructed in 0.0, so no well-typed program
reaches it today.

## 2. Evaluation order

Strict left-to-right, everywhere: call arguments, method receivers before arguments, binary
operands, field initialisers in written order. The order is fixed so that side-effect order is
never something to guess — if order is determined, repair is deterministic (design/0003 §1).

`&&` and `||` are the **only** short-circuiting forms; everything else evaluates all its
operands. `match` evaluates the scrutinee once, tests arms top to bottom, and evaluates a
guard only when its arm's pattern matched; a false guard falls through to the next arm.

## 3. Arithmetic traps

There is no undefined behaviour and no silent wrap. Failures are deterministic **traps** —
precise runtime errors carrying a source span:

| Operation | Trap |
| --- | --- |
| `+` `-` `*` on `Int` out of range | `integer overflow in `…`` |
| `/ 0`, `% 0` on `Int` | `division by zero` / `remainder by zero` |
| unary `-` on the minimum `Int` | overflow trap |
| `<<` `>>` with shift outside `0..64` | `shift amount out of range 0..64` |
| `<<` overflowing | `integer overflow in `<<`` |
| integer literal beyond 64 bits | trap at evaluation |

`Int` division truncates toward zero and remainder takes the dividend's sign:
`(0 - 7) / 2 == -3`, `(0 - 7) % 2 == -1`. Where overflow is an expected case, use
`checked_add`, which returns `Option<Int>` instead of trapping. `Float` follows IEEE 754:
it never traps, and `NaN != NaN`.

## 4. The try operator

`expr?` unwraps or propagates, and is the only early-failure form:

- On `Result`: `Ok(v)` yields `v`; `Err(e)` returns the whole `Err(e)` from the enclosing
  function, whose return type must be `Result<_, E>` with a compatible `E`.
- On `Option`: `Some(v)` yields `v`; `None` returns `None`, and the function must return
  `Option<_>`.
- Anything else is a type error at check time.

There are no exceptions and no `null`; failure travels in return types, and `match` handles
what `?` does not. Inside a closure body `?` is refused outright
([06 §4](06-closures.md#4-no-early-exit)).

## 5. Entry and exit

`xenith run` type-checks first and **refuses to execute a file with any diagnostic** — running
would be executing guesses. A file whose only gaps are holes runs; **reaching** a hole is a
precise trap naming it:

```console
$ xenith run steps.xn
step one
steps.xn:3:21: runtime error: reached hole ??next — ask `xenith goals` what belongs here, then fill it
```

Running a partial program tells you which hole to fill next — a workflow, not a failure mode.

Execution enters `fn main`, which receives its capabilities as parameters (`main(io: Io)`) and
returns a `Result`. Exit codes:

| Exit | Meaning |
| --- | --- |
| 0 | `main` returned `Ok` |
| 1 | `main` returned `Err` |
| 2 | refused: the file (or project) has diagnostics |
| 101 | a runtime trap fired — arithmetic, a reached hole |

In a project, `run` enters `src/main.xn` whichever file was named, and any diagnostic anywhere
in the project refuses the run ([05 §5](05-modules-and-projects.md#5-checking-and-running-a-project)).
A project without `src/main.xn` is a library: it checks, and `run` reports there is nothing to
run.

## 6. Determinism

Same program, same input, same behaviour — byte for byte:

- Evaluation order is fixed (§2) and arithmetic is trapping (§3); there is no scheduler, no
  timer, and no source of nondeterminism in the shipped language.
- `io.write(text:)` writes exactly `text` to stdout, no newline appended, in evaluation order.
- `Map` iteration order is insertion order, normatively specified
  ([07 §4](07-std-prelude.md#4-map-order-is-normative)) — not an implementation accident.
- Debug rendering (`to_text` on `Int`, `List.join`'s element rendering) is total and
  deterministic.

Determinism is load-bearing: benchmark tasks are judged on exact stdout, and a language that
can pass or fail a test by luck cannot be measured (design/0011). Children run in parallel
(§8) without denting it, because the parent is silent while they fly — but that is a rule
with a name and a cost, not a free consequence of purity, and §8 states exactly what the
guarantee does and does not cover.

## 7. Task structure

design/0015 ships the task boundary — the types, the structure, the effect gate — with no
concurrency claim attached. Three forms:

- `scope { .. }` — a block statement or expression opening a task region. `spawn` is legal
  only inside one. A scope is not a value; its value is its tail, like any block.
- `spawn f(args)` — call a named fn as a child. The callee must declare an **empty `uses`
  set** and every parameter type must be CaptureSafe ([06 §3](06-closures.md#3-capturesafe)):
  capabilities do not cross the task boundary, so a child computes and never performs
  effects. Spawning itself is the effect `Task.spawn`, declared by the enclosing function
  ([03 §4](03-effects-and-capabilities.md#4-what-ships-today)).
- `j.await` — consume the handle bound by `let j = spawn f(args);`, exactly once on every
  path. The handle does nothing else: it cannot be copied, stored, returned, passed,
  captured or annotated, and a non-Unit result still live at the scope's normal exit is
  refused. A Unit child may be fired as a statement: `spawn ping();`.

The arguments are evaluated at the spawn point — normal order, exactly once — and the child
starts there. The handle is a ready box; `.await` moves the result out. A trap inside the
child surfaces at the spawn statement, naming the child. On an early exit (`?`, `return`, a
trap) an unconsumed result is discarded — the child was pure, so nothing observable is lost.

```xenith
fn square(n: Int) -> Int {
    n * n
}

fn parse(text: String) -> Result<Int, Error> {
    text.try_to_int()
}

fn main(io: Io) -> Result<Unit, Error> uses {Io.write, Task.spawn} {
    let total = scope {
        let a = spawn square(n: 4);
        let b = spawn parse(text: "26");
        a.await + b.await?
    };
    io.write(text: total.to_text())?;
    return Ok(unit);
}
```

```console
$ xenith run tasks.xn
42
```

A child may fail purely — return `Result` and let the parent propagate with `j.await?`, as
above. The diagnostics own the rest of the contract: `XN6001`–`XN6011` refuse spawns outside
scopes, effectful or non-CaptureSafe children, escaped handles, every await count other than
exactly-once, and effects performed while a task is in flight (§8). `scope`, `spawn` and
`.await` are banned inside closure bodies ([06](06-closures.md)); `async fn` remains outside
the language.

## 8. Children run in parallel

**Children may run at the same time, on other threads** (design/0017). `spawn` starts one;
nothing about it is observable through the handle, and no program can ask whether a child has
finished.

The one rule that makes this cost nothing is **XN6011**: from the first `spawn` in a scope
until every task it created has been consumed, the parent performs no capability operation
and calls no function with a non-empty `uses` set. The window is silent, so there is nothing
for a child's timing to interleave with:

```xenith
fn plan(n: Int) -> Int {
    n * 2
}

fn main(io: Io) -> Result<Unit, Error> uses {Io.write, Task.spawn} {
    scope {
        let a = spawn plan(n: 1);
        let b = spawn plan(n: 2);
        let total = a.await + b.await;
        io.write(text: total.to_text())?;
    }
    return Ok(unit);
}
```

Moving that `io.write` above the awaits is `XN6011`.

The statement form `spawn f(..);` binds no handle, so the scope's closing brace is what joins
it: an effect that must follow one goes after the scope.

Traps and divergence surface at the join, and **outcomes commit in spawn order** — the order
in which a single-threaded run would have decided them. A trapping child 1 is reported even
if child 2 trapped first in wall-clock time; a diverging child 1 hangs the program even
though child 2 already finished; a fine child 1 followed by a trapping child 2 reports child
2's trap. A child's trap outranks anything the parent went on to do, because the child's fate
was sealed at its spawn statement. Once a trap commits, the remaining children are stopped —
so a trapping child beside a diverging sibling still reaches exit 101 rather than waiting
forever. This is not a cancellation feature: no program can request it.

The result is that **stdout, exit codes and diagnostics are exactly what a sequential run
produces** — the §6 guarantee is intact, and the compiler tests it by running every task
program through both executors and comparing the bytes.

What parallelism does change is **outside the determinism promise**: peak memory, the number
of OS threads, wall-clock time, and host exhaustion (out-of-memory, stack overflow, a thread
the host refuses to create). A program near a resource limit may fail under one executor and
not the other; nothing in §6 ever covered those, and design/0017 says so rather than leaving
it to be discovered.
