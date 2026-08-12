# 07 — The prelude

*Draft. Checked against `xenith 0.0.0`; design/0007 fixed this surface, design/0014 §4 added
the four combinators. The prelude is deliberately minimal — it exists to make the benchmark
tasks writable, and it does not grow for convenience.*

Everything here is available without any `use`. In projects, the root namespace `std` is
reserved for the day this surface becomes real modules ([05 §1](05-modules-and-projects.md#1-a-file-is-a-module)).

## 1. Types and construction

| Type | Construction | Notes |
| --- | --- | --- |
| `Int` | literals | 64-bit signed, trapping ([04 §3](04-evaluation.md#3-arithmetic-traps)) |
| `Float` | literals | IEEE 754 double; `Eq` but not `Ord`/`Hash`; **no methods** |
| `Bool` | `true` / `false` | no methods |
| `String` | literals | a sequence of Unicode scalar values (D2) |
| `Char` | literals | one Unicode scalar; no methods |
| `Unit` | `unit` | |
| `List<T>` | `[1, 2, 3]`; empty `[]` needs an expected type | |
| `Map<K, V>` | `empty_map()` — the one prelude free function | keys need `Eq + Hash`, so `Float` cannot key |
| `Option<T>` | `Some(v)` / `None`, unqualified | |
| `Result<T, E>` | `Ok(v)` / `Err(e)`, unqualified | |
| `Io` | never constructed — arrives as `main`'s parameter | the capability ([03](03-effects-and-capabilities.md)) |
| `Error` | produced by the runtime (`Io.write`, `try_to_int`) | opaque; carries a debug message nothing reads back |

`empty_map` is `fn empty_map<K: Eq + Hash, V>() -> Map<K, V>`; its type arguments come from
the expected type — `var m: Map<String, Int> = empty_map();` — or fail closed with
"annotation required" ([02 §3](02-types-and-inference.md#3-inference-is-local)).

`Some` / `None` / `Ok` / `Err` are the **only** variants usable without their enum's name.

## 2. Mutation discipline

Reads from containers return **copies** of the stored value (D1,
[04 §1](04-evaluation.md#1-values-are-values)). Exactly five methods write through their
receiver in place — `List.push`, `List.pop`, `List.replace`, `Map.insert`, `Map.remove` — and
they demand the same thing assignment demands: the receiver must be a mutable place (`var`
binding, `var` field owned by this module). Everything else returns a new value and leaves
the receiver untouched.

## 3. The method surface

This is the **entire** built-in method surface of 0.0. Methods are provided by the language;
user types have no methods — their operations are their module's `pub` functions
([05 §4](05-modules-and-projects.md#4-visibility)).

### Int

| Method | Signature | Notes |
| --- | --- | --- |
| `checked_add` | `(other: Int) -> Option<Int>` | `None` where `+` would trap |
| `to_text` | `() -> String` | |

### String

| Method | Signature | Notes |
| --- | --- | --- |
| `len` | `() -> Int` | Unicode scalar count — this meaning is permanent (D2) |
| `concat` | `(other: String) -> String` | |
| `split` | `(sep: String) -> List<String>` | lossless: `parts.join(sep:)` restores the input; consecutive separators yield empty strings; `sep: ""` splits into scalars |
| `trim` | `() -> String` | strips ASCII whitespace only (space, tab, CR, LF) |
| `try_to_int` | `() -> Result<Int, Error>` | accepts ASCII whitespace, then `[+-]?[0-9]+`; digit separators, decimal points and out-of-`Int`-range values are `Err`, never a trap |
| `starts_with` | `(prefix: String) -> Bool` | |
| `contains` | `(sub: String) -> Bool` | |

### List\<T\>

| Method | Signature | Notes |
| --- | --- | --- |
| `len` | `() -> Int` | |
| `is_empty` | `() -> Bool` | |
| `push` | `(item: T) -> Unit` | in place, appends; `var` receiver |
| `pop` | `() -> Option<T>` | in place, removes last; `None` when empty |
| `get` | `(index: Int) -> Option<T>` | negative or out-of-range is `None` — **there is no panicking index operator** |
| `replace` | `(index: Int, value: T) -> Option<T>` | returns the old value; out of range: `None`, list unchanged |
| `contains` | `(item: T) -> Bool` | requires `T: Eq` |
| `sorted` | `() -> List<T>` | requires `T: Ord` (so never `Float`); new list, stable |
| `concat` | `(other: List<T>) -> List<T>` | new list |
| `join` | `(sep: String) -> String` | `T: Text`, which is total today — elements render as a literal would write them |
| `map` | `(f: fn(T) -> U) -> List<U>` | |
| `filter` | `(f: fn(T) -> Bool) -> List<T>` | |
| `fold` | `(init: B, f: fn(B, T) -> B) -> B` | left fold; two arguments, so names are mandatory: `xs.fold(init: 0, f: \|acc, x\| acc + x)` |
| `find` | `(f: fn(T) -> Bool) -> Option<T>` | short-circuits at the first hit |

The four combinators all traverse left to right and return new values; the closure they take
is capability-effect-zero by construction ([06](06-closures.md)).

### Map\<K: Eq + Hash, V\>

| Method | Signature | Notes |
| --- | --- | --- |
| `len` | `() -> Int` | |
| `is_empty` | `() -> Bool` | |
| `insert` | `(key: K, value: V) -> Option<V>` | returns the old value; `var` receiver |
| `get` | `(key: K) -> Option<V>` | value copy (D1) |
| `remove` | `(key: K) -> Option<V>` | `var` receiver |
| `has_key` | `(key: K) -> Bool` | "key containment" is named apart from value/substring `contains` on purpose |
| `keys` | `() -> List<K>` | an insertion-order **snapshot** — later map mutation does not reach into it |

### Option\<T\>

| Method | Signature | Notes |
| --- | --- | --- |
| `to_result` | `(error: E) -> Result<T, E>` | `Some(v)` → `Ok(v)`, `None` → `Err(error)` |

### Io

| Method | Signature | Notes |
| --- | --- | --- |
| `write` | `(text: String) -> Result<Unit, Error>` | effect `Io.write`; writes exactly `text`, no newline added |

## 4. Map order is normative

Iteration order of a `Map` is **insertion order**, as specification, not accident
(design/0007 §3):

- `insert` on an existing key keeps its position and replaces only the value (the stored key
  is not replaced either).
- `remove` preserves the order of the survivors (shift, not swap).
- Re-inserting a removed key appends at the end.
- `keys()` is therefore always deterministic.
- `==` on maps compares key-value correspondence and **ignores order** — display order and
  equality are different questions.

```xenith
fn main(io: Io) -> Result<Unit, Error> uses {Io.write} {
    var counts: Map<String, Int> = empty_map();
    counts.insert(key: "ash", value: 1);
    counts.insert(key: "elm", value: 2);
    counts.insert(key: "oak", value: 3);
    counts.insert(key: "ash", value: 9);
    counts.remove(key: "elm");
    counts.insert(key: "elm", value: 4);
    io.write(text: counts.keys().join(sep: ","))?;
    return Ok(unit);
}
```

```console
$ xenith run order.xn
ash,oak,elm
```

## 5. What deliberately does not exist

No `format!` and no string interpolation — build strings with `concat` and `to_text`. No
`chars()` (`split(sep: "")` is the spelling). No `values()` on `Map` (`keys` + `get` derives
it). No byte-length or grapheme-cluster APIs on `String` (if ever needed they arrive under
new names; `len` keeps meaning scalar count). No panicking index operator. No `sorted_by`
yet — it needs function-typed parameters in user-facing signatures and an `Ordering` design,
so aggregates currently have no sort spelling at all
([00 §3](00-overview.md#3-adopted-but-not-shipped)).

## 6. Naming rules

Fixed by the language so that a correct name is guessable rather than memorised:

- `to_` — total conversion. `try_` — returns `Result`. `is_` / `has_` — returns `Bool`.
  `from_` — constructor. `with_` — returns a modified copy. `get` / `checked_` — returns
  `Option`.
- Bare verbs are a **closed allowlist**: `len` `push` `pop` `get` `insert` `remove` `replace`
  `split` `trim` `sorted` `keys` `concat` `join`, plus the conventional exceptions
  `contains` / `starts_with` / `has_key`. New bare verbs are not invented.
- Types are `UpperCamel`; functions and bindings are `lower_snake`.
