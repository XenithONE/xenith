# 08 — Diagnostics and tooling

*Draft. Checked against `xenith 0.0.0`; design/0009 (teaching), design/0012 §1 (the
module-call bridge) and design/0013 (one project pipeline) are the decision records.*

Compiler output is a **protocol, not prose**: tools and models consume the structured form
directly, and the terminal rendering is a view over it — never the other way around.

## 1. Anatomy of a diagnostic

Every diagnostic carries:

| Field | Contents |
| --- | --- |
| `code` | a stable identifier, `XN` + four digits (§2) |
| `severity` | `error` or `warning` |
| `span` | half-open byte range `[start, end)` into the file — authoritative; `line` / `column` (one-based, character-counting) are added on the wire so consumers need not recompute them |
| `message` | one sentence naming the problem in terms of the source |
| `fix` | optional: a description plus text edits (`span` + `replacement`; empty span inserts, empty replacement deletes). Attached **only when unambiguously correct** — a merely plausible fix teaches blind application, which is worse than nothing |
| `teaches` | optional knowledge blocks (§4); absent from the wire when empty |

Rendered form, caret at the byte the fix would edit:

```console
$ xenith check broken.xn
broken.xn:2:14: error[XN1002]: expected `;`
2 |     let a = 1
  |              ^
  fix: insert `;`
  run `xenith explain XN1002` for the rule
```

## 2. Code families

**Codes are never reused or renumbered** — a model that learned what `XN0002` means must keep
being right. Numbering has gaps for the same reason; 0.0 ships 53 codes across these families:

| Range | Area |
| --- | --- |
| `XN0xxx` | lexical |
| `XN1xxx` | syntax — including the closure form rules (`XN1009`–`XN1012`) and `XN1008`, the "parsed but not shipped" refusal |
| `XN2xxx` | name resolution — unknown names/types/methods/modules, visibility, `use` hygiene |
| `XN3xxx` | types — mismatches, argument shape, annotation-required, sealed properties, value-sized recursion |
| `XN4xxx` | capabilities and effects — `XN4001` effect-not-permitted; `XN4005`–`XN4008` the closure pillars |
| `XN5xxx` | exhaustiveness |
| `XN6xxx` | concurrency (`Transfer` / `ShareSafe`) — **reserved, no codes shipped** |
| `XN7xxx` | modules and project layout |

The registry is executable, not documentary: `xenith explain` with no argument lists every
code with its first line; `xenith explain XN4001` (case-insensitive) prints the full rule —
written for a reader with the error in front of them, stating the rule and how to satisfy it.

## 3. The wire contract

All machine output shares one definition (`xenith-driver`), spoken identically by the CLI's
`--json` and the MCP server.

- **`schema_version: 1`** on every object a consumer holds — array responses version each
  entry, because the CLI flattens arrays across files. The version bumps only for
  incompatible shape changes, which are to be about as rare as renumbering a code.
- **Tolerant-reader contract**: consumers must ignore fields they do not recognise. New
  optional fields (`teaches`, `features`, `analysis_mode`) arrive under the same version,
  because a skippable addition is not an incompatible change.
- **`features`** names the additive capabilities present —
  `["diagnostic_teaching_v1", "module_call_teach_v1", "project_mode_v1"]` — so a consumer can
  tell "an old compiler without teaching" from "teaching supported, nothing taught here". It
  is an advertisement, not a proof: the mode a response actually ran under is its
  `analysis_mode` field (design/0013 §1).
- Key order is deterministic (sorted), so byte-level comparison of responses is meaningful.

A `check --json` response, teaching on:

```console
$ xenith check --json measure.xn
[
  {
    "diagnostics": [
      {
        "code": "XN2003",
        "column": 8,
        "line": 2,
        "message": "`List<Int>` has no method named `size`",
        "severity": "error",
        "span": { "end": 46, "start": 42 },
        "teaches": [
          {
            "items": [
              { "name": "len", "signature": "len() -> Int" },
              { "name": "is_empty", "signature": "is_empty() -> Bool" },
              { "name": "push", "signature": "push(item: Int) -> Unit" },
              { "name": "pop", "signature": "pop() -> Option<Int>" },
              { "name": "get", "signature": "get(index: Int) -> Option<Int>" },
              { "name": "replace", "signature": "replace(index: Int, value: Int) -> Option<Int>" }
            ],
            "kind": "available_methods",
            "total_items": 14,
            "truncated": true,
            "type": "List<Int>"
          }
        ]
      }
    ],
    "features": ["diagnostic_teaching_v1", "module_call_teach_v1", "project_mode_v1"],
    "file": "measure.xn",
    "schema_version": 1
  }
]
```

(Whitespace above is compacted for the page; the tool emits standard pretty-printed JSON with
the same keys and values.)

## 4. Teaching

Diagnostics **attach knowledge at the point of failure** (design/0009): the signatures a
repair needs, delivered structurally — never as pre-rendered strings — in the diagnostic
itself, because a separate query channel measurably goes unused. Each kind exists for a
measured failure family, not helpfulness in general. Four kinds ship:

| `kind` | Fires on | Carries |
| --- | --- | --- |
| `call_signature` | the first argument-shape diagnostic of a call (`XN3002` / `XN3003` / `XN3008`) | the resolved callee's full signature, generics already bound — the callee is known, so precision is total |
| `available_methods` | unknown method (`XN2003`) | the receiver type's method catalogue: declaration order, at most 6 items, `total_items` and `truncated` making the cut explicit |
| `use_candidates` | unknown name (`XN2002`) whose exact spelling is `pub` in more than one module | the candidate `use` lines in canonical path order — listed, **never auto-picked**. (When exactly one module exports it, there is no teach: the diagnostic carries the machine fix that inserts the `use`.) |
| `module_call` | method-call spelling on a module-owned type (`XN2003`) | the owning module's `pub` functions taking the receiver type as an **input** (return-only producers are excluded on purpose), plus the rewrite bridge |

