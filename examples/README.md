# Examples

> **These do not run yet.** There is no backend and only a provisional
> prelude — but they parse, format, **type-check** (including effects), and
> answer `xenith goals`.
>
> They are written to be read, and to be kept honest by the tooling that does
> exist: every file here is parsed, type-checked and format-verified in CI, so
> an example cannot rot. That is not hypothetical — wiring the checker into
> the test suite immediately caught a real bug in `hello.xn` (`return unit;`
> where `Result<Unit, Error>` requires `return Ok(unit);`).

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
Ask the compiler what belongs there:

```bash
cargo run --manifest-path compiler/Cargo.toml -p xenith -- goals examples/scores.xn
```

```console
examples/scores.xn:59:5 — hole ??lookup in try_find
  expected: Result<Player, ScoreError>
  in scope: name: String
  effects:  none permitted
  candidates:
    1. Err(??)
    2. Ok(??)
    3. try_award(player: ??, points: ??)
```
