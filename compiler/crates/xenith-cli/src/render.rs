//! Diagnostic rendering.
//!
//! Two surfaces over the same data: a caret-and-context form for a person
//! reading a terminal, and JSON for everything else. The JSON is the primary
//! one — it carries byte spans and machine-applicable fixes, so a tool or a
//! model can act on a diagnostic without parsing prose.

use std::path::{Path, PathBuf};

use serde::Serialize;
use xenith_diag::{Diagnostic, LineIndex, Severity, Span};
use xenith_sema::Goal;

/// One file's worth of results, ready to serialise.
#[derive(Serialize)]
struct FileReport<'a> {
    file: String,
    diagnostics: Vec<Entry<'a>>,
}

#[derive(Serialize)]
struct Entry<'a> {
    #[serde(flatten)]
    diagnostic: &'a Diagnostic,
    /// Line and column of the span's start, one-based. Byte offsets stay
    /// authoritative; this is here so a consumer does not have to recompute it.
    line: u32,
    column: u32,
}

pub fn diagnostics_json(findings: &[(PathBuf, String, Vec<Diagnostic>)]) -> String {
    let reports: Vec<FileReport> = findings
        .iter()
        .map(|(path, source, diagnostics)| {
            let index = LineIndex::new(source);
            FileReport {
                file: path.display().to_string(),
                diagnostics: diagnostics
                    .iter()
                    .map(|diagnostic| {
                        let at = index.line_col(source, diagnostic.span.start);
                        Entry {
                            diagnostic,
                            line: at.line,
                            column: at.column,
                        }
                    })
                    .collect(),
            }
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

/// One goal as JSON. `candidates` is present and empty on purpose: the shape
/// is the interface, and ranking is an accelerator that lands later
/// (design/0006 §5 — "the expected type alone is the thesis").
pub fn goals_json(reports: &[(PathBuf, String, Vec<Goal>, usize)]) -> String {
    let rendered: Vec<serde_json::Value> = reports
        .iter()
        .flat_map(|(path, source, goals, _)| {
            let index = LineIndex::new(source);
            goals
                .iter()
                .map(|goal| {
                    let at = index.line_col(source, goal.span.start);
                    serde_json::json!({
                        "file": path.display().to_string(),
                        "line": at.line,
                        "column": at.column,
                        "kind": goal.kind,
                        "hole": goal.name,
                        "expected": goal.expected,
                        "enclosing_function": goal.enclosing_function,
                        "in_scope": goal
                            .in_scope
                            .iter()
                            .map(|(name, ty)| serde_json::json!({ "name": name, "type": ty }))
                            .collect::<Vec<_>>(),
                        "allowed_effects": goal.allowed_effects,
                        "candidates": goal
                            .candidates
                            .iter()
                            .map(|c| serde_json::json!({
                                "expression": c.expression,
                                "complete": c.complete,
                                "requires_effects": c.requires_effects,
                            }))
                            .collect::<Vec<_>>(),
                        "blocked": goal.blocked,
                    })
                })
                .collect::<Vec<_>>()
        })
        .collect();
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
