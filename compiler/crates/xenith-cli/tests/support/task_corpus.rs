//! The task programs, in one place.
//!
//! Two suites read this corpus, and that is the point. `concurrency.rs`
//! asserts what each program *means* — the design/0015 conformance. The
//! differential harness in `executor_equivalence.rs` asserts that the
//! parallel and sequential executors agree on every one of them, byte for
//! byte (design/0017 §5). Sharing the sources is what makes "the harness
//! covers the conformance suite" a fact about the code rather than a claim
//! in a comment.

// Two test binaries include this module and each uses a different part of
// it: the conformance suite reads sources by name and never looks at
// `fate`, the harness reads both.
#![allow(dead_code)]

/// Whether a program is expected to finish at all. Divergence is a real
/// outcome and both executors must produce it: a hang is not a test failure
/// here, it is the assertion.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Fate {
    Terminates,
    /// Neither executor ever finishes. Asserted with a timeout, which is the
    /// only way to observe a hang.
    Diverges,
}

pub struct Program {
    pub name: &'static str,
    pub source: &'static str,
    pub fate: Fate,
}

/// The source of one named program. Panics on a typo, which is what a test
/// helper should do.
pub fn source(name: &str) -> &'static str {
    PROGRAMS
        .iter()
        .find(|program| program.name == name)
        .unwrap_or_else(|| panic!("no program named `{name}` in the corpus"))
        .source
}

pub const PROGRAMS: &[Program] = &[
    // ---------------------------------------------------------------
    // design/0015 conformance — the shape the task boundary shipped in
    // ---------------------------------------------------------------
    Program {
        name: "fan_out",
        fate: Fate::Terminates,
        source: r#"fn square(n: Int) -> Int {
    n * n
}

fn cube(n: Int) -> Int {
    n * n * n
}

fn main(io: Io) -> Result<Unit, Error> uses {Io.write, Task.spawn} {
    let total = scope {
        let a = spawn square(n: 4);
        let b = spawn cube(n: 3);
        a.await + b.await
    };
    io.write(text: total.to_text())?;
    return Ok(unit);
}
"#,
    },
    Program {
        name: "result_err",
        fate: Fate::Terminates,
        source: r#"fn parse(text: String) -> Result<Int, Error> {
    text.try_to_int()
}

fn main(io: Io) -> Result<Unit, Error> uses {Io.write, Task.spawn} {
    scope {
        let j = spawn parse(text: "not-a-number");
        let v = j.await?;
        io.write(text: v.to_text())?;
    }
    return Ok(unit);
}
"#,
    },
    Program {
        name: "result_ok",
        fate: Fate::Terminates,
        source: r#"fn parse(text: String) -> Result<Int, Error> {
    text.try_to_int()
}

fn main(io: Io) -> Result<Unit, Error> uses {Io.write, Task.spawn} {
    scope {
        let j = spawn parse(text: "42");
        let v = j.await?;
        io.write(text: v.to_text())?;
    }
    return Ok(unit);
}
"#,
    },
    Program {
        name: "nested",
        fate: Fate::Terminates,
        source: r#"fn work(n: Int) -> Int {
    n + 1
}

fn main(io: Io) -> Result<Unit, Error> uses {Io.write, Task.spawn} {
    let total = scope {
        let outer = spawn work(n: 10);
        let inner_total = scope {
            let inner = spawn work(n: 100);
            inner.await
        };
        outer.await + inner_total
    };
    io.write(text: total.to_text())?;
    return Ok(unit);
}
"#,
    },
    Program {
        name: "statement_form",
        fate: Fate::Terminates,
        source: r#"fn ping() {
    let x = 1;
}

fn main(io: Io) -> Result<Unit, Error> uses {Io.write, Task.spawn} {
    scope {
        spawn ping();
    }
    io.write(text: "done")?;
    return Ok(unit);
}
"#,
    },
    Program {
        name: "arg_order",
        fate: Fate::Terminates,
        source: r#"fn add(a: Int, b: Int) -> Int {
    a + b
}

