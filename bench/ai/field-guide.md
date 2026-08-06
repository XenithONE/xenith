# Xenith field guide

You are writing **Xenith**, a small language shaped like Rust and TypeScript. This guide is
complete: everything the current compiler accepts is described here, and nothing else exists.
Do not import knowledge from Rust — where Xenith differs, this guide says so.

## A complete program

```xenith
struct Player {
    name: String,
    var score: Int,
}

enum Rank {
    Bronze,
    Gold,
}

fn rank_of(score: Int) -> Rank {
    if score >= 1000 {
        Rank.Gold
    } else {
        Rank.Bronze
    }
}

fn main(io: Io) -> Result<Unit, Error> uses {Io.write} {
    var player = Player { name: "ada", score: 990 };
    player.score = player.score + 15;
    let label = match rank_of(score: player.score) {
        Rank.Gold => "gold",
        Rank.Bronze => "bronze",
    };
    io.write(text: player.name.concat(other: ": ").concat(other: label))?;
    return Ok(unit);
}
```

## Rules that differ from what you might assume

1. **Statements end with `;`.** A block's value is its final expression written *without* `;`.
   Whitespace and newlines never carry meaning.
2. **Calls with two or more arguments must name every argument, in declaration order:**
   `award(player: p, points: 10)`. One-argument calls may stay positional. Enum constructor
   payloads are positional: `Ok(v)`, `NotFound(id)`.
3. **Mutability is a keyword, not a modifier:** `let x = 1;` is immutable, `var x = 1;` is
   mutable. Struct fields are immutable unless declared `var score: Int`.
4. **No implicit conversions.** `Int` and `Float` never mix; `1` and `1.0` are different types.
   Integer overflow and division by zero **trap at runtime** — use `checked_add` when overflow is
   possible.
5. **No null, no exceptions.** Failure is `Option<T>` or `Result<T, E>`, matched exhaustively.
   `expr?` unwraps `Ok`/`Some` or returns the `Err`/`None` from the enclosing function, whose
   return type must agree.
6. **Effects are declared.** A function that performs IO says so: `uses {Io.write}` after the
   return type. Calling an effectful function requires the *caller* to declare those effects too.
   No `uses` clause means the function performs no effects at all.
7. **Capabilities are values.** `main(io: Io)` receives `Io`; there is no global print. Pass `io`
   down to any function that needs it.
8. **Return needs an operand:** `return Ok(unit);`, never a bare `return;`.
9. **`==` is structural equality.** Functions cannot be compared. `Float` compares with
   `<` `>` but cannot be a sort key. There is no `===`.
10. **Enums are referenced through their name:** `Rank.Gold`, `Grade.Pass(95)`. Only `Ok`, `Err`,
    `Some`, `None` may be written unqualified.

## Declarations

```xenith
fn add(a: Int, b: Int) -> Int {
    a + b
}

fn get<K: Eq + Hash, V>(key: K) -> Option<V> {
    None
}

struct Point {
    x: Int,
    var y: Int,
}

enum Shape {
    Circle(Int),
    Rect(Int, Int),
    Empty,
}

const LIMIT: Int = 1_000;
```

Generic bounds come from a sealed set: `Eq`, `Ord`, `Hash`, `Copy`, `Text`. There are no traits
and no way to implement a property by hand — the compiler derives them structurally.

## Statements and expressions

```xenith,in-fn
let total = 1 + 2 * 3;
var count = 0;
count = count + 1;
count += 1;

if count > 0 {
    count = 0;
} else {
    count = 1;
}

while count < 10 {
    count = count + 1;
    if count == 5 {
        continue;
    }
    if count == 9 {
        break;
    }
}

let grade = if count >= 9 { "high" } else { "low" };

let description = match count {
    0 => "zero",
    n if n < 0 => "negative",
    _ => "positive",
};
```

`match` must be exhaustive. Patterns: literals (`0`, `"ok"`), bindings (`n`), wildcards (`_`),
variants (`Ok(v)`, `Rank.Gold`, `Shape.Rect(w, h)`), struct patterns
(`Player { name, score: s }`), alternatives (`Shape.Empty | Shape.Circle(_)`), and guards
(`n if n > 0`).

## Modules

A file under `src/` is one module when `xenith.toml` marks the project root: `src/game/player.xn`
is the module `game.player`. A lone file needs neither. `use game.player;` declares a dependency
on that module — the only form `use` takes — and items are then referenced fully qualified
(`game.player.Player`), never imported or aliased. Top-level items are private to their module
unless declared `pub`. A `pub` struct can be constructed, read and matched from outside, but its
fields cannot be assigned across the boundary — mutation goes through the owning module's `pub`
functions. An unused `use` is an error.

## Types and the prelude

Built-in types: `Int` (64-bit, trapping), `Float` (IEEE), `Bool`, `String`, `Char`, `Unit`
(its only value is `unit`), `Option<T>` (`Some(v)` / `None`), `Result<T, E>` (`Ok(v)` / `Err(e)`),
`List<T>`, `Map<K, V>` (keys need `Eq + Hash`; `Float` cannot be a key), and the capability `Io`.

Construction: a list is written `[1, 2, 3]`; an empty `[]` needs an expected type
(`let xs: List<Int> = [];`). A map starts from the prelude function `empty_map()`, whose key and
value types come from the annotation: `var m: Map<String, Int> = empty_map();`. Reads from
containers return copies; methods that mutate their receiver require a `var` binding.

`List`, `Map`, and `String` carry a substantial method set beyond the core table below. Their
exact names and signatures are what the API reference (when provided) or the compiler's
`goals`/`producers` output (when available) states.

The core method set:

| Method | Signature |
| --- | --- |
| `Int.checked_add` | `(other: Int) -> Option<Int>` |
| `Int.to_text` | `() -> String` |
| `String.concat` | `(other: String) -> String` |
| `Option<T>.to_result` | `(error: E) -> Result<T, E>` |
| `Io.write` | `(text: String) -> Result<Unit, Error>` — effect `Io.write`; writes exactly `text`, no newline added |

There is no `format!` and no string interpolation — build strings with `concat` and `to_text`.

## Holes

`??` or `??name` is a legal expression or type: the program still compiles. Running a program
traps when a hole is reached, naming it. `xenith goals` reports each hole's required type, the
bindings in scope, permitted effects, and candidate expressions. If you do not know what belongs
somewhere yet, write a hole rather than guessing.

## Naming rules (fixed by the language)

`to_` total conversion · `try_` returns `Result` · `is_` / `has_` returns `Bool` ·
`from_` constructor · `with_` returns a modified copy · `get`/`checked_` returns `Option`.
Types are `UpperCamel`, functions and bindings are `lower_snake`.

## The compiler is the workflow

`xenith check file.xn` — diagnostics with stable codes and, often, machine-applicable fixes.
`xenith run file.xn` — type-checks, then executes `fn main`. Exit 0 on success; a file with any
diagnostic is refused. `xenith goals file.xn` — every hole, with what belongs there.
