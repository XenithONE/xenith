# AI benchmark

Xenith's design goal is that a model writes correct code on the first try. That is measurable, so it
gets measured here. This directory is the project's objective function: a language change is judged
by whether these numbers move.

> **Status: not yet implemented.** There is no compiler to benchmark against.

## Conditions

Each task is attempted under three conditions, and the interesting result is the *comparison*, not
any single number.

| Condition | What the model is given |
| --- | --- |
| `full-pack` | The entire field guide in context |
| `retrieved` | Only task-relevant types, signatures, and verified examples |
| `hole-guided` | Compiler holes and queries available during construction |

The project's central premise is that `hole-guided` > `retrieved` > `full-pack`. **If that ordering
does not hold, the premise is wrong** and the design gets revisited — see
[design/0002 §10](../../design/0002-design-review.md).

An earlier draft benchmarked only the `full-pack` condition. That was dropped: measuring a single
condition cannot tell you whether the architecture is right, and `full-pack` is the condition most
likely to be an inferior deployment shape.

## Metrics

- **pass@1** — fraction of tasks whose first attempt passes the hidden tests
- **mean fix-iterations-to-green** — how many compile/repair rounds to reach passing
- **compile-error rate on first attempt** — isolates syntax/type problems from logic problems

## Layout

```
tasks/      task definitions: prompt, hidden tests, difficulty tier
results/    committed run reports (this directory is NOT gitignored)
```

## Why results are committed rather than produced in CI

Runs shell out to subscription CLIs (`codex`, `grok`, `agy`, `opencode`) rather than metered APIs.
Those CLIs are not available to GitHub Actions runners, so benchmark runs happen locally and their
reports are committed. CI covers builds, tests, and conformance only.

This means benchmark numbers are **not independently reproducible from CI**. Each report records the
CLI versions and the date so a reader can judge how stale it is.
