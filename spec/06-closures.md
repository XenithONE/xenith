# 06 — Closures

*Draft. Checked against `xenith 0.0.0`; design/0014 is the decision record.*

Closures are the one shipped form of function value, and they are **capability-effect-zero**:
a closure computes — it cannot print, read, spawn or hold a capability. The teaching sentence
every closure diagnostic converges on is the model of use:

> closures are plans — effects run in the enclosing named fn's `while` loop, and the closure
> returns data.

"Capability-effect-zero" is claimed, not "pure": a closure may still diverge or trap
(design/0014 §1 — the honest downgrade after review).

## 1. The two pillars

The guarantee is held by two independent checks, because either alone leaks:

1. **Body check.** A closure body is checked under an implicitly empty effect budget —
   `uses {}` with no clause to widen. Any effect, by any route — a capability method called
   directly, a call to a named fn whose `uses` is non-empty, a generic that turns out
   effectful — is `XN4006`.
2. **Capture restriction.** A closure may capture only **CaptureSafe** values (§3). A
   capability arriving through capture is `XN4005` before the body is even the question.

Capturing was never the only route to an effect — `|io| io.print(x)` captures nothing — which
is why the body check exists independently (design/0014: the original "no capture = pure"
claim was disproved in review).

## 2. Where a closure may appear

**Only as a call argument, for a parameter declared with a `fn(..)` type** — in 0.0, that
means the four std combinators `map`, `filter`, `fold`, `find` ([07 §3](07-std-prelude.md#3-the-method-surface)).

```xenith
fn main(io: Io) -> Result<Unit, Error> uses {Io.write} {
    let scores = [3, 10, 7, 22];
    let doubled = scores.map(f: |n| n * 2);
    let high = doubled.filter(f: |n| n > 10);
    let total = high.fold(init: 0, f: |acc, n| acc + n);
    let first = high.find(f: |n| n > 0);
    let shown = match first {
        Some(n) => n.to_text(),
        None => "none",
    };
    io.write(text: total.to_text().concat(other: " ").concat(other: shown))?;
    return Ok(unit);
}
```

```console
$ xenith run combinators.xn
78 20
```

A closure bound with `let`, returned, stored in a container or field is `XN1011`. This is the
restriction that keeps "call a function value" out of the language: a closure that cannot be
stored cannot be called somewhere its effects were never checked, and the named-argument rule
never meets a function-typed value it would have to re-litigate.

Two spellings only: `|x| expr` and `|x| { … }` (final expression is the value). Zero
parameters is `||`, a discarded parameter is `_`. **Closure parameters take no type
annotations** (`XN1009`, fix deletes it): the `fn(..)` type of the parameter the closure is
passed to already fixes them, so an annotation could only agree or lie. Rust-shaped forms —
`move`, `async` closures, reference patterns `|&x|`, `mut` or destructuring parameters — are
recognised and refused as `XN1010` with the same teach, at parse time: negative transfer is
handled as a shipped product, not an afterthought.

Named functions are **resolved, never passed**: `xs.map(f: double)` is `XN1008` — wrap the
call, `xs.map(f: |n| double(n))`. An effectful named fn riding into `map` as a value would
run outside every effect check, so the spelling does not exist.

## 3. CaptureSafe

A capture is a **copy taken once, at closure creation** (observable snapshot semantics;
nested closures re-snapshot). CaptureSafe is decided by the type, inductively
(design/0014 §1):

| Type | CaptureSafe? |
| --- | --- |
| `Int` `Float` `Bool` `String` `Char` `Unit` | yes |
| struct / enum / `List` / `Map` / `Option` / `Result` | when every component is |
| `fn(..)` types | yes |
| capabilities (`Io`) | **no** |
| `Shared`, `Task` — identity, shared mutation, resource handles | **no** (reserved unsafe before they even exist) |
| type parameters | **no** — no bound can promise safety in 0.0, so unresolved `T` never captures |
| the prelude `Error` value | yes (a plain value, no identity) |

Related refusals, each a distinct diagnostic so the fix is precise:

- **`var` capture is refused** (`XN4008`): a `var` exists to be reassigned, and updates after
  the snapshot would never be visible inside the closure — the two meanings contradict. The
  fix is to bind the current value to a `let` and capture that, which makes the copy explicit.
- **Self- and forward-reference are refused** (`XN4007`): a closure cannot refer to the
  binding its own `let` is initialising — there is nothing to copy yet. Definite
  initialisation, applied to captures. Recursion belongs in a named fn, which is resolved,
  not captured.
- Large captures are legal (the copy is the same D1 copy as any binding); the guidance is
  "pass big values as data through the combinator, capture the small configuration".

## 4. No early exit

`?`, `return`, `break` and `continue` cannot cross a closure boundary (`XN1012`). A closure
body is an expression producing a value for its combinator — `map` collects it, `filter` and
`find` test it, `fold` threads it — so there is no enclosing function to return from and no
loop to break. Failure-carrying iteration is written as a `while` loop in the enclosing named
fn, where `?` and `return` mean what they say:

```console
$ xenith check parse_all.xn
parse_all.xn:2:38: error[XN1012]: `?` cannot cross a closure boundary; closures cannot early-return; failure-carrying iteration belongs in a `while` loop
```

## 5. What closures do not have

No equality, hashing or serialisation of fn values (`fns.contains(f)` is a type error); no
`==` between closures. No effectful fn types, no `uses` on a `fn(..)` type — the effect set of
every closure is empty, so there is nothing to write. Eta-conversion, partial application,
let-bound closures and effect polymorphism are all future RFCs, listed once in
[00 §3](00-overview.md#3-adopted-but-not-shipped).
