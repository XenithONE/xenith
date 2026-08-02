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

### Separation conditions (0007)

The 2×2 arms of [design/0007 §5](../../design/0007-std-minimal.md), which factor apart what
`hole-guided` bundled. The std API table is [`api-table.md`](api-table.md):

| Condition | API table in guide | `goals`/`producers` in feedback |
| --- | --- | --- |
| `docs` | yes | no |
| `query` | no | yes |
| `docs-query` | yes | yes |
| `blind` | no | no |

All four arms share identical budgets (rounds cap 4) and identical prompts apart from those
two factors — no asymmetric nudges in any arm. The main comparisons are `query` vs `blind`
and `docs-query` vs `docs`, and results are reported as an exploratory pilot per 0007 §5-5.

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

Models are the subscription CLIs on the maintainer's machine, dispatched through
[`invoke.ps1`](invoke.ps1) — flag conventions differ per tool and two of them break on argument
order, so that knowledge lives in one place. Repair rounds resend the accumulated exchange,
since the CLIs are stateless.

Seven columns, two of them deliberately unusual. `cursor` is Cursor's **Auto router**: each call
may land on a different underlying model, so the cell measures the router as deployed — a
mixture, and labelled as one. `opencode-deepseek` and `opencode-nemotron` reach different model
*families* through the same CLI, so the matrix is not just five flavours of one lab. Runs resume
by default: a cell is accumulated over short bursts, and re-invoking the same command continues
where the last burst stopped.

## The first full matrix (2026-08-02)

Seven models, three conditions, ten tasks — 210 measured runs, accumulated over short local
bursts. The generated table lives in [`results/summary.md`](results/summary.md); regenerate it
with `xenith-bench summarize`. Four readings, including the one that did not go our way:

1. **Bare is a wipeout: 0/70 pass@1.** No model — not one, on any task — writes correct
   Xenith first-try from its priors, and repair rounds salvage only 7/70. Negative transfer
   from Rust/TS is total on a training-data-zero language. This is the control the whole
   project rests on, and it is unambiguous.
2. **The field guide flips the board: 66/70 pass@1, 70/70 green.** ~1,500 tokens of context
   pack turn every model near-perfect — including two free-tier models that scored zero bare.
   Capability barely matters; context dominates. This is design/0002's context-pack thesis at
   maximum amplitude.
3. **Hole-guided ties full-pack (66 = 66) instead of beating it.** The premise's second
   inequality — `hole-guided > full-pack` — is **not demonstrated at this task scale**, and
   that is recorded rather than massaged. With the guide already at ~95% pass@1 there is no
   headroom left for holes to show value: a ceiling effect, not a refutation. The separation
   test needs tasks with genuine uncertainty — real APIs to discover, underspecified
   requirements — which arrive with `std/`. Until then the central premise stands only on its
   first inequality.
4. **Mean rounds-to-green is ~1.0 with the guide, 2.3–3.5 without.** The guide does not make
   models better at repairing — it makes repair unnecessary. Diagnostics earn their keep in
   the rare miss (every full-pack/hole-guided miss recovered in one round).

## The separation pilot (2026-08-02, exploratory)

Seven models × the four 0007 arms × the six frozen tier-4 tasks — 168 runs. The generated
table is in [`results/summary.md`](results/summary.md). Totals (pass@1 · green out of 42):
`blind` 22 · 36, `query` 21 · 37, `docs` 29 · 41, `docs-query` 36 · 42. Four readings:

1. **The ceiling broke.** Tier-4 under `blind` runs at ~52% pass@1 where tiers 1–3 ran at 94%
   with a guide — these tasks finally have room to separate conditions.
2. **The docs factor is the big one** (+7 and +15 pass@1 along its two edges). Models guess
   conventional *names* well — `blind` still repairs to 36/42 green — but exact *signatures*,
   above all Xenith's mandatory named arguments (`get(index:)`, `insert(key:, value:)`), are
   what they cannot invent first-try. An API table pays even when every name is guessable.
3. **The query factor read ≈0 — because the channel went unused, not because it failed.**
   Across all query-family cells, only 2 of 50 failure rounds carried any `goals` output: the
   arms carry no behavioral nudges, no model spontaneously wrote a `??` hole, and `goals` has
   nothing to say about a program without holes. What the old `hole-guided` condition bundled
   as "the invitation to leave holes" turns out to be the *activation condition* of the whole
   query mechanism. Measuring the channel's value requires giving every arm the same
   hole-usage instruction — the next iteration's design problem, recorded here rather than
   papered over.
4. `docs-query` is the only perfect-green cell block (42/42) and the best pass@1 (36) — but
   with the query channel dormant, its edge over `docs` is confounded with the one
   feedback-mechanics sentence the arms are allowed to differ by. Exploratory, per 0007 §5-5;
   no verdict is claimed.

## Two measurement bugs, caught and fixed

Recorded here because the numbers only mean something if the instrument is honest.

**Empty replies were judged as programs.** One CLI (agy) reacts to compiler feedback by
reaching for shell tools; headless mode auto-denies the permission prompt and the reply comes
back empty. The harness judged the empty file — which passes `check` (an empty module is
legal) and fails `run` with "no main" — and recorded a *runtime failure* of a program the
model never wrote, then quoted the empty file back as the model's "previous attempt". Every
agy bare repair round died this way, which produced a suspiciously uniform 0/10 and exposed
the bug. Empty replies are now their own outcome class, and the contract tells every model
uniformly: answer directly, no tools, the harness runs your code.

**The working directory answered the exam.** The CLIs were spawned from inside the
repository, and several of them are agents with ambient workspace access even in single-answer
mode. Cursor's ask mode scored a *perfect* 10/10 bare — implausible, since `bare` withholds
all documentation — and re-asking from an empty directory produced the same guessed-syntax
Xenith every other bare model writes. It had been reading the field guide and the reference
solutions out of the working tree. Every CLI now runs from a neutral empty directory outside
the repo, and grok's default-on web search is disabled (the language has a public repository —
a searching model could fetch what the condition withholds). All bare cells measured before
this fix were voided and re-run; the voided reports are archived beside the live ones with
`.void-*` / `.cwd-repo*` suffixes instead of deleted.

## Why results are committed rather than produced in CI

Runs shell out to subscription CLIs, not metered APIs. Those CLIs are not available to hosted
runners, so benchmark runs happen locally and their reports land in `results/` with the model,
condition and per-round outcomes. This means benchmark numbers are **not independently
reproducible from CI** — the tradeoff is recorded here on purpose.
