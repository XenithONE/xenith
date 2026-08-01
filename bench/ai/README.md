# AI benchmark

Xenith's design goal is that a model writes correct code on the first try. That is measurable, so
it gets measured here. This directory is the project's objective function: a language change is
judged by whether these numbers move.

## Conditions

Each task is attempted under conditions that isolate one question each; the interesting result is
the *comparison*, not any single number.

| Condition | What the model gets |
| --- | --- |
| `bare` | Nothing but the language's name. The lower control: how far does Rust/TS transfer alone go? |
| `full-pack` | [`field-guide.md`](field-guide.md) in context. Measures the context-pack thesis. |
| `hole-guided` | The guide, plus the compiler protocol: every round feeds back diagnostics **and `xenith goals`**, and the model is invited to leave `??holes` deliberately. Measures the central premise. |

The project's premise is `hole-guided` > `full-pack` > `bare`. **If that ordering does not hold,
the premise is wrong** and the design gets revisited — see
[design/0002 §10](../../design/0002-design-review.md).

> **Deviation from 0002 §10, recorded:** the middle condition there was `retrieved` —
> task-relevant excerpts selected per task. With no standard library yet there is nothing real to
> retrieve from, and a hand-simulated retrieval would measure the simulation. `bare` replaces it
> as the lower control until retrieval is a real mechanism. The original three-way comparison
> returns when `std/` exists.

## Metrics

- **pass@1** — first attempt passes the hidden expectation
- **rounds-to-green** — attempts until passing (cap 4)
- **outcome classes per round** — `diagnostics` / `runtime failure` / `wrong output` / `pass`,
  so the failure *mode* is visible, not just the rate

## Tasks

`tasks/*.toml` — ten tasks across three tiers, each carrying a prompt, the expected stdout, and a
**reference solution**. References are not decoration:

```bash
cargo run --manifest-path compiler/Cargo.toml -p xenith-bench -- verify
```

runs every reference through `xenith check` and `xenith run` and compares output. **This runs in
CI.** A task whose reference fails is a broken task, and measuring models against broken tasks
produces numbers that lie.

Tasks deliberately stay within the provisional prelude (no lists, no maps — nothing constructs
them yet) and cover: loops and arithmetic, string building, struct field mutation, enums with
payloads and guards, `checked_add`/`to_result`/`?` plumbing, effect declarations, recursion.

## Running the benchmark

```bash
cargo run --manifest-path compiler/Cargo.toml -p xenith-bench -- run --model codex --condition hole-guided
```

Models are the subscription CLIs on the maintainer's machine (`codex`, `grok`, `agy`,
`opencode`), dispatched through [`invoke.ps1`](invoke.ps1) — flag conventions differ per tool
and two of them break on argument order, so that knowledge lives in one place. Repair rounds
resend the accumulated exchange, since the CLIs are stateless.

## Why results are committed rather than produced in CI

Runs shell out to subscription CLIs, not metered APIs. Those CLIs are not available to hosted
runners, so benchmark runs happen locally and their reports land in `results/` with the model,
condition and per-round outcomes. This means benchmark numbers are **not independently
reproducible from CI** — the tradeoff is recorded here on purpose.
