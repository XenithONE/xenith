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

### Teaching conditions (0009 v3)

The 2×2 arms of [design/0009 §4](../../design/0009-diagnostics-that-teach.md): the docs factor
again, crossed with whether diagnostics carry their structured `teaches` section (the off arms
pass `--diagnostic-teaching=off` to every compiler call):

| Condition | API table in guide | Diagnostic teaching in feedback |
| --- | --- | --- |
| `v3-plain` | no | no |
| `v3-teach` | no | yes |
| `v3-docs` | yes | no |
| `v3-docs-teach` | yes | yes |

Round-1 prompts are byte-identical across the teaching factor — teaching exists only in
post-failure compiler output, so a prompt cannot reveal which arm a model is in. The
goals-on-holes channel is disabled in all four arms, so the teaches section is the only
feedback difference. Primary endpoints are rounds-to-green, final green, and signature
adoption judged from the recorded per-round feedback (the consumption oracle, 0009 §1b), with
pass@1 kept as a sanity check that must stay flat across the teaching factor; results are
exploratory per 0007 §5-5.

### Constrained-integration conditions (0011 tier-5)

The 3×2 arms of [design/0011 §2](../../design/0011-measurement-rfc.md): docs form crossed
with diagnostic teaching, over six frozen **project** tasks in
[`tasks-t5/`](tasks-t5/ALLOCATION.md) — four t5a implementation grafts (the frozen
`src/main.xn` is the calling contract; the model writes one new module file) and two t5b
wiring tasks (the model writes `src/main.xn` itself):

| Condition | Docs in the prompt | Diagnostic teaching in feedback |
| --- | --- | --- |
| `t5-guide-on` / `t5-guide-off` | field guide + std API table | on / off |
| `t5-api-on` / `t5-api-off` | the task's frozen machine-generated api-dump + std API table | on / off |
| `t5-none-on` / `t5-none-off` | nothing beyond the shared primer | on / off |

Every arm shares [`primer-syntax.md`](primer-syntax.md) — syntax only, deliberately silent
on modules, `use`, qualified references and `pub` (0011 §2: pre-teaching the measured
discipline would be treatment, not control) — plus the task statement, the target path and
the file-output contract; t5a arms additionally carry the frozen `main.xn` verbatim.
**Provided module sources never enter any prompt**, and round-1 prompts are byte-identical
across the teaching factor (both asserted in tests). Round 1 sees zero compiler output;
repair rounds rebuild the project from a fresh skeleton copy, run `xenith check` / `xenith
run` in project mode, and differ across the teaching factor only by
`--diagnostic-teaching`. Runs follow the frozen shuffle table
[`tasks-t5/shuffle-order.tsv`](tasks-t5/shuffle-order.tsv) (`verify` recomputes it), and
each round's record carries the submitted file, diagnostic codes, fix availability and
teach-line counts — the raw material for the 0011 §6 post-hoc observations, including the
green-but-never-references-a-provided-module flag in `summarize`.

The per-task `api-dump.txt` is generated by `xenith-bench api-dump <skeleton>` from the
compiler's own syntax tree — deterministic ordering, generator version and content hash in
the header, never hand-edited. `verify` regenerates every dump and fails on drift, then
checks the golden gate: every provided-module surface the reference solution consumes
must appear in the dump (0011 §7 — no measuring against a broken map). With the six
project references, `verify` covers 22 references.

**Reading (252/252 runs, 2026-08-08).** The family split is the finding — a double
dissociation the pooled table hides:

| family | guide arms | api arms | none arms |
| --- | --- | --- | --- |
| t5a implementation, pass@1 (of 56) | **2** | **17** | 4 |
| t5b wiring, pass@1 (of 28) | **12** | **0** | 1 |

