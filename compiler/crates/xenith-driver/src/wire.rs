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
//!
//! **Tolerant-reader contract**: a consumer must ignore fields it does not
//! recognise. New optional fields — `teaches` on a diagnostic, `features` on
//! a response — arrive under the same `schema_version`, because adding a
//! field a reader may skip is not an incompatible change. `features` names
//! the additive capabilities present, so a consumer can tell "an old
//! compiler without teaching" apart from "teaching supported, nothing to
//! teach here".

use serde_json::{Value, json};
use xenith_diag::{Diagnostic, LineIndex};
use xenith_sema::{Goal, Probe, Producer};

/// The version of the wire shapes. Bump only when a shape changes
/// incompatibly, which should be as rare as renumbering a diagnostic code.
pub const SCHEMA_VERSION: u32 = 1;

/// Additive capabilities this compiler's wire output carries, named so a
/// consumer can distinguish absence of support from absence of content.
/// `project_mode_v1` is an advertisement, not a proof: the mode a response
/// actually ran under is its `analysis_mode` field (design/0013 §1).
pub const FEATURES: &[&str] = &[
    "diagnostic_teaching_v1",
    "module_call_teach_v1",
    "project_mode_v1",
];

/// One file's diagnostics: `{ file, diagnostics: [ { …, line, column } ] }`.
///
/// `teaching` declares whether diagnostic teaching was enabled for this run:
/// when it was, the response carries `features` even if nothing taught —
/// "supported but empty" and "not supported" must read differently. With
/// teaching off the response reproduces the pre-teaching shape exactly.
pub fn file_diagnostics(
    file: &str,
    source: &str,
    diagnostics: &[Diagnostic],
    teaching: bool,
) -> Value {
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
    let mut report = json!({
        "schema_version": SCHEMA_VERSION,
        "file": file,
        "diagnostics": entries,
    });
    if teaching {
        report
            .as_object_mut()
            .expect("a report is an object")
            .insert("features".into(), json!(FEATURES));
    }
    report
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

/// The single-file `check` response a mode-aware tool returns: the
/// [`file_diagnostics`] shape plus the honest `analysis_mode`. The CLI's
/// frozen array shape predates modes and stays on `file_diagnostics`.
pub fn single_file_check(file: &str, source: &str, diagnostics: &[Diagnostic]) -> Value {
    let mut report = file_diagnostics(file, source, diagnostics, true);
    report
        .as_object_mut()
        .expect("a report is an object")
        .insert("analysis_mode".into(), json!("single_file"));
    report
}

/// One project file a project-mode response reports on.
pub struct ProjectFileReport<'a> {
    /// Root-relative path, forward slashes ("src/main.xn").
    pub file: String,
    pub source: &'a str,
    pub diagnostics: &'a [Diagnostic],
}

/// The project-mode `check` response: every file, the requested one first,
/// the rest in path-lexicographic order — cascade mitigation by priority,
/// never by truncation (design/0013 §1). `project_root` is relative to the
/// workspace root the server was confined to.
pub fn project_check(
    project_root: &str,
    requested: Option<&str>,
    files: &[ProjectFileReport],
) -> Value {
    let entries: Vec<Value> = ordered(files, requested)
        .into_iter()
        .map(|report| {
            let per_file = file_diagnostics(&report.file, report.source, report.diagnostics, false);
            json!({
                "file": report.file,
                "diagnostics": per_file["diagnostics"],
            })
        })
        .collect();
    json!({
        "schema_version": SCHEMA_VERSION,
        "analysis_mode": "project",
        "project_root": project_root,
        "requested": requested,
        "features": FEATURES,
        "files": entries,
    })
}

