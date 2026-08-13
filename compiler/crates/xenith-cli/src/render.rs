//! Diagnostic rendering.
//!
//! Two surfaces over the same data: a caret-and-context form for a person
//! reading a terminal, and JSON for everything else. The JSON is the primary
//! one — it carries byte spans and machine-applicable fixes, so a tool or a
//! model can act on a diagnostic without parsing prose.

use std::path::{Path, PathBuf};

use xenith_diag::{Diagnostic, LineIndex, Severity, Span, Teach, TeachKind};
use xenith_sema::Goal;

// The JSON shapes live in xenith-driver's `wire` module, shared with the MCP
// server — one wire format, two frontends. This module only renders for
// humans and stitches per-file wire values into whole responses.

pub fn diagnostics_json(findings: &[(PathBuf, String, Vec<Diagnostic>)], teaching: bool) -> String {
    let reports: Vec<serde_json::Value> = findings
        .iter()
        .map(|(path, source, diagnostics)| {
            xenith_driver::wire::file_diagnostics(
                &path.display().to_string(),
                source,
                diagnostics,
                teaching,
            )
        })
        .collect();
    serde_json::to_string_pretty(&reports).unwrap_or_else(|_| "[]".to_string())
}

pub fn diagnostic(path: &Path, source: &str, index: &LineIndex, diagnostic: &Diagnostic) -> String {
    let at = index.line_col(source, diagnostic.span.start);
    let severity = match diagnostic.severity {
        Severity::Error => "error",
        Severity::Warning => "warning",
    };

    let mut out = format!(
        "{}:{}:{}: {severity}[{}]: {}\n",
        path.display(),
        at.line,
        at.column,
        diagnostic.code.id(),
        diagnostic.message
    );

    // Teaches sit directly under the primary message, not as a tail note —
    // a tail is the ignored position (design/0009 §3).
    for entry in &diagnostic.teaches {
        out.push_str(&teach(entry));
    }

    if let Some(line_text) = index.line_text(source, at.line) {
        let number = at.line.to_string();
        let gutter = " ".repeat(number.len());
        out.push_str(&format!("{number} | {line_text}\n"));
        out.push_str(&format!(
            "{gutter} | {}\n",
            caret(line_text, index, source, diagnostic.span, at.column)
        ));
    }

    if let Some(fix) = &diagnostic.fix {
        out.push_str(&format!("  fix: {}\n", fix.description));
    }
    out.push_str(&format!(
        "  run `xenith explain {}` for the rule\n",
        diagnostic.code.id()
    ));

    out
}

/// One file's goals as `xenith goals` reports them, with what it took to
/// produce them: `project_root` is `Some` exactly when the file was analysed
/// as part of a project, which is what the entry's `analysis_mode` reports.
pub struct GoalReport {
    /// The path as this run spells it — for a project file, `<root>/src/…`.
    pub path: PathBuf,
    pub source: String,
    pub goals: Vec<Goal>,
    /// How many diagnostics the same analysis produced, for the note that
    /// sends the reader to `xenith check`.
    pub problems: usize,
    pub project_root: Option<String>,
}

/// The flat array of goal entries, one per hole, across every report.
///
/// Every entry carries the `analysis_mode` that actually ran, the way the
/// MCP responses do — a response says what it *did*, never only what the
/// tool could do (design/0013 §1).
pub fn goals_json(reports: &[GoalReport]) -> String {
    let mut rendered: Vec<serde_json::Value> = Vec::new();
    for report in reports {
        let mut value = xenith_driver::wire::goals(
            &report.path.display().to_string(),
            &report.source,
            &report.goals,
        );
        let root = report.project_root.as_deref();
        let mode = if root.is_some() {
            "project"
        } else {
            "single_file"
        };
        xenith_driver::wire::stamp_mode(&mut value, mode, root);
        if let serde_json::Value::Array(entries) = value {
            rendered.extend(entries);
        }
    }
    serde_json::to_string_pretty(&rendered).unwrap_or_else(|_| "[]".to_string())
}

