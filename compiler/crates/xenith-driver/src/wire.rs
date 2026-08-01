//! The JSON the tools speak — the single definition.
//!
//! Key order is deterministic (`serde_json::Value` maps are sorted), byte
//! spans stay authoritative, and line/column are included so consumers do not
//! recompute them. Shapes here are an interface models learn; change them the
//! way diagnostic codes change, which is to say almost never.

use serde_json::{Value, json};
use xenith_diag::{Diagnostic, LineIndex};
use xenith_sema::{Goal, Probe, Producer};

/// One file's diagnostics: `{ file, diagnostics: [ { …, line, column } ] }`.
pub fn file_diagnostics(file: &str, source: &str, diagnostics: &[Diagnostic]) -> Value {
    let index = LineIndex::new(source);
    let entries: Vec<Value> = diagnostics
        .iter()
        .map(|diagnostic| {
            let mut value = serde_json::to_value(diagnostic).unwrap_or_else(|_| json!({}));
            let at = index.line_col(source, diagnostic.span.start);
            if let Some(map) = value.as_object_mut() {
                map.insert("line".into(), json!(at.line));
                map.insert("column".into(), json!(at.column));
            }
            value
        })
        .collect();
    json!({ "file": file, "diagnostics": entries })
}

/// One file's goals, as an array in source order.
pub fn goals(file: &str, source: &str, goals: &[Goal]) -> Value {
    let index = LineIndex::new(source);
    let entries: Vec<Value> = goals
        .iter()
        .map(|goal| {
            let at = index.line_col(source, goal.span.start);
            json!({
                "file": file,
                "line": at.line,
                "column": at.column,
                "kind": goal.kind,
                "hole": goal.name,
                "expected": goal.expected,
                "enclosing_function": goal.enclosing_function,
                "in_scope": goal
                    .in_scope
                    .iter()
                    .map(|(name, ty)| json!({ "name": name, "type": ty }))
                    .collect::<Vec<_>>(),
                "allowed_effects": goal.allowed_effects,
                "candidates": goal
                    .candidates
                    .iter()
                    .map(|c| json!({
                        "expression": c.expression,
                        "complete": c.complete,
                        "requires_effects": c.requires_effects,
                    }))
                    .collect::<Vec<_>>(),
                "blocked": goal.blocked,
            })
        })
        .collect();
    Value::Array(entries)
}

/// A `type-at` answer.
pub fn probe(file: &str, line: u32, column: u32, probe: &Probe) -> Value {
    json!({
        "file": file,
        "line": line,
        "column": column,
        "type": probe.ty,
        "enclosing_function": probe.enclosing_function,
        "in_scope": probe
            .in_scope
            .iter()
            .map(|(name, ty)| json!({ "name": name, "type": ty }))
            .collect::<Vec<_>>(),
        "allowed_effects": probe.allowed_effects,
    })
}

/// A `producers` answer, as an array.
pub fn producers(found: &[Producer]) -> Value {
    let entries: Vec<Value> = found
        .iter()
        .map(|p| {
            json!({
                "kind": p.kind,
                "symbol": p.symbol,
                "signature": p.signature,
                "effects": p.effects,
            })
        })
        .collect();
    Value::Array(entries)
}

/// One-based line and character column to a byte offset. `None` when the
/// position is outside the file.
pub fn position_to_offset(source: &str, index: &LineIndex, line: u32, column: u32) -> Option<u32> {
    let start = index.line_start(line)?;
    let text = index.line_text(source, line)?;
    let mut seen = 0u32;
    for (byte_offset, _) in text.char_indices() {
        seen += 1;
        if seen == column {
            return Some(start + byte_offset as u32);
        }
    }
    // One past the last character addresses the end of the line.
    if column == seen + 1 {
        Some(start + text.len() as u32)
    } else {
        None
    }
}
