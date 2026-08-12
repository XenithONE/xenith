# Specification

> **Status: DRAFT.** This is a consolidated description of the language the compiler ships
> **today** (`xenith 0.0.0`). It is normative in intent — where it and the compiler disagree, one
> of the two has a bug worth filing — but there is **no conformance machinery** behind it: no test
> suite is generated from these pages, and none is promised here. What is enforced in CI is
> narrower and real: every `xenith` code block in this directory must parse with the current
> compiler ([`compiler/crates/xenith-syntax/tests/docs.rs`](../compiler/crates/xenith-syntax/tests/docs.rs)),
> and every complete program shown here was checked and run against the actual binary when the
> draft was written, with its output recorded verbatim.

The [design records](../design/) are the decision log: each rule's full reasoning, the reviews
that shaped it, and the reversals. This directory is the consolidation — the rules themselves,
one place, current. Chapters cite design documents by filename; the citation is the "why" in
long form.

## Chapters

| Chapter | Scope |
| --- | --- |
| [00 — Overview](00-overview.md) | What Xenith is, what this draft covers, and the single list of adopted-but-unshipped designs |
| [01 — Lexical structure and syntax](01-lexical-and-syntax.md) | Tokens, literals, comments, items, statements, expressions, patterns, canonical form |
| [02 — Types and inference](02-types-and-inference.md) | Local bidirectional checking, sealed properties, generics, exhaustiveness, typed holes |
| [03 — Effects and capabilities](03-effects-and-capabilities.md) | `uses` clauses, the subset rule, capabilities as values |
| [04 — Evaluation](04-evaluation.md) | Value semantics, evaluation order, traps, exit codes, determinism |
| [05 — Modules and projects](05-modules-and-projects.md) | File = module, `use`, `pub`, project layout, the API surface |
| [06 — Closures](06-closures.md) | Capability-effect-zero function values and where they may appear |
| [07 — The prelude](07-std-prelude.md) | Built-in types and the `List` / `Map` / `String` method surface |
| [08 — Diagnostics and tooling](08-diagnostics-and-tooling.md) | Diagnostic anatomy, the wire contract, teaching, CLI and MCP surfaces |

Start at [00 — Overview](00-overview.md); it says what is deliberately *not* in this draft.