pub fn goal(path: &Path, source: &str, index: &LineIndex, goal: &Goal) -> String {
    let at = index.line_col(source, goal.span.start);
    let shown = goal
        .name
        .as_ref()
        .map(|n| format!("??{n}"))
        .unwrap_or_else(|| "??".to_string());

    let mut out = format!(
        "{}:{}:{} — hole {shown} in {}\n",
        path.display(),
        at.line,
        at.column,
        goal.enclosing_function
    );
    out.push_str(&format!("  expected: {}\n", goal.expected));
    if goal.in_scope.is_empty() {
        out.push_str("  in scope: (nothing)\n");
    } else {
        let listed: Vec<String> = goal
            .in_scope
            .iter()
            .map(|(name, ty)| format!("{name}: {ty}"))
            .collect();
        out.push_str(&format!("  in scope: {}\n", listed.join(", ")));
    }
    if goal.allowed_effects.is_empty() {
        out.push_str("  effects:  none permitted\n");
    } else {
        out.push_str(&format!(
            "  effects:  {}\n",
            goal.allowed_effects.join(", ")
        ));
    }
    if !goal.candidates.is_empty() {
        out.push_str("  candidates:\n");
        for (index, candidate) in goal.candidates.iter().enumerate() {
            let effects = if candidate.requires_effects.is_empty() {
                String::new()
            } else {
                format!("  — uses {{{}}}", candidate.requires_effects.join(", "))
            };
            out.push_str(&format!(
                "    {}. {}{effects}\n",
                index + 1,
                candidate.expression
            ));
        }
    }
    for blocked in &goal.blocked {
        out.push_str(&format!("  blocked:  {blocked}\n"));
    }
    out
}

/// One teaching block: knowledge, no directions — a pointer at a command is
/// an invitation to a channel the measurements say is dead (design/0009 §3).
fn teach(teach: &Teach) -> String {
    let mut out = String::new();
    match teach.kind {
        TeachKind::CallSignature => {
            for item in &teach.items {
                out.push_str(&format!("  call shape: {}\n", item.signature));
            }
        }
        TeachKind::AvailableMethods => {
            if teach.truncated {
                out.push_str(&format!(
                    "  methods of {} ({} of {}):\n",
                    teach.type_name,
                    teach.items.len(),
                    teach.total_items
                ));
            } else {
                out.push_str(&format!("  methods of {}:\n", teach.type_name));
            }
            for item in &teach.items {
                out.push_str(&format!("      {}\n", item.signature));
            }
        }
        TeachKind::UseCandidates => {
            out.push_str(&format!(
                "  `{}` is pub in more than one module:\n",
                teach.type_name
            ));
            for item in &teach.items {
                out.push_str(&format!("      {}\n", item.signature));
            }
        }
        TeachKind::ModuleCall => {
            // The rewrite bridge (design/0012 §1): candidates whole, the
            // receiver-taking rewrite right under its signature, and any
            // omission stated as a count rather than a cut signature.
            if teach.truncated {
                out.push_str(&format!(
                    "  module functions taking {} ({} of {}):\n",
                    teach.type_name,
                    teach.items.len(),
                    teach.total_items
                ));
            } else {
                out.push_str(&format!("  module functions taking {}:\n", teach.type_name));
            }
            for item in &teach.items {
                out.push_str(&format!("      {}\n", item.signature));
                if let Some(rewrite) = &item.rewrite {
                    out.push_str(&format!("      rewrite: {rewrite}\n"));
                }
            }
        }
    }
    out
}

/// The underline row: spaces up to the span, then carets across it.
///
/// Widths are counted in characters rather than bytes so that a caret under
/// multi-byte text still lands in the right place.
fn caret(line_text: &str, index: &LineIndex, source: &str, span: Span, column: u32) -> String {
    let leading = " ".repeat(column.saturating_sub(1) as usize);

    let end = index.line_col(source, span.end);
    let width = if end.line == index.line_col(source, span.start).line {
        end.column.saturating_sub(column).max(1)
    } else {
        // The span runs past this line; underline the rest of it.
        (line_text.chars().count() as u32)
            .saturating_sub(column - 1)
            .max(1)
    };

    format!("{leading}{}", "^".repeat(width as usize))
}
