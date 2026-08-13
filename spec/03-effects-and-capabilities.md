# 03 — Effects and capabilities

*Draft. Checked against `xenith 0.0.0`; design/0002 §8 and design/0003 carry the reasoning.*

## 1. The clause

A function declares its effects between return type and body:

```xenith
fn log_line(io: Io, text: String) -> Result<Unit, Error> uses {Io.write} {
    io.write(text: text)
}

fn plan() -> Int uses {} {
    1
}
```

**An absent clause means the empty set** — the strongest claim a signature can make, identical
to writing `uses {}`. Effect names are dotted paths (`Io.write`); the set is written sorted and
deduplicated, and compares order-independently.

## 2. The subset rule

One rule does all the work:

> Every effect a call performs must be contained in the enclosing function's declared set.

Effects are checked at every call edge, so they propagate up the call chain by construction: a
caller of `uses {Io.write}` code must itself declare `Io.write`, and so on to `main`. A
violation is `XN4001`, and it carries a machine-applicable fix that edits this function's
header — extending a non-empty clause, filling an empty one, or creating one:

```console
$ xenith check shout.xn
shout.xn:2:5: error[XN4001]: this call uses {Io.write}, which `shout` does not declare
2 |     io.write(text: "hi")
  |     ^^^^^^^^^^^^^^^^^^^^
  fix: declare `uses {Io.write}`
```

Applying that fix is one of two correct answers, and the diagnostic's `explain` says so: the
other is to move the effectful call out to a caller that already holds the capability. Which
is right is a design decision the compiler does not make.

This is what makes a signature trustworthy — a reader (or a model) sees `uses {Fs.read}` and
knows the function touches nothing else. That guarantee needs the closure rules to stay
airtight: a function value that escaped its checked context could smuggle an effect, which is
why closures carry an implicitly empty effect budget and named functions are not values
([06 §1](06-closures.md#1-the-two-pillars)).

## 3. Capabilities are ordinary values

There is no ambient authority: no global print, no reachable-from-anywhere filesystem. A
capability is a plain value of a capability type, handed to `main` as a parameter and passed
down explicitly:

```xenith
fn greeting(name: String) -> String {
    "Hello, ".concat(other: name)
}

fn main(io: Io) -> Result<Unit, Error> uses {Io.write} {
    io.write(text: greeting(name: "world"))?;
    return Ok(unit);
}
```

`greeting` takes no capability and declares no effects, so its signature proves it cannot
perform IO — checkable by reading one line. Effect (permission to act, on the function) and
capability (the value acted through) are deliberately both required: the effect makes the
signature honest, the capability makes the data flow explicit.

Capability types satisfy no sealed property and are not CaptureSafe
([06 §3](06-closures.md#3-capturesafe)), so a capability cannot be compared, keyed, or
smuggled into a closure.

## 4. What ships today

The shipped effect surface is two operations:

| Operation | Form | Effect |
| --- | --- | --- |
| `Io.write` | `write(text: String) -> Result<Unit, Error>` — a method on `Io` | `Io.write` |
| `Task.spawn` | `spawn f(args)` — a construct, not a method ([04 §7](04-evaluation.md#7-task-structure)) | `Task.spawn` |

`Io.write` writes exactly `text` to stdout — no newline is added
([04 §6](04-evaluation.md#6-determinism)). `Io` reaches a program only as `main`'s parameter
([04 §5](04-evaluation.md#5-entry-and-exit)).

`Task.spawn` is performed by the `spawn` construct inside a `scope { .. }` block: the
enclosing function declares it like any other effect, and the `XN4001` fix inserts it. The
spawned child itself declares an empty `uses` set — capabilities and effects do not cross the
task boundary ([04 §7](04-evaluation.md#7-task-structure)) — so `Task.spawn` never propagates
*through* a child, only up the parent's call chain. Declaring `uses {Task.spawn}` without
spawning was legal before design/0015 gave it meaning (the namespace below is open) and stays
legal now.

**The effect namespace is open.** `uses {Net.send}` on a function that never performs it is
accepted: declared-but-unused effects are not an error, and effect names are not validated
against a registry. The checker constrains what a function *does*, not what it promises it
might do. Over-declaring weakens a signature's information value, so the convention is to
declare exactly what the body needs — the `XN4001` fix maintains that discipline mechanically.

Effect-polymorphic functions, effect rows, and further capabilities (`Fs`, `Net`, clocks) are
design work, not shipped surface — [00 §3](00-overview.md#3-adopted-but-not-shipped).