fn main(io: Io) -> Result<Unit, Error> uses {Io.write, Task.spawn} {
    var count = 0;
    scope {
        let j = spawn add(a: { count = count + 1; count }, b: { count = count + 1; count * 10 });
        let v = j.await;
        io.write(text: v.to_text())?;
        io.write(text: " ")?;
        io.write(text: count.to_text())?;
    }
    return Ok(unit);
}
"#,
    },
    Program {
        name: "arg_trap",
        fate: Fate::Terminates,
        source: r#"fn add(a: Int, b: Int) -> Int {
    a + b
}

fn main(io: Io) -> Result<Unit, Error> uses {Io.write, Task.spawn} {
    scope {
        let j = spawn add(a: 1 / 0, b: 2 / 0);
        let v = j.await;
        io.write(text: v.to_text())?;
    }
    return Ok(unit);
}
"#,
    },
    Program {
        name: "early_discard",
        fate: Fate::Terminates,
        source: r#"fn work(n: Int) -> Int {
    n * 2
}

fn fail() -> Result<Int, Error> {
    "x".try_to_int()
}

fn main(io: Io) -> Result<Unit, Error> uses {Io.write, Task.spawn} {
    scope {
        let j = spawn work(n: 21);
        let g = fail()?;
        io.write(text: j.await.to_text())?;
        io.write(text: g.to_text())?;
    }
    return Ok(unit);
}
"#,
    },
    Program {
        name: "compat_names",
        fate: Fate::Terminates,
        source: r#"fn main(io: Io) -> Result<Unit, Error> uses {Io.write} {
    let spawn = 40;
    let scope = 2;
    let total = spawn + scope;
    if scope > 0 {
        io.write(text: total.to_text())?;
    }
    return Ok(unit);
}
"#,
    },
    // ---------------------------------------------------------------
    // design/0017 — control effects, commit order, cancellation
    // ---------------------------------------------------------------
    Program {
        name: "child_trap",
        fate: Fate::Terminates,
        source: r#"fn boom() -> Int {
    1 / 0
}

fn main(io: Io) -> Result<Unit, Error> uses {Io.write, Task.spawn} {
    io.write(text: "before ")?;
    scope {
        let j = spawn boom();
        let x = j.await;
        io.write(text: x.to_text())?;
    }
    return Ok(unit);
}
"#,
    },
    Program {
        name: "flight_over",
        fate: Fate::Terminates,
        source: r#"fn plan(n: Int) -> Int {
    n * 2
}

fn main(io: Io) -> Result<Unit, Error> uses {Io.write, Task.spawn} {
    io.write(text: "before ")?;
    scope {
        let a = spawn plan(n: 1);
        let b = spawn plan(n: 2);
        let sum = a.await + b.await;
        io.write(text: sum.to_text())?;
    }
    io.write(text: " after")?;
    return Ok(unit);
}
"#,
    },
    // Two children trap. Spawn order decides which trap the program
    // reports, because sequentially child 1's fate was sealed at its spawn
    // statement — before child 2 existed (design/0017 §3).
    Program {
        name: "two_traps",
        fate: Fate::Terminates,
        source: r#"fn boom_a() -> Int {
    1 / 0
}

fn boom_b() -> Int {
    2 % 0
}

fn main(io: Io) -> Result<Unit, Error> uses {Io.write, Task.spawn} {
    scope {
        let a = spawn boom_a();
        let b = spawn boom_b();
        let total = a.await + b.await;
        io.write(text: total.to_text())?;
    }
    return Ok(unit);
}
"#,
    },
    Program {
        name: "second_child_traps",
        fate: Fate::Terminates,
        source: r#"fn fine() -> Int {
    7
}

fn boom() -> Int {
    2 % 0
}

fn main(io: Io) -> Result<Unit, Error> uses {Io.write, Task.spawn} {
    scope {
        let a = spawn fine();
        let b = spawn boom();
        let total = a.await + b.await;
        io.write(text: total.to_text())?;
    }
    return Ok(unit);
}
"#,
    },
    // The await order is not the spawn order: `b` is awaited first, but the
    // trap that must surface is `a`'s.
    Program {
        name: "await_out_of_spawn_order",
        fate: Fate::Terminates,
        source: r#"fn boom() -> Int {
    1 / 0
}

