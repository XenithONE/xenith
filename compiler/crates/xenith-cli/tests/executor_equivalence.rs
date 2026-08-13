//! The main gate for design/0017: the two executors must agree, byte for
//! byte, on every task program.
//!
//! design/0017 §5 rejected "the conformance suite still passes" as the
//! gate — it does not measure the axis that breaks. What breaks when
//! children start running for real is *which* trap surfaces, *whether* the
//! program ends at all, and what stdout held when it did. So the gate is a
//! differential test: run the same source through the parallel executor and
//! through the sequential one, and compare stdout, stderr and exit code
//! exactly. The sequential executor is kept for this reason and no other.
//!
//! Divergence is compared too. A program that never finishes is a real
//! outcome, and the two executors must produce it together; the only way to
//! observe it is a timeout, so a timeout is what the corpus asserts.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

#[path = "support/task_corpus.rs"]
mod task_corpus;

use task_corpus::{Fate, PROGRAMS};

/// Long enough that a terminating program on a loaded machine still
/// finishes, short enough that the two diverging cases do not dominate the
/// suite.
const DIVERGENCE_TIMEOUT: Duration = Duration::from_secs(5);

/// A program that terminates gets much longer: a timeout here is a failure,
/// not an assertion, so the bar is set where only a real hang trips it.
const TERMINATION_TIMEOUT: Duration = Duration::from_secs(120);

