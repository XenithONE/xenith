# 05 — Modules and projects

*Draft. Checked against `xenith 0.0.0`; design/0010 is the decision record, design/0013 the
project-analysis contract.*

## 1. A file is a module

A **project** is a directory tree whose root holds the manifest `xenith.toml`. The manifest is
a root marker; its `name` field is optional and unused by resolution (reserved for future
package identity). Every `.xn` file under `src/` is one module, named by its path:
`src/game/player.xn` is the module `game.player`. There is no `mod` declaration and no
`mod.xn` — the filesystem is the single source of truth.

A lone `.xn` file outside any project needs no manifest and behaves as before modules existed.

Layout rules, each with its own diagnostic so the failure is nameable:

| Rule | Code |
| --- | --- |
| Path segments are `lower_snake` identifiers (`player.xn`, not `Player-v2.xn`); symlinks rejected | `XN7001` |
| Two module paths may not differ only by letter case — rejected on **every** host, so Windows and Linux build the same program | `XN7002` |
| A module path and a top-level item of its parent are exclusive: `game.xn` declaring `player` while `game/player.xn` exists would give `game.player` two readings | `XN7003` |
| `fn main` lives in `src/main.xn` and nowhere else | `XN7004` |
| The root name `std` is reserved for the language's own future modules; the prelude needs no `use` | `XN7005` |
| One project, one manifest — no `xenith.toml` nested inside another's sources | `XN7006` |

## 2. The smallest project

```text
depot/
├── xenith.toml
└── src/
    ├── main.xn
    └── depot/
        └── locker.xn
```

```xenith
// src/depot/locker.xn
pub struct Locker {
    label: String,
    var load: Int,
}

pub fn new_locker() -> Locker {
    Locker { label: "a", load: 0 }
}

pub fn stow(locker: Locker, load: Int) -> Locker {
    var updated = locker;
    updated.load = updated.load + load;
    updated
}

pub fn load_of(locker: Locker) -> Int {
    locker.load
}
```

```xenith
// src/main.xn
use depot.locker;

fn main(io: Io) -> Result<Unit, Error> uses {Io.write} {
    let locker = depot.locker.new_locker();
    let full = depot.locker.stow(locker: locker, load: 12);
    io.write(text: depot.locker.load_of(locker: full).to_text())?;
    return Ok(unit);
}
```

```console
$ xenith run src/main.xn
12
```

## 3. `use` declares a dependency

`use <module-path>;` is the **only** form: no item `use`, no glob, no `as` alias. It does not
import names — it declares that this file depends on that module and licenses fully qualified
references to its items (`depot.locker.Locker`). Full qualification makes same-name items in
different modules collide **nowhere**, which is what made aliases unnecessary (design/0010 §1);
every entity has exactly one spelling, so diagnostics, `goals` and search share a canonical
form.

- A qualified reference to a module this file has not `use`d is `XN2007`, and the message
  names the exact `use` line to add. A **bare** name that is `pub` in exactly one module is
  `XN2002` carrying the machine fix that inserts the `use`; when several modules export it,
  they are listed and none is auto-picked ([08 §4](08-diagnostics-and-tooling.md#4-teaching)).
- `use` of a module the project does not contain is also `XN2007`.
- **An unused `use` is a hard error**, not a lint (`XN2009`): the `use` list is the file's
  exact dependency list, and "use everything, then guess" must never become a strategy.
- The same module twice is `XN2010`. Canonical form keeps `use`s at the top, dictionary-sorted,
  deduplicated — which is also where the machine fix inserts.

Import **cycles are permitted**. Xenith has no module initialisation statements, so mutual
reference between declarations has no execution-order problem; only semantic cycles are
refused — a type that contains itself by value (`XN3011`) names its cycle and any of
`Option` / `List` / `Map` breaks it.

## 4. Visibility

- Top-level items are **private to their module** by default. There is no parent or child
  privilege — a module's private items are equally private from everywhere else.
- `pub` exposes an item. `main` needs no `pub`.
- A cross-module reference to a private item is `XN2008`. **No fix is attached, on purpose**:
  making an item `pub` is an API decision for the owning module, not a local syntax repair.
- **Public API closure**: a `pub fn`'s signature, a `pub struct`'s field types and a
  `pub enum`'s payload types may not mention a private type (`XN7007`) — callers must be able
  to spell every value they are handed.
- A `pub struct` can be **constructed, read and pattern-matched** from other modules, but its
  fields cannot be **assigned** across the boundary — `var` field or not (`XN7008`). Invariants
  live with the owning module; mutation goes through its `pub` functions. One rule, no new
  syntax, and the strict side is the backward-compatible side to start from.
- A `pub enum` is fully open: every variant constructs and matches from outside.

```console
$ xenith check src/main.xn
src/main.xn:5:12: error[XN7008]: field `load` of `depot.locker.Locker` cannot be assigned from outside `depot.locker`
```

## 5. Checking and running a project

Any project-aware command handed **any path inside the project** analyses the whole project
(the manifest is discovered upward; there is no silent single-file fallback — a broken
manifest or layout is an explicit error, design/0013 §1). Checking is two-phase — all
declaration headers are indexed, then bodies are checked, in dictionary order of module path —
which is what makes import cycles harmless and diagnostics deterministically ordered.

`run` on any file in the project runs the project, entering `src/main.xn`; one diagnostic
anywhere refuses the run ([04 §5](04-evaluation.md#5-entry-and-exit)).

**Every project-aware command, on both surfaces:** `check`, `run`, `api`, `goals` and
`query type-at` / `query producers` all resolve a path through the one pipeline, so the CLI
and the MCP server answer the same question the same way (design/0013 §1). Inside a project,
`goals` reports the whole project's holes — the named file's first, the rest in path order —
and both queries answer with every module's declarations in view, so a cross-module type
renders qualified instead of reading as unknown. The MCP tools additionally take an explicit
`mode` ([08 §7](08-diagnostics-and-tooling.md#7-the-mcp-server)); the CLI, which has no such
flag, always asks for `auto`.

One difference remains, and it follows from that missing flag. A project whose **layout** is
invalid is a discovery failure: the MCP tools refuse it and point at `mode: "single_file"`.
The CLI has no single-file escape hatch to point at, so it states each layout problem on
stderr and answers from the modules that did load — degraded, and never silently so.

## 6. The API surface

`xenith api <project>` prints the reachable public API — per module, the `pub` functions with
effect sets, `pub` structs and enums — in deterministic order; `--module game.player` scopes
to one subtree, `--json` emits the machine form, which carries its own
`api_schema_version: 1` (a separate series from the diagnostics wire version,
design/0013 §2).

```console
$ xenith api .
module depot.locker

pub struct Locker {
    label: String,
    var load: Int,
}

pub fn load_of(locker: Locker) -> Int

pub fn new_locker() -> Locker

pub fn stow(locker: Locker, load: Int) -> Locker

module main

(no public items)
```

An API map answers "what may callers write" — it is deliberately **not** wiring knowledge: it
does not place `use` lines or connect modules, and measurement bore that out (design/0013:
strongest document for implementation tasks, useless for wiring).

## 7. Not in the module system

By decision, not omission (design/0010 §8): item `use`, glob `use`, `as` aliases, re-export,
field-level visibility syntax, parent/child visibility privileges, conditional compilation,
test privileges (tests use the same file or the `pub` API), package-to-package dependencies
and version resolution.
