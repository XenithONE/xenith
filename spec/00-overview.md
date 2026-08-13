# 00 — Overview

*Draft, describing `xenith 0.0.0`.*

Xenith is an experimental general-purpose language designed so that a **compiler can guide a
language model to correct code**. The premise (design/0001, design/0002): a model's dominant
failure is guessing while local intent is still undetermined, so the language makes guessing
unnecessary — partial programs are legal and queryable, signatures cannot lie about effects,
inference is local so repairs stay local, and diagnostics are a machine protocol that carries
the knowledge a repair needs. The syntax is deliberately boring — Rust- and TypeScript-shaped —
because the novelty budget is spent on the compiler–model protocol, not on punctuation.

## 1. What this specification is

A consolidated, current description of the shipped language. Two commitments hold everywhere:

1. **Every claim is true of the compiler as it ships today.** Where a design is adopted but
   not implemented, it appears in §3 — nowhere else in these chapters is future tense dressed
   as present.
2. **Every complete example is real.** Complete programs were checked and run against the
   `xenith 0.0.0` binary when this draft was written, with outputs recorded verbatim;
   fragments are marked as fragments. CI additionally parses every `xenith` code block in
   this directory with the current compiler, so examples cannot rot silently
   (design/0006 §6: a stale canonical example is a compiler bug in effect).

What this specification is **not**: a conformance standard. No test suite is generated from
these pages, no conformance machinery exists, and none is promised here. The draft is
normative in intent — a disagreement between it and the compiler is a bug in one of them —
and that is the strongest claim it makes.

The [design records](../design/) hold the full reasoning and the reversals; chapters cite
them by filename. Where this draft and a design record disagree, the shipped compiler is the
tiebreak and the draft follows the compiler.

## 2. What ships in 0.0

- **Front end** — total parser (recovery everywhere), canonical formatter that verifies its
  own output ([01](01-lexical-and-syntax.md)).
- **Types** — local-only bidirectional checking, no implicit conversions, sealed structural
  properties (`Eq` `Ord` `Hash` `Copy` `Text`) instead of traits, generic functions, structs
  and enums — constructed from the expected type — exhaustive `match` with counter-example
  witnesses ([02](02-types-and-inference.md)).
