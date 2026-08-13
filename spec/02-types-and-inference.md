# 02 — Types and inference

*Draft. Checked against `xenith 0.0.0`; design/0006 is the decision record.*

## 1. The types

Built-in scalar and value types: `Int` (64-bit signed), `Float` (IEEE 754 double), `Bool`,
`String`, `Char` (one Unicode scalar), `Unit` (sole value `unit`).

Prelude generic types: `List<T>`, `Map<K, V>`, `Option<T>` (`Some(v)` / `None`),
`Result<T, E>` (`Ok(v)` / `Err(e)`); the capability type `Io` and the opaque error type
`Error` ([07](07-std-prelude.md)). The names `Shared<T>` and `Task<T>` also resolve — they are
reserved by the concurrency design — but nothing in 0.0 can construct either
([00 §3](00-overview.md#3-adopted-but-not-shipped)).

User types are `struct` and `enum` declarations. Function types `fn(T) -> U` exist in the type
system but may be *written* only in the standard library's signatures; a function type in user
source is `XN1008` ([06 §2](06-closures.md#2-where-a-closure-may-appear)).

Type positions accept holes: `fn scale(factor: ??) -> Int` records a type goal instead of an
error (§7).

## 2. No implicit conversions

Type compatibility is structural identity of head and arguments — deliberately **not** a
subtyping relation. `Int` and `Float` never mix; `1` and `1.0` are unrelated values; there is
no numeric widening, no truthiness, no coercion to `String`. Conversions are spelled:
`to_` functions are total, `try_` functions return `Result` ([07 §6](07-std-prelude.md#6-naming-rules)).

The only "compatible with everything" types are the checker's two internal ones (§5).

## 3. Inference is local

The checker is bidirectional: `check` pushes an expected type down, `synth` reads one up.
Nothing is inferred **across** bindings or declarations — every parameter, return type and
field is written in source. The reason is repair, not purity: with inference at a distance, an
edit at a leaf can flip a type far away, which makes programs unrepairable in small steps
(design/0006 §1).

Where the expected type is present, it *seeds* what literals and constructors mean:

```xenith,in-fn
let xs: List<Int> = [];
var counts: Map<String, Int> = empty_map();
let outcome: Result<Int, String> = Ok(9);
let missing: Option<Int> = None;
```

Each of the four needs its annotation: an empty `[]` fixes no element type, `empty_map()` fixes
neither `K` nor `V`, `Ok(9)` fixes `T` but not `E`, `None` fixes nothing.

Seeding reaches **user** generic types on the same terms — a struct literal and a payload-less
variant read their type arguments from whatever expectation the position carries, whether that
is an annotation, a return type or a parameter:

```xenith
struct Pair<T> {
    a: T,
    b: T,
}

enum Wrap<T> {
    Hollow,
    Full(T),
}

fn take(p: Pair<Int>) -> Int {
    p.b
}

fn f() -> Wrap<Int> {
    let p: Pair<Int> = Pair { a: 1, b: 2 };
    let n = take(p: Pair { a: 3, b: 4 });
    Wrap.Hollow
}
```

## 4. When an annotation is required

Where nothing determines a type, the checker refuses (`XN3005`) rather than invent one. **This
is a specification, not a shortfall** (design/0006 §1-1): local inference means `let x = ??;`
has no type to discover.

Positions that require an annotation today:

- `let x = ??;` — a hole with nothing above it. Write `let x: Config = ??;`.
- A call whose generic result nothing pins down: `xs.map(f: …)` with `U` undetermined, `Ok(5)`
  bound to an un-annotated `let`.
- **A generic user struct literal in a position that fixes nothing** — `let p = Pair { a: 1,
  b: 2 };`. The refusal names the undetermined parameters, and the fields report nothing
  further: one missing annotation is one diagnostic (§5).
- **A payload-less variant of a generic user enum, likewise** — `let w = Wrap.Hollow;`.
  Payload-carrying variants need no help (`Wrap.Full(5)` binds `T` from the payload).

A `const` is the one declaration whose initializer is not checked bidirectionally at all: it is
folded, and its type comes out of the fold, so `const NAME: String = 5;` is an ordinary
mismatch and `const N: Int = f();` is `XN3012`
([01 §5](01-lexical-and-syntax.md#5-items)).

## 5. Two internal types, and the poison discipline

| Kind | Meaning | Diagnostics | Goals |
| --- | --- | --- | --- |
| `Error` | recovery poison from an already-reported failure | never reported itself | never |
| `Hole` | a deliberate gap — **not** an error | none | **one each** |
| anything else | an actual type | as usual | — |

Both are compatible with everything, so checking continues around them; they differ only in
goal emission. Mismatches are reported **only between two concrete types** — one mistake
produces one diagnostic, not an avalanche, and a hole produces none at all (design/0006 §2).

## 6. Generics and sealed properties

User functions take type parameters with bounds: `fn count_of<T: Eq>(items: List<T>, wanted: T) -> Int`.
The bound vocabulary is **sealed** — five properties, decided by the compiler from a type's
structure, with no user implementation syntax and no extension point:

```text
Eq    can be compared with == / !=
Ord   has a total order (sort keys, ordered bounds)
Hash  can key a Map (implies Eq in use: Map keys demand Eq + Hash)
Copy  copies implicitly (scalars only)
Text  renders for debug output — total for every type today
```

Satisfaction is structural, recursive, and automatic — there is no `derive`:

| Type | Eq | Ord | Hash |
| --- | --- | --- | --- |
| `Int` `Bool` `Char` `Unit` | yes | yes | yes |
| `String` | yes | yes | yes |
| `Float` | yes | **no** | **no** |
| struct / enum | when every component is | **never** | when every component is |
| `List<T>` `Option<T>` `Result<T, E>` `Map<K, V>` | when arguments are | never | when arguments are |
| `Shared<T>`, function types, capabilities | no | no | no |

Two exclusions are load-bearing (design/0006 §3-3, §3-4):

- **`Float` is `Eq` but neither `Ord` nor `Hash`.** IEEE NaN makes float equality a non-total
  relation, so it can ground neither a sort nor a hash key. The comparison *operators*
  `< <= > >=` still accept `Float` — a use-site partial order is well-defined — but
  `xs.sorted()` on `List<Float>` and `Map<Float, V>` are both `XN3010`.
- **Aggregates are never `Ord`.** An order derived from field declaration order would silently
  change when fields are reordered. Where you need one, pass the comparison explicitly — what
  runs is then readable at the call site.

Why sealed properties instead of traits: the implementation for a type is *unique*, so "which
impl applies here" — the classic model failure — cannot arise; bound checking is a table
lookup, not a search. `trait` stays a reserved word permanently.

Bounds are enforced at call sites once the receiver binds the parameters:

```console
$ xenith check floatsort.xn
floatsort.xn:3:18: error[XN3010]: `sorted` requires `T: Ord`, but `Float` does not satisfy it
```

## 7. Typed holes

`??` and `??name` are legal expressions and legal types. **A partial program is a normal
state**: a file whose only gaps are holes checks clean, and each hole records a *goal* — the
expected type, the bindings in scope, and the effect budget, all of which the bidirectional
checker had in hand at that position anyway (design/0006 §1: the goal is the checker's state,
written down).

```console
$ xenith goals scores.xn
scores.xn:59:5 — hole ??lookup in try_find
  expected: Result<Player, ScoreError>
  in scope: name: String
  effects:  none permitted
  candidates:
    1. Err(??)
    2. Ok(??)
    3. try_award(player: ??, points: ??)
```

Candidates are deliberately **scaffolds, not completions**: nested holes mark what still needs
deciding, and named arguments are already spelled out — a partially correct skeleton with
explicit gaps beats a fully formed but irrelevant expression (design/0006 §4-1). Symbols that
produce the right type but need effects this position does not permit are listed as `blocked`,
with the reason:

```console
$ xenith goals quiet.xn
quiet.xn:2:5 — hole ??step in quiet
  expected: Result<Unit, Error>
  in scope: io: Io
  effects:  none permitted
  candidates:
    1. Err(??)
    2. Ok(??)
  blocked:  io.write — needs {Io.write}, not permitted here
```

A type-position hole records a `TypeGoal` (`expected: <type>`) rather than pretending to be
every type. Running a program that reaches a hole is a precise trap naming it
([04 §5](04-evaluation.md#5-entry-and-exit)).

## 8. `match` is checked for exhaustiveness

Every `match` must cover every value of its scrutinee. The check is Maranget's usefulness
algorithm asked one question — after all unguarded arms, would a wildcard row still be useful?
— and when the answer is yes, the diagnostic carries a **witness** rendered in source syntax:

```console
$ xenith check match.xn
match.xn:8:5: error[XN5001]: this `match` is not exhaustive: `Rank.Silver` is not covered
```

"`Rank.Silver` is not covered" is actionable; "not exhaustive" is not.

Rules that follow from the algorithm:

- A guarded arm contributes **nothing** to coverage — its guard can be false at runtime, so the
  value must land somewhere else regardless.
- `Int`, `Float`, `String` and `Char` cannot be enumerated by literals; a `match` over one of
  them needs a binding or `_` arm, and their witness renders as `_`.
- OR-patterns cover each alternative; variant patterns are covered when their payload patterns
  are.
