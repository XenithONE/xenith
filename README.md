# Xenith

**An experimental general-purpose language designed so that a compiler can guide an LLM to correct code.**

> ⚠️ **Status: pre-alpha.** Xenith programs parse, type-check and **run** — through a
> deterministic tree-walking interpreter. What exists: the front end, a bidirectional checker
> with checked effects and sealed properties, a module system with projects, closures and four
> `List` combinators, a minimal prelude (`List`/`Map`/`String`), typed holes that answer through
> `xenith goals`, compiler queries, diagnostics that teach, an API-surface dump, execution via
> `xenith run`, and all of it doubled as an MCP server. What does not: concurrency, a broader
> standard library, native code. If you are looking for a language to build software in today,
> this is not it yet.

---

## The thesis

Most languages are designed around what humans find easy to read and write. A growing share of code
is now written by language models, whose failure modes are different from ours.

The obvious move — "design a syntax that is easy for an LLM" — is mostly wrong. An LLM's fluency in a
language is dominated by how much of that language was in its training data, and a new language has
none. You cannot out-train Python with a nicer grammar. Worse, a deliberately novel syntax fights the
model's pretrained priors, so under load it regresses toward Python and Rust habits anyway.

So Xenith bets on a different mechanism:

> **The compiler, not the prompt, is what constrains the model.**

An LLM's dominant failure is not forgetting grammar. It is being made to complete an entire program
while the local intent is still undetermined — so it guesses. Xenith is built to make guessing
unnecessary.

## What that means concretely

**Partial programs are legal.** A hole compiles:

```xenith
fn fetch(client: Client, request: Request) -> Result<Response, HttpError> uses {Net.send} {
    ??response
}
```

The compiler then answers, as machine-readable data, what would fit there. This is real output —
[`examples/scores.xn`](examples/scores.xn) ends in a hole, and this is what the compiler says
about it today:

```console
$ xenith goals examples/scores.xn
examples/scores.xn:59:5 — hole ??lookup in try_find
  expected: Result<Player, ScoreError>
  in scope: name: String
  effects:  none permitted
  candidates:
    1. Err(??)
    2. Ok(??)
    3. try_award(player: ??, points: ??)
```

Candidates are deliberately *scaffolds*, not completions: nested holes mark what still needs
deciding, and named arguments are already spelled out. A model is better served by a partially
correct skeleton with explicit gaps than by a fully formed but irrelevant expression. Functions
that produce the right type but need effects this position does not permit are listed as
`blocked`, with the reason — a model that is not told *why* repeats the mistake. `--json` emits
all of it as data.

The unit of work becomes *fill one hole*, not *rewrite the module*. Compiler output is always
`{ diagnostics[], holes[], suggested_edits[] }`, and every diagnostic carries a stable code, an
`explain` entry, and a machine-applicable fix.

**Signatures cannot lie.** Capabilities are ordinary values, and effects are checked:

```xenith
fn load_config(fs: Fs, path: Path) -> Result<Config, ConfigError> uses {Fs.read} {
    let text = fs.try_read_text(path: path)?;
    Config.try_parse(text: text)
}
```

A function that does not declare an effect cannot perform it. Closures exist and are
capability-effect-free by construction: their bodies are checked under an empty effect set and
they cannot capture a capability ([design/0014](design/0014-closures.md)); `async` and
named-function values remain rejected. Either way a capability cannot hide inside a function
value — the lie is impossible rather than checked.

**Type inference is local.** Public APIs and parameters are fully annotated. There is no
whole-program inference, because non-local inference is repair poison: a model fixes a leaf and the
root type flips underneath it.

**Names are guessable.** Verb prefixes are fixed by the spec (`to_` total conversion, `try_` returns
`Result`, `is_`/`has_` returns `Bool`, `from_` constructs, `with_` returns a modified copy, `get`
returns `Option`). Calls with two or more arguments must use named arguments, which makes
argument-order mistakes a syntax error rather than a silent bug.

**Syntax is deliberately boring.** Rust- and TypeScript-shaped, on purpose. The novelty budget is
spent entirely on the compiler–model protocol, not on punctuation.

## Non-goals

- Not chasing peak runtime performance. Correctness and predictability come first.
- No borrow checker and no lifetime annotations in user code.
- No user-facing syntax macros. (Compiler-owned generators for schemas and bindings are fine, and
  their output is committed and inspectable.)
- No package registry before 1.0 — dependencies are git references.

## How this gets evaluated

The design goal is measurable, so it is measured. `bench/ai/` holds tasks with hidden tests. The same
model attempts each task under three conditions:

| condition | what the model gets |
| --- | --- |
| `full-pack` | the whole field guide in context |
| `retrieved` | only task-relevant types, signatures and verified examples |
| `hole-guided` | compiler holes and queries during construction |

Reported metrics are **pass@1** and **mean fix-iterations-to-green**. A language change is judged by
whether those numbers move. If `hole-guided` does not beat the others, the central premise of this
project is wrong and the design gets revisited.

Benchmarks run locally against subscription CLIs rather than metered APIs, so results are committed
to the repository rather than produced in CI.

## Design record

Design decisions and the reasoning behind them — including reversals — live in [`design/`](design/).

- [`0001-why-xenith.md`](design/0001-why-xenith.md) — goals, non-goals, and the original draft
- [`0002-design-review.md`](design/0002-design-review.md) — an external review by four models that
  overturned the original north star, with the counterexample that disproved the first purity rule
- `0003`–`0014` — the semantic kernel, the concurrency decision, the minimal std, diagnostics
  that teach, the module system, two measurement RFCs and their verdicts, project truth, and
  closures — one adopted file per decision, review wreckage recorded in each

The specification draft is in [`spec/`](spec/).

## Building

Requires a recent stable Rust toolchain.

```bash
cargo build --manifest-path compiler/Cargo.toml
```

## What works today

```bash
cargo run --manifest-path compiler/Cargo.toml -p xenith -- check examples/scores.xn
```

`check` parses **and type-checks**: local-only inference, checked effect sets, sealed property
bounds, move-free mutability rules. `--json` emits diagnostics as data, with byte spans and
applicable fixes — an undeclared effect, for instance, carries the edit that would amend the
`uses` clause. `goals` reports every hole as shown above. `explain XN4001` prints the rule behind
a code. `fmt` rewrites source into canonical form — it takes no options, and it verifies its own
output before writing, refusing rather than risking a silent change of meaning.

The compiler also answers direct questions. `query type-at` reports the type at any position —
expression, binding name, or hole — with the scope and effect budget around it. `query producers`
is the anti-hallucination query: instead of guessing a function name, ask what can make the type
you need.

```console
$ xenith query producers examples/scores.xn "Result<Player, ScoreError>"
producers of Result<Player, ScoreError>:
  function  try_award(player: Player, points: Int) -> Result<Player, ScoreError>
  function  try_find(name: String) -> Result<Player, ScoreError>
  method    List<T>.fold(init: Result<Player, ScoreError>, f: fn(Result<Player, ScoreError>, T) -> Result<Player, ScoreError>) -> Result<Player, ScoreError>
  method    Option<Player>.to_result(error: ScoreError) -> Result<Player, ScoreError>
  variant   Err(ScoreError)
  variant   Ok(Player)
```

Both take `--json`. Partial programs answer like any other — a query is a hole the author did not
have to write, and it rides the same traversal that answers `goals`.

Programs run:

```console
$ xenith run examples/hello.xn
Hello, world
```

Execution is a tree-walking interpreter, deliberately: peak performance is a non-goal, and what
the benchmark needs is execution that is correct and deterministic — strict left-to-right
evaluation, trapping overflow and division by zero, IEEE floats, no undefined behaviour. A file
with diagnostics is refused; a file with **holes** runs, and reaching one is a precise trap that
names the hole and points at `goals`. Running a partial program tells you which hole to fill
next — that is the workflow, not a failure mode.

## Using from an agent (MCP)

Everything above is also an MCP server: `check`, `goals`, `type_at`, `producers`, `fmt`,
`explain` and `run` as tools over stdio, speaking the same JSON as the CLI — one wire format,
defined once in `xenith-driver`. To connect it to Claude Code:

```bash
claude mcp add xenith -- cargo run -q --manifest-path compiler/Cargo.toml -p xenith-mcp
```

(The first call pays for a build; point the command at a compiled `xenith-mcp` binary to skip
that.) The tool descriptions carry the usage rules — a model that has only read the tool list
knows that holes are legal, that `producers` replaces guessing a function name, and that an
unknown type is an error rather than an empty result.

Diagnostics look like this, with the caret at the position the fix edits:

```console
$ xenith check broken.xn
broken.xn:2:14: error[XN1002]: expected `;`
2 |     let a = 1
  |              ^
  fix: insert `;`
  run `xenith explain XN1002` for the rule
```

## License

Dual-licensed under [Apache-2.0](LICENSE-APACHE) or [MIT](LICENSE-MIT), at your option.