- **Constants** — `const NAME: T = <literal, or arithmetic over literals>;`, folded while the
  module is checked, `pub` like any other item ([01 §5](01-lexical-and-syntax.md#5-items)).
- **Typed holes** — `??` / `??name` in expression and type position; a partial program is a
  normal, queryable state ([02 §7](02-types-and-inference.md#7-typed-holes)).
- **Effects and capabilities** — checked `uses { … }` sets, subset rule at every call edge,
  capabilities as ordinary values, `Io.write` as the shipped effect surface
  ([03](03-effects-and-capabilities.md)).
- **Execution** — a deterministic tree-walking interpreter: value semantics, strict
  left-to-right order, trapping arithmetic, exact stdout ([04](04-evaluation.md)).
- **Modules** — file = module under `src/`, `use` as dependency declaration, fully qualified
  references, `pub` visibility with an API-closure check, read-not-write `pub` structs
  ([05](05-modules-and-projects.md)).
- **Closures** — capability-effect-zero function values in call-argument position, with the
  `map` / `filter` / `fold` / `find` combinators ([06](06-closures.md)).
- **Task structure** — `scope { … }` / `spawn f(args)` / `j.await`: pure children run to
  completion at the spawn point, consumed exactly once, gated by the `Task.spawn` effect;
  no concurrency claim attached ([04 §7](04-evaluation.md#7-task-structure)).
- **Prelude** — `List` / `Map` / `String` / `Int` / `Option` / `Io` method surface, fixed
  small ([07](07-std-prelude.md)).
- **Diagnostics and tooling** — stable codes, machine-applicable fixes, teaching blocks with
  a byte-identical off switch, one JSON wire contract, `goals` / `type-at` / `producers`
  queries, an MCP server speaking the same wire ([08](08-diagnostics-and-tooling.md)).

## 3. Adopted but not shipped

This is the **single** list. Everything below has a decided design or a deliberately reserved
footprint, and none of it works today. The compiler's own boundary marker is `XN1008`: the
parser accepts several of these forms so that a half-edited file still yields a tree to
repair from, and the checker then refuses them — accepting syntax is not shipping a feature,
and passing one through half-checked would let an effect escape its declaration.

**Concurrency (design/0004, narrowed by design/0015).** The task boundary shipped
(design/0015): `scope` / `spawn` / `.await`, pure children, the `Task.spawn` effect, and the
`XN6xxx` diagnostic family enforcing it — with no concurrency claim attached. What remains
adopted but unshipped from design/0004: actual concurrent execution and its executors
(`IoExecutor`, `FrameExecutor`); `Transfer` / `ShareSafe` marker properties with
compiler-only derivation; capability transfer across the task boundary; `Shared<T>` with
`share()`, `Mutex`, atomics, channels; lock guards that cannot cross suspension points; the
memory-model text. Reserved footprint still visible in 0.0: the keywords `async` / `await` /
`move`, the type names `Shared<T>` and `Task<T>` (they resolve; nothing constructs them),
and the `is` identity operator (checks only against `Shared`, so no well-typed program
reaches it).

**Refused today under `XN1008`:**

- `async fn`, and `.await` anywhere other than the design/0015 task-handle position — no
  effect rules exist for suspension yet.
- `for` loops — iteration is a future RFC; write `while` + `len()` + `get(index:)`.
- Function types `fn(T) -> U` written in user source — they appear only in std signatures.
- A named function used as a value — named functions are resolved, never passed; wrap the
  call in a closure.

**Designed, no syntax reserved:**

- The design/0003 kernel's affine layer: moves with use-after-move errors, explicit
  `.clone()` / `.share()`, `wrapping_*` / `saturating_*` arithmetic variants (only
  `checked_add` ships). Shipped 0.0 semantics is the D1 value-copy model, on which the affine
  rules are designed to layer later (design/0007 D1).
- `sorted_by` and an `Ordering` type — deferred until function-typed parameters in user-facing
  signatures are designed (design/0007 D3); until then aggregates have no sort spelling.
- Constant expressions wider than literals and arithmetic over them: a `const` naming another
  `const`, or calling anything. The exclusion is what keeps 0.0 free of const initialization
  order — and therefore of the initialization-cycle diagnostic design/0010 §5 reserves for it.
  Widening the grammar is the change that has to answer that question, not the const surface
  itself ([01 §5](01-lexical-and-syntax.md#5-items)).
- Let-bound closures, fn values in general positions, partial application, effectful fn
  types (design/0014 §5).
- Further capabilities (`Fs`, `Net`, clocks) and `std` as real modules; item-level `use`
  forms remain rejected by design, not by delay (design/0010 §8).
- The reserved words `trait impl where mod loop defer yield capability effect extern static
  macro` fence off the remaining design space.

## 4. Reading order

[01 Lexical and syntax](01-lexical-and-syntax.md) →
[02 Types and inference](02-types-and-inference.md) →
[03 Effects and capabilities](03-effects-and-capabilities.md) →
[04 Evaluation](04-evaluation.md) →
[05 Modules and projects](05-modules-and-projects.md) →
[06 Closures](06-closures.md) →
[07 The prelude](07-std-prelude.md) →
[08 Diagnostics and tooling](08-diagnostics-and-tooling.md).

Per-topic decision records: kernel semantics design/0003 · type checking design/0006 ·
formatter design/0005 · std design/0007 · diagnostics-that-teach design/0009 · modules
design/0010 · module-call teaching design/0012 · project analysis design/0013 · closures
design/0014 · concurrency design/0004 · task structure design/0015.