The field guide structurally cannot carry a project's API, so on implementation
tasks it is nearly as blind as nothing (2 vs 4 first-shot) while the machine dump
carries them (17). But the dump never teaches `use` discipline, so on wiring
tasks it goes to zero while the guide's eight-line module section pays 12.
*What to call* and *how to wire* are different knowledge in different documents;
neither substitutes for the other — the 0011 central prediction ("full-pack
stops scaling the moment a second file exists") is confirmed, and the honest
product shape is primer + repo map, not either alone. Other pre-registered
outcomes: none+off is the floor (H0 holds); 164/252 runs opened with a
module-discipline diagnostic (H3's "majority" confirmed at 65%, largely repaired
by the loop — 155/252 green overall); diagnostic teaching moved nothing overall
(amendment 1's coverage map, registered before these runs) *except* exactly
where its coverage exists — wiring tasks with no docs, where `use_candidates`
lifts green 6/14 → 10/14; the XN7008 temptation never fired in 252 runs (no
model ever attempted a cross-boundary write — a null observation point); and
zero greens bypassed the provided API.

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

### v2: the hole permission changed nothing — and that is the finding

The pilot's diagnosis was that the query channel never fired because no arm invited holes. v2
added the same one sentence to every arm — *if you cannot determine an expression, write `??`
instead of guessing* — re-ran all 28 cells, and the totals barely moved (pass@1 / 42):

| | docs | query | docs-query | blind |
| --- | --- | --- | --- | --- |
| v1 | 29 | 21 | 36 | 22 |
| v2 | 29 | 22 | **37** | 23 |

And `goals` spoke in **2 of 46** failure rounds — the same as v1's 2 of 50. Three conclusions,
one caveat:

1. **Permission does not activate the channel either.** Across seven models and two runs,
   models almost never choose to write a partial program; they commit to complete guesses.
   The passive, hole-triggered query mechanism goes unused not because models were forbidden
   but because holes are not in any model's writing habit. If the compiler's knowledge is to
   matter, it has to be **volunteered** — surfaced on ordinary type errors and unknown-method
   diagnostics without the model asking — which is the next design iteration (0008 §3 already
   points there: active `producers-at` / `methods-of` style delivery).
2. **The docs factor replicates exactly** (+7 to +15 pass@1 on both its edges, twice). The API
   table — really the named-argument signatures — remains the one intervention that reliably
   moves first-try correctness.
3. **`docs-query` tops both runs** (36 then 37 of 42, perfect green twice) — but its pass@1
   edge over `docs` cannot be caused by the query channel, which only speaks after a failure.
   The only textual difference in the first prompt is the sentence describing the feedback
   channel, so the edge is either replicated noise or a priming effect of mentioning goals and
   producers. Recorded as unexplained rather than claimed.

Caveat: the compiler changed between runs (XN1008/XN5001 landed), so v1↔v2 comparisons carry
that confound; comparisons *between arms within v2* do not.

## The teaching experiment (0009 v3, exploratory)

Seven models × the four v3 arms × the six frozen tasks — 168 runs. Totals (pass@1 · green
of 42): `v3-plain` 24 · 34, `v3-teach` 24 · **40**, `v3-docs` 33 · 41, `v3-docs-teach` 30 · 41.
Three readings:

1. **The sanity check holds exactly: pass@1 is 24 = 24 across the teaching factor.** Teaching
   lives in post-failure feedback and cannot touch the first attempt — and it didn't, to the
   point. This is what a clean design looks like, and it is also 0009 §1's admission made
   visible: the north star did not move.
2. **Teaching lifts final green from 34 to 40 of 42 with no documentation at all** — nearly
   matching the docs arm's 41. The weakest models gain the most (nemotron 1/6 → 4/6,
   deepseek 4/6 → 6/6 green). Where the hole channel spoke in 4–5% of failure rounds across
   two experiments, **teaches fired in 19 of 31** — because XN2003 and the argument-shape
   family are the errors models actually make. Delivery at the diagnostic works; delivery at
   the hole never did.
3. **Docs and teaching are substitutes on final success** (41 = 41 green with docs, with or
   without teaching): once the table is in context, the catalogue in the diagnostic answers a
   question already answered. The practical reading for real projects: past the context
   budget, the compiler can carry what the guide cannot.

Exploratory per 0007 §5-5; per-round feedback and diagnostic codes are recorded in the result
files for deeper consumption analysis.

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
