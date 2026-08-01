# Xenith

**An experimental general-purpose language designed so that a compiler can guide an LLM to correct code.**

> ⚠️ **Status: pre-alpha.** Xenith programs do not run. There is no standard library, no type
> checker and no backend. What exists is the front end — the compiler lexes, parses and formats,
> and the `xenith` command has `check`, `fmt` and `explain`. If you are looking for a language you
> can use today, this is not it.

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
let response: Result<Response, HttpError> = ?response
```

The compiler then answers, as machine-readable data, what would fit there:

```console
$ xenith goals --json
{
  "hole": "response",
  "expected": "Result<Response, HttpError>",
  "in_scope": ["client", "request"],
  "allowed_effects": ["Net.send"],
  "candidates": ["client.try_send(request: request)", "Ok(Response.empty())"]
}
```

The unit of work becomes *fill one hole*, not *rewrite the module*. Compiler output is always
`{ diagnostics[], holes[], suggested_edits[] }`, and every diagnostic carries a stable code, an
`explain` entry, and a machine-applicable fix.

**Signatures cannot lie.** Capabilities are ordinary values, and effects are checked:

```xenith
fn load_config(fs: Fs, path: Path) -> Result<Config, ConfigError> uses {Fs.read} {
    let text = fs.try_read_text(path: path)?
    Config.try_parse(text: text)
}
```

A function that does not declare an effect cannot perform it — including through a captured closure.

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

`check` parses and reports problems; `--json` emits them as data, with byte spans and applicable
fixes. `explain XN1002` prints the rule behind a code. `fmt` rewrites source into canonical form —
it takes no options, and it verifies its own output before writing, refusing rather than risking a
silent change of meaning.

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