fn fine() -> Int {
    7
}

fn main(io: Io) -> Result<Unit, Error> uses {Io.write, Task.spawn} {
    scope {
        let a = spawn boom();
        let b = spawn fine();
        let total = b.await + a.await;
        io.write(text: total.to_text())?;
    }
    return Ok(unit);
}
"#,
    },
    // codex's counterexample: without cooperative cancellation the parallel
    // executor can never reach exit 101 here, because the scope cannot be
    // left while the diverging sibling still runs.
    Program {
        name: "trap_beside_diverging_sibling",
        fate: Fate::Terminates,
        source: r#"fn boom() -> Int {
    1 / 0
}

fn forever() -> Int {
    var i = 0;
    while i == 0 {
        i = 0;
    }
    1
}

fn main(io: Io) -> Result<Unit, Error> uses {Io.write, Task.spawn} {
    scope {
        let a = spawn boom();
        let b = spawn forever();
        let total = a.await + b.await;
        io.write(text: total.to_text())?;
    }
    return Ok(unit);
}
"#,
    },
    // Divergence with no trap to cancel it: both executors hang, and that
    // agreement is the assertion.
    Program {
        name: "diverging_first_fine_second",
        fate: Fate::Diverges,
        source: r#"fn forever() -> Int {
    var i = 0;
    while i == 0 {
        i = 0;
    }
    1
}

fn fine() -> Int {
    7
}

fn main(io: Io) -> Result<Unit, Error> uses {Io.write, Task.spawn} {
    scope {
        let a = spawn forever();
        let b = spawn fine();
        let total = a.await + b.await;
        io.write(text: total.to_text())?;
    }
    return Ok(unit);
}
"#,
    },
    Program {
        name: "lone_diverging_child",
        fate: Fate::Diverges,
        source: r#"fn forever() -> Int {
    var i = 0;
    while i == 0 {
        i = 0;
    }
    1
}

fn main(io: Io) -> Result<Unit, Error> uses {Io.write, Task.spawn} {
    scope {
        let j = spawn forever();
        io.write(text: j.await.to_text())?;
    }
    return Ok(unit);
}
"#,
    },
    // A nested scope's child was spawned after the outer one's, so the
    // outer's trap is the one that happened first.
    Program {
        name: "nested_scope_traps",
        fate: Fate::Terminates,
        source: r#"fn boom_a() -> Int {
    1 / 0
}

fn boom_b() -> Int {
    2 % 0
}

fn main(io: Io) -> Result<Unit, Error> uses {Io.write, Task.spawn} {
    scope {
        let a = spawn boom_a();
        let inner = scope {
            let b = spawn boom_b();
            b.await
        };
        let total = a.await + inner;
        io.write(text: total.to_text())?;
    }
    return Ok(unit);
}
"#,
    },
    Program {
        name: "scope_in_a_loop",
        fate: Fate::Terminates,
        source: r#"fn work(n: Int) -> Int {
    n * 2
}

fn main(io: Io) -> Result<Unit, Error> uses {Io.write, Task.spawn} {
    var i = 0;
    var total = 0;
    while i < 5 {
        let step = scope {
            let j = spawn work(n: i);
            j.await
        };
        total = total + step;
        i = i + 1;
    }
    io.write(text: total.to_text())?;
    return Ok(unit);
}
"#,
    },
    Program {
        name: "spawn_in_a_loop_inside_one_scope",
        fate: Fate::Terminates,
        source: r#"fn work(n: Int) -> Int {
    n * 3
}

fn main(io: Io) -> Result<Unit, Error> uses {Io.write, Task.spawn} {
    var i = 0;
    var total = 0;
    scope {
        while i < 5 {
            let j = spawn work(n: i);
            total = total + j.await;
            i = i + 1;
        }
    }
    io.write(text: total.to_text())?;
    return Ok(unit);
}
"#,
    },
    // More children at once than the pool has threads: the queue holds the
    // excess, in spawn order, which is also commit order.
    Program {
        name: "more_children_than_threads",
        fate: Fate::Terminates,
        source: r#"fn work(n: Int) -> Int {
    n * n
}

