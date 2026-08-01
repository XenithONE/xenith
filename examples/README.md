# Examples

> **These do not run yet.** There is no standard library and no backend — the
> compiler currently lexes, parses, and formats. `xenith check` will accept
> these files and `xenith fmt` leaves them unchanged, which is the extent of
> what can be demonstrated today.
>
> They are written to be read, and to be kept honest by the tooling that does
> exist: every file here is checked and format-verified in CI, so an example
> cannot rot into something that no longer parses.

| File | Shows |
| --- | --- |
| [`hello.xn`](hello.xn) | Capabilities as parameters, and a signature that proves a function performs no IO |
| [`scores.xn`](scores.xn) | Exhaustive matching, `Result` and `?`, the naming rules, and a typed hole |

## Try them

```bash
cargo run --manifest-path compiler/Cargo.toml -p xenith -- check examples/hello.xn
```

```bash
cargo run --manifest-path compiler/Cargo.toml -p xenith -- fmt --check examples/scores.xn
```

`scores.xn` ends with a function whose body is the hole `??lookup`. That file
checks cleanly: a partial program is a normal state in Xenith, not an error.
Once `xenith goals` exists it will report the type required at that hole, the
bindings in scope, and the effects permitted — see
[design/0002](../design/0002-design-review.md).
