//! Execution: a tree-walking interpreter over the checked syntax tree.
//!
//! Walking the tree is deliberate. Peak performance is a stated non-goal
//! (design/0001 §3); what the benchmark and the examples need is execution
//! that is **correct and deterministic** — strict left-to-right evaluation,
//! trapping overflow, no undefined behaviour anywhere (design/0003). A
//! bytecode VM is an optimisation for later, behind the same `run` interface.
//!
//! Two rules connect execution to the rest of the project:
//!
//! - A program with diagnostics is refused, but a program with **holes** runs.
//!   Reaching a hole is a trap that names it and points at `xenith goals` —
//!   the workflow is *fill the next hole*, and running the program tells you
//!   which one that is.
//! - Capabilities are ordinary values at runtime too. `main` receives them;
//!   nothing else can conjure them.

pub mod interp;

pub use interp::{Outcome, run};