#[derive(PartialEq, Eq, Debug)]
enum Verdict {
    Finished {
        exit: i32,
        stdout: String,
        stderr: String,
    },
    TimedOut,
}

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir()
        .join("xenith-0017-equivalence")
        .join(format!("{name}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("scratch directory");
    dir
}

/// Run one program under one executor. Output goes to files rather than
/// pipes: a killed child cannot deadlock a reader that way.
fn execute(dir: &Path, sequential: bool, timeout: Duration) -> Verdict {
    let tag = if sequential { "seq" } else { "par" };
    let out_path = dir.join(format!("{tag}.out"));
    let err_path = dir.join(format!("{tag}.err"));
    let out = std::fs::File::create(&out_path).expect("stdout file");
    let err = std::fs::File::create(&err_path).expect("stderr file");

    let mut command = Command::new(env!("CARGO_BIN_EXE_xenith"));
    command
        .current_dir(dir)
        .args(["run", "main.xn"])
        .stdin(Stdio::null())
        .stdout(out)
        .stderr(err);
    if sequential {
        command.arg("--sequential");
    }
    let mut child = command.spawn().expect("the compiler binary runs");

    let started = Instant::now();
    let status = loop {
        match child.try_wait().expect("wait on the compiler") {
            Some(status) => break Some(status),
            None if started.elapsed() >= timeout => {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
            None => std::thread::sleep(Duration::from_millis(10)),
        }
    };

    let Some(status) = status else {
        return Verdict::TimedOut;
    };
    let read = |path: &Path| {
        String::from_utf8(std::fs::read(path).expect("output file")).expect("output is UTF-8")
    };
    Verdict::Finished {
        exit: status.code().unwrap_or(-1),
        stdout: read(&out_path),
        stderr: read(&err_path),
    }
}

/// Both executors, on one source, from the same directory — so the paths
/// the diagnostics print are the same string in both runs and any
/// difference is a real difference.
fn both(name: &str, source: &str, timeout: Duration) -> (Verdict, Verdict) {
    let dir = scratch(name);
    std::fs::write(dir.join("main.xn"), source).expect("write test program");
    let parallel = execute(&dir, false, timeout);
    let sequential = execute(&dir, true, timeout);
    (parallel, sequential)
}

#[test]
fn the_two_executors_agree_byte_for_byte_on_every_task_program() {
    for program in PROGRAMS {
        let timeout = match program.fate {
            Fate::Terminates => TERMINATION_TIMEOUT,
            Fate::Diverges => DIVERGENCE_TIMEOUT,
        };
        let (parallel, sequential) = both(program.name, program.source, timeout);
        assert_eq!(
            parallel, sequential,
            "`{}` behaves differently under the two executors:\n\
             parallel:   {parallel:#?}\nsequential: {sequential:#?}",
            program.name
        );

        // Agreement on its own is not enough: two executors that both
        // stopped diverging would still agree. The declared fate is what
        // keeps the cancellation and divergence cases honest — the
        // design/0016 lesson that green is not evidence the feature ran.
        for (label, verdict) in [("parallel", &parallel), ("sequential", &sequential)] {
            match (program.fate, verdict) {
                (Fate::Terminates, Verdict::TimedOut) => panic!(
                    "`{}` never finished under the {label} executor",
                    program.name
                ),
                (Fate::Diverges, Verdict::Finished { .. }) => panic!(
                    "`{}` finished under the {label} executor, but the corpus says it \
                     diverges: {verdict:#?}",
                    program.name
                ),
                _ => {}
            }
        }
    }
}

// ------------------------------------------------------- the pointed cases
//
// Agreement alone cannot say *what* the two executors agree on. These pin
// the answers design/0017 §3 argues for, so that a change which broke both
// executors in the same direction would still be caught.

fn parallel_only(name: &str, source: &str) -> Verdict {
    let dir = scratch(name);
    std::fs::write(dir.join("main.xn"), source).expect("write test program");
    execute(&dir, false, TERMINATION_TIMEOUT)
}

fn finished(verdict: &Verdict) -> (i32, &str, &str) {
    match verdict {
        Verdict::Finished {
            exit,
            stdout,
            stderr,
        } => (*exit, stdout.as_str(), stderr.as_str()),
        Verdict::TimedOut => panic!("expected the program to finish"),
    }
}

#[test]
fn a_trap_beside_a_diverging_sibling_still_reaches_exit_101() {
    // The case that killed arrival-order commit: nothing joins the diverging
    // sibling, so only cooperative cancellation lets the program end.
    let verdict = parallel_only(
        "pointed_trap_beside_diverging",
        task_corpus::source("trap_beside_diverging_sibling"),
    );
    let (exit, _, stderr) = finished(&verdict);
    assert_eq!(exit, 101, "{stderr}");
    assert!(
        stderr.contains("task `boom` trapped: division by zero"),
        "the trapping child is the one reported:\n{stderr}"
    );
}

#[test]
fn two_trapping_children_report_the_first_in_spawn_order() {
    let verdict = parallel_only("pointed_two_traps", task_corpus::source("two_traps"));
    let (exit, _, stderr) = finished(&verdict);
    assert_eq!(exit, 101);
    assert!(
        stderr.contains("task `boom_a` trapped: division by zero"),
        "spawn order decides, not arrival order:\n{stderr}"
    );
    assert!(!stderr.contains("boom_b"), "{stderr}");
}

#[test]
fn a_trap_in_an_outer_child_outranks_one_in_a_nested_scope() {
    let verdict = parallel_only("pointed_nested", task_corpus::source("nested_scope_traps"));
    let (exit, _, stderr) = finished(&verdict);
    assert_eq!(exit, 101);
    assert!(
        stderr.contains("task `boom_a` trapped"),
        "the outer child was spawned first, so its fate was sealed first:\n{stderr}"
    );
}

#[test]
fn awaiting_out_of_spawn_order_still_commits_in_spawn_order() {
    let verdict = parallel_only(
        "pointed_await_order",
        task_corpus::source("await_out_of_spawn_order"),
    );
    let (exit, _, stderr) = finished(&verdict);
    assert_eq!(exit, 101);
    assert!(
        stderr.contains("task `boom` trapped"),
        "`b.await` must commit `a` first:\n{stderr}"
    );
}

#[test]
fn a_child_trap_outranks_a_parent_trap_that_happened_later() {
    let verdict = parallel_only(
        "pointed_child_over_parent",
        task_corpus::source("child_trap_outranks_a_later_parent_trap"),
    );
    let (exit, _, stderr) = finished(&verdict);
    assert_eq!(exit, 101);
    assert!(
        stderr.contains("task `boom` trapped: division by zero"),
        "the child's fate was sealed at its spawn statement, before `5 % 0`:\n{stderr}"
    );
    assert!(!stderr.contains("remainder by zero"), "{stderr}");
}

#[test]
fn an_in_flight_effect_is_refused_identically_by_both_executors() {
    let (parallel, sequential) = both(
        "pointed_refusal",
        task_corpus::source("flight_effect_refusal"),
        TERMINATION_TIMEOUT,
    );
    assert_eq!(parallel, sequential);
    let (exit, stdout, _) = finished(&parallel);
    assert_eq!(exit, 2, "a file with diagnostics is refused, not run");
    assert!(stdout.contains("error[XN6011]"), "{stdout}");
}

#[test]
fn children_really_run_at_the_same_time() {
    // Everything above would also pass if `spawn` still ran children one at
    // a time on this thread. Four children, each a few hundred million
    // interpreter steps: with a pool the wall time is a fraction of the
    // sequential run, and this is the one assertion that says the feature
    // was actually used (design/0016's lesson — green is not evidence).
    let source = r#"fn grind(seed: Int) -> Int {
    var i = 0;
    var acc = seed;
    while i < 300000 {
        acc = (acc + i) % 1000003;
        i = i + 1;
    }
    acc
}

fn main(io: Io) -> Result<Unit, Error> uses {Io.write, Task.spawn} {
    let total = scope {
        let a = spawn grind(seed: 1);
        let b = spawn grind(seed: 2);
        let c = spawn grind(seed: 3);
        let d = spawn grind(seed: 4);
        a.await + b.await + c.await + d.await
    };
    io.write(text: total.to_text())?;
    return Ok(unit);
}
"#;
    let dir = scratch("really_parallel");
    std::fs::write(dir.join("main.xn"), source).expect("write test program");

    let started = Instant::now();
    let parallel = execute(&dir, false, TERMINATION_TIMEOUT);
    let parallel_time = started.elapsed();

    let started = Instant::now();
    let sequential = execute(&dir, true, TERMINATION_TIMEOUT);
    let sequential_time = started.elapsed();

    assert_eq!(parallel, sequential, "same answer, both ways");
    // A deliberately loose bar: the point is that four children did not
    // queue up behind one another, not how fast the host is. Anything at or
    // below 80% of the sequential time is impossible without real overlap,
    // and leaves room for a busy CI machine.
    assert!(
        parallel_time.as_secs_f64() < sequential_time.as_secs_f64() * 0.8,
        "four children took {parallel_time:?} in parallel and {sequential_time:?} \
         sequentially — that is not concurrency"
    );
}
