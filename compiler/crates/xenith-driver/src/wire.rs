//! The JSON the tools speak — the single definition.
//!
//! Key order is deterministic (`serde_json::Value` maps are sorted), byte
//! spans stay authoritative, and line/column are included so consumers do not
//! recompute them. Shapes here are an interface models learn; change them the
//! way diagnostic codes change, which is to say almost never.
//!
//! Every object this module emits carries `schema_version` so a consumer can
//! tell which shape it is reading. Responses that are arrays version each
//! entry — the entries are the objects a consumer holds onto, and the CLI
//! already flattens arrays across files, so a version on the array itself
//! would not survive.

use serde_json::{Value, json};
use xenith_diag::{Diagnostic, LineIndex};
use xenith_sema::{Goal, Probe, Producer};

/// The version of the wire shapes. Bump only when a shape changes
/// incompatibly, which should be as rare as renumbering a diagnostic code.
pub const SCHEMA_VERSION: u32 = 1;

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
    json!({ "schema_version": SCHEMA_VERSION, "file": file, "diagnostics": entries })
}

/// One file's goals, as an array in source order.
pub fn goals(file: &str, source: &str, goals: &[Goal]) -> Value {
    let index = LineIndex::new(source);
    let entries: Vec<Value> = goals
        .iter()
        .map(|goal| {
            let at = index.line_col(source, goal.span.start);
            json!({
                "schema_version": SCHEMA_VERSION,
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
        "schema_version": SCHEMA_VERSION,
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
                "schema_version": SCHEMA_VERSION,
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

#[cfg(test)]
mod tests {
    //! Every wire shape carries `schema_version: 1`. These tests go through
    //! the real pipeline — parse, analyze, query — so they also pin the keys
    //! consumers index by, not just the version stamp.

    use super::*;

    #[test]
    fn file_diagnostics_carries_the_schema_version() {
        let source = "fn main() -> Int {\n    true\n}\n";
        let analysis = crate::analyze_source(source);
        assert!(!analysis.diagnostics.is_empty(), "the fixture must mistype");
        let value = file_diagnostics("bad.xn", source, &analysis.diagnostics);
        assert_eq!(value["schema_version"], SCHEMA_VERSION);
        assert_eq!(value["file"], "bad.xn");
        assert!(value["diagnostics"][0]["line"].is_u64(), "{value}");
    }

    #[test]
    fn every_goal_entry_carries_the_schema_version() {
        let source = "fn f() -> Int {\n    ??body\n}\n";
        let analysis = crate::analyze_source(source);
        let value = goals("holes.xn", source, &analysis.goals);
        let entries = value.as_array().expect("goals are an array");
        assert!(!entries.is_empty(), "the fixture must hold a hole");
        for entry in entries {
            assert_eq!(entry["schema_version"], SCHEMA_VERSION, "{entry}");
        }
        assert_eq!(entries[0]["hole"], "body");
    }

    #[test]
    fn a_probe_carries_the_schema_version() {
        let source = "fn f() -> Int {\n    let total = 1 + 2;\n    total\n}\n";
        let index = LineIndex::new(source);
        let offset =
            position_to_offset(source, &index, 2, 9).expect("the position is inside the file");
        let parsed = xenith_syntax::parse(source);
        let found = xenith_sema::type_at(&parsed.module, offset).expect("a binding sits there");
        let value = probe("probe.xn", 2, 9, &found);
        assert_eq!(value["schema_version"], SCHEMA_VERSION);
        assert_eq!(value["type"], "Int");
    }

    #[test]
    fn every_producer_entry_carries_the_schema_version() {
        let source = "struct Player {\n    name: String,\n}\n\nenum ScoreError {\n    Overflow,\n}\n\n\
             fn try_award(player: Player, points: Int) -> Result<Player, ScoreError> {\n    ??x\n}\n";
        let parsed = xenith_syntax::parse(source);
        let found = xenith_sema::producers(&parsed.module, "Result<Player, ScoreError>")
            .expect("the type is known");
        let value = producers(&found);
        let entries = value.as_array().expect("producers are an array");
        assert!(!entries.is_empty(), "try_award and the variants produce it");
        for entry in entries {
            assert_eq!(entry["schema_version"], SCHEMA_VERSION, "{entry}");
        }
    }
}
