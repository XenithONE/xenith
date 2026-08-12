//! Name resolution, type checking, effect checking — and the hole goals that
//! the rest of the project exists to produce.
//!
//! The checker is bidirectional: `check` pushes an expected type down the tree,
//! `synth` reads a type back up. That shape is chosen for one reason above all
//! — in checking mode the required type is available at *every* position, so a
//! hole's goal falls out of the ordinary traversal instead of needing a
//! separate machine. See `design/0006-type-checking.md`.

pub mod candidates;
pub mod check;
pub mod def;
mod exhaustive;
pub mod project;
pub mod query;
mod recursion;
pub mod ty;

pub use candidates::Candidate;
pub use check::{Analysis, Goal, Probe, analyze, analyze_at};
pub use def::{DefTable, Property};
pub use project::{ModuleUnit, ProjectAnalysis, analyze_project, analyze_project_at};
pub use query::{Producer, producers, project_producers, project_type_at, type_at};
pub use ty::{DefId, EffectSet, HoleId, Type, TypeName};