/// The project-mode `goals` response: still one flat array — the frozen
/// single-file shape — with each entry naming its root-relative file and
/// carrying the mode, ordered like project diagnostics are.
pub fn project_goals(
    project_root: &str,
    requested: Option<&str>,
    files: &[(String, &str, &[Goal])],
) -> Value {
    let reports: Vec<ProjectFileReport> = files
        .iter()
        .map(|(file, source, _)| ProjectFileReport {
            file: file.clone(),
            source,
            diagnostics: &[],
        })
        .collect();
    let mut entries: Vec<Value> = Vec::new();
    for report in ordered(&reports, requested) {
        let (_, source, file_goals) = files
            .iter()
            .find(|(file, _, _)| *file == report.file)
            .expect("ordered() permutes the same files");
        let mut rendered = goals(&report.file, source, file_goals);
        stamp_mode(&mut rendered, "project", Some(project_root));
        if let Value::Array(mut values) = rendered {
            entries.append(&mut values);
        }
    }
    Value::Array(entries)
}

/// Stamp the actual `analysis_mode` — and, in project mode, the root-relative
/// `project_root` — onto a response: objects take the fields directly, array
/// entries each take their own. Additive under the tolerant-reader contract.
pub fn stamp_mode(value: &mut Value, analysis_mode: &str, project_root: Option<&str>) {
    let stamp_object = |map: &mut serde_json::Map<String, Value>| {
        map.insert("analysis_mode".into(), json!(analysis_mode));
        if let Some(root) = project_root {
            map.insert("project_root".into(), json!(root));
        }
    };
    match value {
        Value::Object(map) => stamp_object(map),
        Value::Array(entries) => {
            for entry in entries {
                if let Value::Object(map) = entry {
                    stamp_object(map);
                }
            }
        }
        _ => {}
    }
}

/// The requested file first, everything else path-lexicographic.
fn ordered<'a, 'b>(
    files: &'a [ProjectFileReport<'b>],
    requested: Option<&str>,
) -> Vec<&'a ProjectFileReport<'b>> {
    let mut sorted: Vec<&ProjectFileReport> = files.iter().collect();
    sorted.sort_by(|a, b| {
        let a_requested = Some(a.file.as_str()) == requested;
        let b_requested = Some(b.file.as_str()) == requested;
        b_requested
            .cmp(&a_requested)
            .then_with(|| a.file.cmp(&b.file))
    });
    sorted
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
        let value = file_diagnostics("bad.xn", source, &analysis.diagnostics, true);
        assert_eq!(value["schema_version"], SCHEMA_VERSION);
        assert_eq!(value["file"], "bad.xn");
        assert!(value["diagnostics"][0]["line"].is_u64(), "{value}");
    }

    #[test]
    fn teaching_runs_declare_the_feature_and_off_runs_reproduce_the_old_shape() {
        let source = "fn main() -> Int {\n    true\n}\n";
        let analysis = crate::analyze_source(source);
        // On, with nothing taught: the feature is still declared, so a
        // consumer can tell "supported but empty" from "old compiler".
        let on = file_diagnostics("t.xn", source, &analysis.diagnostics, true);
        assert_eq!(on["features"][0], "diagnostic_teaching_v1");
        assert_eq!(on["features"][1], "module_call_teach_v1");
        let off = file_diagnostics("t.xn", source, &analysis.diagnostics, false);
        assert!(off.get("features").is_none());
    }

    #[test]
    fn an_old_shape_consumer_parses_taught_output() {
        // The tolerant-reader contract, exercised: a consumer whose shape
        // predates `teaches` must read new output by ignoring it.
        #[derive(serde::Deserialize)]
        struct OldSpan {
            start: u32,
            end: u32,
        }
        #[derive(serde::Deserialize)]
        #[allow(dead_code)]
        struct OldDiagnostic {
            code: String,
            severity: String,
            span: OldSpan,
            message: String,
        }

        let source = "fn f(xs: List<Int>) -> Int {\n    xs.size()\n}\n";
        let analysis = crate::analyze_source(source);
        let value = file_diagnostics("t.xn", source, &analysis.diagnostics, true);
        assert!(
            value["diagnostics"][0]["teaches"].is_array(),
            "the new field must be present for the test to prove anything: {value}"
        );
        let old: OldDiagnostic = serde_json::from_value(value["diagnostics"][0].clone())
            .expect("an old-shape consumer still parses");
        assert_eq!(old.code, "XN2003");
        assert_eq!(old.span.start, 36);
        assert_eq!(old.span.end, 40);
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