The module-call teach in full, because it is the richest shape:

```console
$ xenith check src/main.xn
src/main.xn:5:23: error[XN2003]: `depot.locker.Locker` has no method named `stow`; module functions are called as `depot.locker.stow(...)`
  module functions taking depot.locker.Locker:
      depot.locker.stow(locker: depot.locker.Locker, load: Int) -> depot.locker.Locker
      rewrite: depot.locker.stow(locker: <receiver>, load: ...)
      depot.locker.load_of(locker: depot.locker.Locker) -> Int
      rewrite: depot.locker.load_of(locker: <receiver>)
5 |     let full = locker.stow(load: 12);
  |                       ^^^^
```

On the wire each item may carry `receiver_parameter` and `rewrite`; both are omitted when
more than one input position fits — naming one of several would be mis-guidance
(design/0012 §1). Candidates whose function name equals the unknown member name are always
kept inside the budget.

Budget rules, so attention is spent and not flooded: at most **6 items** per block; a
signature is cut at **200 bytes** on a character boundary — except `module_call`, which
includes or omits whole candidates and never cuts mid-signature; one catalogue per type per
run (deduplication); teach structure and order are deterministic. Rendering places the block
directly after the primary message, because trailing notes measurably fall into the same
ignore-path as external hints.

Some teaching rides as a sentence inside the message itself (the module-call sentence above;
the closure sentences of `XN4005`/`XN4006`/`XN1010`/`XN1012`). Those suffixes are tracked, so
the off switch can remove exactly them.

## 5. The off switch

`check` and `run` take `--diagnostic-teaching=off` (default `on`). The contract is **byte
identity**: off-mode output equals on-mode output minus the teach blocks and teach-note
sentences — nothing else moves. On the wire, `teaches` disappears, message suffixes are
stripped, and `features` is absent, reproducing the pre-teaching shape exactly. The guarantee
exists for measurement (design/0009's experiments difference exactly this) and is held by
test.

The MCP surface has no teaching flag: a tool consumer always receives the taught shape and
may ignore what it does not read.

## 6. The CLI

| Command | Does | Exit |
| --- | --- | --- |
| `xenith check <paths…> [--json] [--diagnostic-teaching=on\|off]` | parse + full analysis; project-aware ([05 §5](05-modules-and-projects.md#5-checking-and-running-a-project)) | 0 clean; 1 on any error |
| `xenith run <path> [--diagnostic-teaching=…]` | check, then execute `fn main` | 0 / 1 / 2 / 101 ([04 §5](04-evaluation.md#5-entry-and-exit)) |
| `xenith goals <paths…> [--json]` | every hole: expected type, scope, effect budget, candidates, blocked symbols | 0 unless a file was unreadable |
| `xenith query type-at <path> --at L:C [--json]` | the type at a position, with scope and effect budget — a query is a hole the author did not have to write, riding the same traversal | |
| `xenith query producers <path> "<Type>" [--json]` | everything in scope that can produce the type: functions, methods, variants. The anti-hallucination query — ask, instead of guessing a name. An unknown type is an error, not an empty list | |
| `xenith fmt <paths…> [--check]` | canonical form ([01 §10](01-lexical-and-syntax.md#10-canonical-form)); `--check` lists files that would change, exit 1 | |
| `xenith explain [CODE]` | the rule behind a code; no argument lists all | |
| `xenith api <project> [--module M] [--json]` | the public API surface ([05 §6](05-modules-and-projects.md#6-the-api-surface)) | |

`goals` and `query` analyse single files in 0.0 ([05 §5](05-modules-and-projects.md#5-checking-and-running-a-project));
`type-at` positions are one-based and count characters, and answer for partial programs like
any others.

```console
$ xenith query producers scores.xn "Result<Player, ScoreError>"
producers of Result<Player, ScoreError>:
  function  try_award(player: Player, points: Int) -> Result<Player, ScoreError>
  function  try_find(name: String) -> Result<Player, ScoreError>
  method    List<T>.fold(init: Result<Player, ScoreError>, f: fn(Result<Player, ScoreError>, T) -> Result<Player, ScoreError>) -> Result<Player, ScoreError>
  method    Option<Player>.to_result(error: ScoreError) -> Result<Player, ScoreError>
  variant   Err(ScoreError)
  variant   Ok(Player)
```

## 7. The MCP server

`xenith-mcp` exposes the same operations as tools over MCP's stdio transport, speaking the
same JSON — one wire format, defined once. Tools: `check`, `goals`, `type_at`, `producers`,
`fmt`, `explain`, `run`, and — only behind the startup flag `--experimental-api-surface`,
because its shape is unstable — `api_surface`.

- **Workspace confinement**: every path-taking tool resolves against the server's workspace
  root (`--workspace-root`, default: startup working directory) and refuses paths outside it,
  after canonicalising both sides.
- **Modes** (design/0013 §1): `mode: auto | project | single_file`, default `auto` — project
  analysis when a manifest governs the file, single-file otherwise. Discovery failures (broken
  manifest, containment violation, invalid layout) are **explicit errors, never a silent
  single-file fallback**, and every response carries the `analysis_mode` that actually ran
  (plus `project_root` in project mode).
- Project `check` reports every file — the requested file's diagnostics first, the rest in
  path-lexicographic order: cascade mitigation by priority, never truncation.
- The tool descriptions carry the usage rules (holes are legal; `producers` replaces guessing;
  an unknown type is an error), so a model that has only read the tool list already knows the
  workflow.
