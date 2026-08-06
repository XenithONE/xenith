//! The shared pipeline: one place that turns source into diagnostics and
//! goals, and one definition of the JSON the tools speak.
//!
//! Both frontends — the `xenith` CLI and the MCP server — call through here,
//! so a model talking to either sees identical shapes. The README once taught
//! syntax the compiler rejected; two renderers over one wire format is the
//! same class of drift, and this crate exists so it has one fewer place to
//! live.

use xenith_diag::Diagnostic;
use xenith_sema::Goal;

pub mod project;
pub mod wire;

pub struct FileAnalysis {
    /// Parse and type diagnostics together, in source order.
    pub diagnostics: Vec<Diagnostic>,
    pub goals: Vec<Goal>,
}

/// Parse and check one file's worth of source.
///
/// The checker runs even when parsing reported problems: recovery nodes are
/// ordinary nodes, and a model mid-edit still deserves type information about
/// the parts that did parse.
pub fn analyze_source(source: &str) -> FileAnalysis {
    let parsed = xenith_syntax::parse(source);
    let analysis = xenith_sema::analyze(&parsed.module);
    let mut diagnostics = parsed.diagnostics;
    diagnostics.extend(analysis.diagnostics);
    diagnostics.sort_by_key(|d| d.span.start);
    FileAnalysis {
        diagnostics,
        goals: analysis.goals,
    }
}