fn main(io: Io) -> Result<Unit, Error> uses {Io.write, Task.spawn} {
    let total = scope {
        let a = spawn work(n: 1);
        let b = spawn work(n: 2);
        let c = spawn work(n: 3);
        let d = spawn work(n: 4);
        let e = spawn work(n: 5);
        let f = spawn work(n: 6);
        let g = spawn work(n: 7);
        let h = spawn work(n: 8);
        a.await + b.await + c.await + d.await + e.await + f.await + g.await + h.await
    };
    io.write(text: total.to_text())?;
    return Ok(unit);
}
"#,
    },
    // A trapping child whose result nothing awaits: the scope's closing
    // brace joins it, and the trap still surfaces.
    Program {
        name: "statement_form_child_traps",
        fate: Fate::Terminates,
        source: r#"fn boom() {
    let x = 1 / 0;
}

fn main(io: Io) -> Result<Unit, Error> uses {Io.write, Task.spawn} {
    io.write(text: "before ")?;
    scope {
        spawn boom();
    }
    io.write(text: "after")?;
    return Ok(unit);
}
"#,
    },
    // The parent trapped, but a child spawned earlier had already trapped:
    // the child's trap is the one that happened first.
    Program {
        name: "child_trap_outranks_a_later_parent_trap",
        fate: Fate::Terminates,
        source: r#"fn boom() -> Int {
    1 / 0
}

fn main(io: Io) -> Result<Unit, Error> uses {Io.write, Task.spawn} {
    scope {
        let j = spawn boom();
        let x = 5 % 0;
        let v = j.await + x;
    }
    return Ok(unit);
}
"#,
    },
    // An early `?` exit with a trapping child already spawned: sequentially
    // the child trapped at its spawn statement, before `fail()` ran, so the
    // trap beats the `Err` — exit 101, not exit 1.
    Program {
        name: "child_trap_outranks_a_later_early_exit",
        fate: Fate::Terminates,
        source: r#"fn boom() -> Int {
    1 / 0
}

fn fail() -> Result<Int, Error> {
    "x".try_to_int()
}

fn main(io: Io) -> Result<Unit, Error> uses {Io.write, Task.spawn} {
    scope {
        let j = spawn boom();
        let g = fail()?;
        let v = j.await + g;
    }
    return Ok(unit);
}
"#,
    },
    // ---------------------------------------------------------------
    // design/0017 §1 — refusals. Checking is executor-independent, and the
    // harness proves it byte for byte rather than assuming it.
    // ---------------------------------------------------------------
    Program {
        name: "flight_effect_refusal",
        fate: Fate::Terminates,
        source: r#"fn boom() -> Int {
    1 / 0
}

fn main(io: Io) -> Result<Unit, Error> uses {Io.write, Task.spawn} {
    io.write(text: "before ")?;
    scope {
        let j = spawn boom();
        io.write(text: "between ")?;
        let x = j.await;
        io.write(text: x.to_text())?;
    }
    return Ok(unit);
}
"#,
    },
    Program {
        name: "flight_statement_form_refusal",
        fate: Fate::Terminates,
        source: r#"fn ping() {
    let x = 1;
}

fn main(io: Io) -> Result<Unit, Error> uses {Io.write, Task.spawn} {
    scope {
        spawn ping();
        io.write(text: "nope")?;
    }
    return Ok(unit);
}
"#,
    },
    Program {
        name: "flight_effectful_call_refusal",
        fate: Fate::Terminates,
        source: r#"fn plan(n: Int) -> Int {
    n
}

fn shout(io: Io) -> Result<Unit, Error> uses {Io.write} {
    io.write(text: "x")
}

fn main(io: Io) -> Result<Unit, Error> uses {Io.write, Task.spawn} {
    scope {
        let a = spawn plan(n: 1);
        shout(io: io)?;
        io.write(text: a.await.to_text())?;
    }
    return Ok(unit);
}
"#,
    },
];
