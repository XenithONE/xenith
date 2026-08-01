//! Diagnostic rendering.
//!
//! Two surfaces over the same data: a caret-and-context form for a person
//! reading a terminal, and JSON for everything else. The JSON is the primary
//! one — it carries byte spans and machine-applicable fixes, so a tool or a
//! model can act on a diagnostic without parsing prose.

use std::path::{Path, PathBuf};

use serde::Serialize;
use xenith_diag::{Diagnostic, LineIndex, Severity, Span};

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
