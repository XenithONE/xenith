# 01 — Lexical structure and syntax

*Draft. Every rule here is checked against `xenith 0.0.0`; every `xenith` code block in this file
is parsed in CI.*

## 1. Source text

Source is UTF-8. **Whitespace carries no meaning anywhere**: newlines and indentation never
change what a program does, and reflowing a line cannot change behaviour. Any Unicode
space-like character is accepted as ordinary whitespace, precisely because whitespace means
nothing (design/0003 §3).

Comments run to end of line: `//` for an ordinary comment, `///` for a documentation comment.
There are no block comments. Comments are tokens, not discarded — the canonical formatter must
preserve every one ([§8](#8-canonical-form)).

## 2. Identifiers, keywords, reserved words

Identifiers start with a letter or `_`. The convention is enforced by the ecosystem rather than
the lexer: types are `UpperCamel`, functions and bindings are `lower_snake`.

Keywords in use:

```text
fn let var const struct enum match if else while for in return break continue
use uses pub async await move is as true false unit self unsafe
```

Several of those are recognised but guard constructs the shipped language does not include
(`for`, `async`, `await`, `move`, `as`, `self`, `unsafe`) — see
[00 — Overview §3](00-overview.md#3-adopted-but-not-shipped) for the single list.

Reserved for future versions, and an error (`XN0006`) as identifiers today:

```text
trait impl where mod loop defer yield capability effect extern static macro
```

Reserving them now means adding the feature later cannot break existing code.

## 3. Literals

| Form | Rule |
| --- | --- |
| `Int` | Decimal digits, `_` permitted as separator: `1_000_000`. No hex, octal or binary forms. |
| `Float` | Digits required on both sides of the point: `1.0`, never `1.` or `.5`. |
| `String` | `"…"`, single line only. Escapes, closed set: `\n` `\r` `\t` `\0` `\\` `\"` `\'`. |
| `Char` | `'a'` — exactly one Unicode scalar value; `'あ'` is one `Char`. |
| `Bool` | `true`, `false`. |
| `Unit` | `unit` — the only value of type `Unit`. |

Negative numbers are unary `-` applied to a literal. There are no numeric suffixes and no
implicit numeric conversions: `1` and `1.0` are different values of different types
([02 §2](02-types-and-inference.md#2-no-implicit-conversions)).

## 4. Statements end with `;`

`;` terminates statements. A block is an expression whose value is its final expression written
*without* a trailing `;`; adding one makes the block evaluate to `unit` (the Rust rule). This is
the trade recorded in design/0003 §3: a forgotten `;` fails **loudly** as a parse or type error
with a machine-applicable fix, where newline-terminated grammars fail silently.

`return` always takes an operand — `return Ok(unit);`, never bare `return;` — so the
"return with no operand silently yields a zero value" failure class cannot exist.

## 5. Items

A module is a sequence of top-level items: `use`, `fn`, `struct`, `enum`, `const`. Statements
and expressions do not appear at the top level; execution starts at `fn main`
([04 §5](04-evaluation.md#5-entry-and-exit)).

```xenith
pub struct Player {
    name: String,
    var score: Int,
}

enum Rank {
    Bronze,
    Silver,
    Gold,
}

fn rank_of(score: Int) -> Rank uses {} {
    if score >= 1000 { Rank.Gold } else { Rank.Bronze }
}
```

- **`fn`** — every parameter and the return type are written out; there is no whole-program
  inference to fill them in ([02 §3](02-types-and-inference.md#3-inference-is-local)). The
  optional `uses { … }` clause between return type and body declares the effect set
  ([03](03-effects-and-capabilities.md)). Generic parameters take bounds from the sealed
  property set: `fn get<K: Eq + Hash, V>(…)`.
- **`struct`** — named fields, every one annotated. A field is immutable unless declared
  `var name: T`. There are no default values and no field visibility markers; `pub` applies to
  the item as a whole ([05 §4](05-modules-and-projects.md#4-visibility)).
- **`enum`** — variants with optional positional payloads: `Circle(Int)`, `Rect(Int, Int)`,
  `Empty`. Payloads are unnamed by design; constructor calls stay positional (§7).
- **`use`** — a module dependency declaration (`use game.player;`), meaningful only inside a
  project ([05 §3](05-modules-and-projects.md#3-use-declares-a-dependency)).
- **`const`** — parses as an item, but a reference to a `const` name does not resolve in 0.0
  (see [00 §3](00-overview.md#3-adopted-but-not-shipped)). Do not use it yet.

One file may declare any number of items. Top-level names are unique per module — no
overloading, no shadowing between declarations (`XN2005`): when two functions share a name,
every reader has to re-derive which one a call means.

## 6. Statements and expressions

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
```

- **`let` / `var`** — immutable and mutable bindings. Mutability is a different keyword, not a
  modifier, so it cannot be forgotten silently (design/0003 §1). Assignment through a `let` is
  `XN3009`. Local re-`let` of the same name is permitted (only top-level items refuse
  duplicates).
- **Assignment** — `=` plus the compound forms `+=` `-=` `*=` `/=` `%=`. The target's root
  binding must be `var`, and a field written through must itself be a `var` field.
- **Expression statements** — any expression followed by `;`; the value is discarded, whatever
  its type. There is no unused-result diagnostic in 0.0.
- **`while`** — the only loop. The condition must be `Bool`; the body checks against `Unit`.
  `break` and `continue` work as usual. Iteration over a list is written
  `while` + `len()` + `get(index:)`; `for` is not shipped.
- **`if`** — statement or expression. As an expression the `else` is obligatory in practice: an
  `if` without `else` has type `Unit`, so using it as a value reports a type mismatch at the
  use site.
- **`return expr;`** — early return, operand required (§4).

Expressions: literals, paths (`player`, `Rank.Gold`, `game.scores.best`), calls, method calls
(`xs.len()`), field access (`player.score`), unary `-` and `!`, binary operators (§8), `match`
([02 §8](02-types-and-inference.md#8-match-is-checked-for-exhaustiveness)), blocks, `if`,
closures in call-argument position ([06](06-closures.md)), the try operator `expr?`
([04 §4](04-evaluation.md#4-the-try-operator)), and holes `??` / `??name`
([02 §7](02-types-and-inference.md#7-typed-holes)).

## 7. Calls and the named-argument rule

Calls with **two or more arguments must name every argument, in declaration order**:

```xenith,in-fn
let moved = shift(dx: 2, dy: 3);
let replaced = items.replace(index: 0, value: 9);
```

Swapped positional arguments type-check and then misbehave at runtime; named arguments turn
that mistake into `XN3008`, which carries the fix that inserts the missing names. One-argument
calls may stay positional (`describe(rank)` or `describe(rank: rank)`). Enum constructor
payloads are positional always: `Ok(value)`, `ScoreError.NotFound(id)`, `Shape.Rect(3, 4)`.

Named arguments follow declaration order; a name on the wrong position is `XN3003`, with the
declared name attached as a fix.

## 8. Operator precedence

From loosest to tightest; all binary operators are left-associative:

| Level | Operators |
| --- | --- |
| 1 | `\|\|` |
| 2 | `&&` |
| 3 | `==` `!=` `<` `<=` `>` `>=` `is` |
| 4 | `\|` |
| 5 | `^` |
| 6 | `&` |
| 7 | `<<` `>>` |
| 8 | `+` `-` |
| 9 | `*` `/` `%` |
| 10 | unary `-` `!` |
| 11 | postfix: call, method call, field access, `?` |

Bitwise operators bind tighter than comparisons, so `a & b == c` means `(a & b) == c` — the
Rust ordering, not the C gotcha. `<<` and `>>` are recognised only when the two characters are
adjacent, so `Map<String, List<Int>>` closes its generics as expected.

## 9. Patterns

Pattern forms, usable in `match` arms and `let`/`var` positions:

| Form | Example |
| --- | --- |
| binding | `total` |
| wildcard | `_` |
| literal | `0`, `"ok"`, `true` |
| enum path | `Rank.Gold` |
| variant with payload | `Ok(value)`, `Shape.Rect(w, h)` |
| struct | `Player { name, score: s }` — shorthand and renaming both shown |
| alternatives | `Shape.Empty \| Shape.Circle(_)` |
| guard (match arms only) | `n if n > 0` |

In a `match`, a lowercase name that is not a variant is a binding and matches everything —
which is why variant names in patterns are checked against the scrutinee's enum (`XN2006`):
a misspelt variant would otherwise silently become a catch-all.

## 10. Canonical form

`xenith fmt` rewrites source into the one canonical form. **It takes no options** — a
configurable formatter cannot deliver "same meaning, same bytes", and that property is the
point: it removes formatting variance from model output and from diffs (design/0005).

| Rule | Value |
| --- | --- |
| Indent | 4 spaces, never tabs |
| Line width | 100 columns; call arguments wrap beyond it |
| Line endings | LF; file ends with exactly one newline |
| Between top-level items | exactly one blank line |
| Inside blocks | blank lines are **removed** — a deliberate, stronger-than-gofmt rule (design/0005 §3) |
| Multi-line argument lists | trailing comma; single-line lists take none |
| Comments | preserved in position; losing even one aborts formatting |

The formatter verifies its own output before writing: it re-lexes the result and compares the
meaningful token sequence (kind *and* spelling) with the input, and confirms no comment was
lost. If either check fails it refuses and reports a compiler bug rather than write. Source
that does not parse is not formatted, and `format(format(x)) == format(x)` holds by test.
`fmt --check` reports files that would change and exits non-zero, without writing.
