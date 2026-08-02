### `List<T>`

| Method | Signature |
| --- | --- |
| `len` | `() -> Int` |
| `is_empty` | `() -> Bool` |
| `push` | `(item: T) -> Unit` — requires a `var` binding; appends in place at the end |
| `pop` | `() -> Option<T>` — removes the last element; `None` when empty |
| `get` | `(index: Int) -> Option<T>` — negative or out-of-range index gives `None`; returns a copy of the value |
| `replace` | `(index: Int, value: T) -> Option<T>` — returns the old value; out-of-range gives `None` and the list is unchanged |
| `contains` | `(item: T) -> Bool` — requires `T: Eq` |
| `sorted` | `() -> List<T>` — requires `T: Ord`; returns a new, stably sorted `List`; `Float` is rejected at the bound |
| `concat` | `(other: List<T>) -> List<T>` — returns a new `List` |
| `join` | `(sep: String) -> String` — requires `T: Text` |

Construction: `[1, 2, 3]`; the empty literal `[]` requires an expected type.

### `Map<K: Eq + Hash, V>`

| Method | Signature |
| --- | --- |
| `len` | `() -> Int` |
| `is_empty` | `() -> Bool` |
| `insert` | `(key: K, value: V) -> Option<V>` — returns the old value; requires a `var` binding |
| `get` | `(key: K) -> Option<V>` — returns a copy of the value |
| `remove` | `(key: K) -> Option<V>` |
| `has_key` | `(key: K) -> Bool` |
| `keys` | `() -> List<K>` — a snapshot `List` in insertion order |

Construction is the generic free function `empty_map`, with signature
`fn empty_map<K: Eq + Hash, V>() -> Map<K, V>`; the type arguments come from the expected
type, so write `let m: Map<String, Int> = empty_map();`. Insertion order is part of the
specification: `insert` on an existing key keeps the key's position and replaces only the
value, never the stored key; `remove` preserves the order of the remaining keys; re-inserting
a removed key places it at the end; the order of `keys()` follows from these rules and is
always deterministic. `==` on Maps ignores order — it compares key-value correspondence.
`Float` has no `Hash` and cannot be a key.

### String additions

| Method | Signature |
| --- | --- |
| `len` | `() -> Int` — the number of Unicode scalar values |
| `split` | `(sep: String) -> List<String>` |
| `trim` | `() -> String` |
| `try_to_int` | `() -> Result<Int, Error>` |
| `starts_with` | `(prefix: String) -> Bool` |
| `contains` | `(sub: String) -> Bool` |

`split` is lossless: joining the pieces with the same separator always restores the original
string; consecutive separators keep empty strings, and `sep = ""` splits into one piece per
scalar value. `trim` removes ASCII whitespace only (space, tab, CR, LF). `try_to_int` accepts
any amount of ASCII whitespace around `[+-]?[0-9]+`; digit separators and decimal points are
`Err`, and a value outside `Int` range is `Err`, never a trap.
