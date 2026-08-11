# Xenith syntax primer

You are writing **Xenith**, a small language shaped like Rust and TypeScript. Statements end
with `;`; a block's value is its final expression, written without `;`. `let` binds
immutably, `var` mutably.

```xenith
fn glide(distance: Int, lift: Int) -> Int {
    distance + lift
}

fn main(io: Io) -> Result<Unit, Error> uses {Io.write} {
    let start = 4;
    var total = 0;
    var i = 0;
    while i < 3 {
        total = total + glide(distance: start, lift: i);
        i = i + 1;
    }
    io.write(text: total.to_text())?;
    return Ok(unit);
}
```

- Calls with two or more arguments must name every argument, in declaration order:
  `glide(distance: 4, lift: 1)`. One-argument calls may stay positional.
- Effects are declared: a function that writes says `uses {Io.write}` after its return type,
  and every caller must declare the same effects. `main(io: Io)` receives the `Io` capability
  value; there is no global print. `io.write(text: ...)` returns a `Result` and adds no
  newline. There is no `format!` and no string interpolation — build strings with
  `concat(other: ...)` and `to_text()`.
- No null, no exceptions: absence is `Option<T>` (`Some(v)` / `None`), failure is
  `Result<T, E>` (`Ok(v)` / `Err(e)`). `expr?` unwraps `Ok`/`Some` or returns the `Err`/`None`
  from the enclosing function, whose return type must agree. `return` always takes an
  operand: `return Ok(unit);` — never a bare `return;`.
- `match` must cover every possible case:

```xenith,in-fn
let told = match maybe_count {
    Some(n) if n > 0 => n.to_text(),
    Some(_) => "spent",
    None => "unknown",
};
```
