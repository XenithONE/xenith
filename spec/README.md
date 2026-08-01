# Specification

> **Status: not yet written.** The normative specification starts once the design decisions in
> [`../design/`](../design/) stop moving. Two of them were reversed within a day of being made, so
> writing a spec on top of them now would be writing it twice.

## What is settled

| Area | Document |
| --- | --- |
| Goals, non-goals, naming | [0001](../design/0001-why-xenith.md) |
| North star, capabilities, effects, evaluation method | [0002](../design/0002-design-review.md) |
| Semantic kernel — evaluation order, moves, copies, equality | [0003](../design/0003-semantic-kernel.md) |
| Concurrency, race freedom, `Transfer` / `ShareSafe` | [0004](../design/0004-concurrency.md) |

[0003](../design/0003-semantic-kernel.md) is the one-page semantic kernel. Everything the
specification will eventually say rests on it, so read it first.

## Planned layout

| File | Contents |
| --- | --- |
| `00-kernel.md` | The semantic kernel, promoted from 0003 |
| `10-lexical.md` | Tokens, literals, comments |
| `20-syntax.md` | Grammar |
| `30-types.md` | Type system, generics, local inference rules |
| `40-effects.md` | Capabilities and closed effect sets |
| `50-concurrency.md` | Tasks, scopes, `Transfer` / `ShareSafe`, memory model |
| `60-stdlib-index.md` | Standard library index |

## Literate specification

Specification files are the single source of truth. Code blocks in them carry expected output, and
five artifacts are generated from that one source:

- human-readable documentation
- the field guide pack consumed by models
- conformance tests, which execute the blocks and compare against the recorded output
- training pairs
- seeds for the AI benchmark

Because the conformance tests *are* the specification's examples, the specification cannot silently
drift from the implementation.
