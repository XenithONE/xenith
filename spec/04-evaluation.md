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
can pass or fail a test by luck cannot be measured (design/0011).
