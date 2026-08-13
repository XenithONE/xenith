//! CLI ↔ MCP equivalence over normalized diagnostics (design/0013 §1).
//!
//! The two frontends share one pipeline; these tests prove it from the
//! outside, comparing what each reports for the same fixture as
//! *normalized diagnostic objects* — file (root-relative), code, severity,
//! span, message, teaches — never as rendered bytes: each surface owns its
//! rendering, and byte-level goldens live with the surface that froze them.
//!
//! The three adjudicated fixtures (design/0012 §3): a multi-file project
//! (the vertical proof project), a single file outside any manifest, and a
//! path outside the workspace root, which the confined surface must refuse.
//! The `refused` project rides along as the diagnostics-bearing case so the
//! comparison is proven on non-empty content too.

use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::{Value, json};
use xenith_mcp::server::handle_message;

fn manifest_dir() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

/// `xenith check --json <path>` from `dir`, parsed.
fn cli_check_json(dir: &Path, path: &str) -> Value {
    let output = Command::new(env!("CARGO_BIN_EXE_xenith"))
        .current_dir(dir)
        .args(["check", "--json", path])
        .output()
        .expect("the compiler binary runs");
    let stdout = String::from_utf8(output.stdout).expect("stdout is UTF-8");
    serde_json::from_str(&stdout).expect("the CLI emits JSON")
}

/// One MCP tool call with `root` as the workspace root, parsed payload.
fn mcp_call(root: &Path, tool: &str, arguments: Value) -> (bool, Value) {
    let reply = handle_message(
        &json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": { "name": tool, "arguments": arguments },
        }),
        root,
    )
    .expect("a call has a response");
    let result = &reply["result"];
    let is_error = result["isError"].as_bool().expect("isError");
    let text = result["content"][0]["text"].as_str().expect("text");
    if is_error {
        (true, json!(text))
    } else {
        (false, serde_json::from_str(text).expect("payload is JSON"))
    }
}

/// One MCP `check` call with `root` as the workspace root, parsed payload.
fn mcp_check(root: &Path, path: &str) -> (bool, Value) {
    mcp_call(root, "check", json!({ "path": path }))
}

/// The normalized form of one diagnostic (design/0013 §1): root-relative
/// file, code, severity, span, message, teaches — and nothing rendered.
fn normalized(file: &str, diagnostic: &Value) -> Value {
    json!({
        "file": file,
        "code": diagnostic["code"],
        "severity": diagnostic["severity"],
        "span": diagnostic["span"],
        "message": diagnostic["message"],
        "teaches": diagnostic.get("teaches").cloned().unwrap_or(Value::Null),
    })
}

/// Root-relative, forward-slash spelling of a CLI-reported file path.
fn root_relative(root: &Path, file: &str) -> String {
    let root = format!("{}/", root.display().to_string().replace('\\', "/"));
    let file = file.replace('\\', "/");
    file.strip_prefix(&root).unwrap_or(&file).to_string()
}

/// Every diagnostic in a CLI report array, normalized, in report order.
fn cli_normalized(root: &Path, reports: &Value) -> Vec<Value> {
    let mut out = Vec::new();
    for report in reports.as_array().expect("reports are an array") {
        let file = root_relative(root, report["file"].as_str().expect("file"));
        for diagnostic in report["diagnostics"].as_array().expect("array") {
            out.push(normalized(&file, diagnostic));
        }
    }
    out
}

/// Every diagnostic in an MCP payload — project or single-file — normalized.
fn mcp_normalized(payload: &Value) -> Vec<Value> {
    let mut out = Vec::new();
    if let Some(files) = payload.get("files").and_then(Value::as_array) {
        for entry in files {
            let file = entry["file"].as_str().expect("file");
            for diagnostic in entry["diagnostics"].as_array().expect("array") {
                out.push(normalized(file, diagnostic));
            }
        }
    } else {
        let file = payload["file"].as_str().expect("file");
        for diagnostic in payload["diagnostics"].as_array().expect("array") {
            out.push(normalized(file, diagnostic));
        }
    }
    out
}

/// Order-insensitive comparison: the CLI reports files in load order, the
/// MCP response leads with the requested file — the *set* of normalized
/// diagnostics is what one truth means. (Within a file both are span-sorted
/// by the shared analysis, so sorting here loses nothing.)
fn sorted(mut diagnostics: Vec<Value>) -> Vec<Value> {
    diagnostics.sort_by_key(|d| d.to_string());
    diagnostics
}

fn fixture_root(name: &str) -> PathBuf {
    manifest_dir().join("tests/fixtures/projects").join(name)
}

#[test]
fn a_multi_file_project_reports_identically_on_both_surfaces() {
    let root = fixture_root("vertical");
    let cli = cli_check_json(&root, "src/main.xn");
    let (is_error, mcp) = mcp_check(&root, "src/main.xn");
    assert!(!is_error, "{mcp}");

    assert_eq!(mcp["analysis_mode"], "project");
    assert_eq!(mcp["project_root"], ".");
    // Same file set: the CLI reports every project file, so must the MCP.
    let cli_files: Vec<String> = cli
        .as_array()
        .expect("reports")
        .iter()
        .map(|r| root_relative(&root, r["file"].as_str().expect("file")))
        .collect();
    let mcp_files: Vec<&str> = mcp["files"]
        .as_array()
        .expect("files")
        .iter()
        .map(|f| f["file"].as_str().expect("file"))
        .collect();
    let mut cli_sorted = cli_files.clone();
    cli_sorted.sort();
    let mut mcp_sorted: Vec<String> = mcp_files.iter().map(|f| f.to_string()).collect();
    mcp_sorted.sort();
    assert_eq!(cli_sorted, mcp_sorted);
    // And the requested file leads the MCP ordering.
    assert_eq!(mcp_files.first().copied(), Some("src/main.xn"));

    assert_eq!(
        sorted(cli_normalized(&root, &cli)),
        sorted(mcp_normalized(&mcp)),
        "the vertical project is clean on both surfaces"
    );
}

#[test]
fn a_diagnostics_bearing_project_reports_identically_on_both_surfaces() {
    let root = fixture_root("refused");
    let cli = cli_check_json(&root, "src/main.xn");
    let (is_error, mcp) = mcp_check(&root, "src/main.xn");
    assert!(!is_error, "{mcp}");

    let cli_diagnostics = sorted(cli_normalized(&root, &cli));
    let mcp_diagnostics = sorted(mcp_normalized(&mcp));
    assert!(
        !cli_diagnostics.is_empty(),
        "the fixture must carry diagnostics for the comparison to bite"
    );
    assert_eq!(cli_diagnostics, mcp_diagnostics);
}

#[test]
fn a_single_file_outside_any_manifest_reports_identically_on_both_surfaces() {
    let root = manifest_dir().join("tests/fixtures/diag");
    let cli = cli_check_json(&root, "xn3001_mismatch.xn");
    let (is_error, mcp) = mcp_check(&root, "xn3001_mismatch.xn");
    assert!(!is_error, "{mcp}");

    assert_eq!(mcp["analysis_mode"], "single_file");
    let cli_diagnostics = sorted(cli_normalized(&root, &cli));
    let mcp_diagnostics = sorted(mcp_normalized(&mcp));
    assert!(!cli_diagnostics.is_empty(), "the fixture mistypes");
    assert_eq!(cli_diagnostics, mcp_diagnostics);
}

// ------------------------------------------- goals and query (design/0013 §1)
//
// `check` and `run` were project-aware from the start; `goals` and
// `query type-at` / `query producers` analysed the named file alone, so
// inside a project their answers degraded — a cross-module type read as
// `<unknown>` and a qualified type had no producers at all. They now walk the
// same ProjectSnapshot pipeline, and these tests hold the two surfaces to the
// same answer.
//
// Compared field by field, minus the two each surface legitimately spells its
// own way: `file` (the name the caller used) and `project_root` (root-relative
// for the confined server, as-given for the unconfined CLI). `analysis_mode`
// is compared — that a project ran is exactly the claim under test.

/// `xenith <args…>` from `dir`, stdout parsed as JSON.
fn cli_json(dir: &Path, args: &[&str]) -> Value {
    let output = Command::new(env!("CARGO_BIN_EXE_xenith"))
        .current_dir(dir)
        .args(args)
        .output()
        .expect("the compiler binary runs");
    let stdout = String::from_utf8(output.stdout).expect("stdout is UTF-8");
    serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("the CLI emits JSON ({e}); stdout was:\n{stdout}"))
}

/// Drop the two path-spelling fields, at the top level and inside each entry
/// of an array — everything left must match between the surfaces.
fn scrub(value: &Value) -> Value {
    let strip = |value: &Value| -> Value {
        let mut value = value.clone();
        if let Some(map) = value.as_object_mut() {
            map.remove("file");
            map.remove("project_root");
        }
        value
    };
    match value {
        Value::Array(entries) => Value::Array(entries.iter().map(strip).collect()),
        other => strip(other),
    }
}

#[test]
fn project_goals_answer_identically_on_both_surfaces() {
    // A hole whose expected type lives in another module: single-file
    // analysis renders it `<unknown>` and offers no candidates, so this
    // fixture only agrees when both surfaces really ran the project.
    let root = fixture_root("holed");
    let cli = cli_json(&root, &["goals", "--json", "src/main.xn"]);
    let (is_error, mcp) = mcp_call(&root, "goals", json!({ "path": "src/main.xn" }));
    assert!(!is_error, "{mcp}");

    let entries = cli.as_array().expect("goals are an array");
    assert_eq!(entries.len(), 1, "one hole in the fixture: {cli}");
    assert_eq!(entries[0]["analysis_mode"], "project");
    assert_eq!(
        entries[0]["expected"], "game.player.Player",
        "the cross-module type must resolve: {cli}"
    );
    assert_eq!(
        entries[0]["candidates"][0]["expression"], "game.player.fresh(name: ??)",
        "candidates come from the other module too: {cli}"
    );
    assert_eq!(scrub(&cli), scrub(&mcp));
}

#[test]
fn project_goals_agree_on_a_project_with_no_holes() {
    let root = fixture_root("vertical");
    let cli = cli_json(&root, &["goals", "--json", "src/main.xn"]);
    let (is_error, mcp) = mcp_call(&root, "goals", json!({ "path": "src/main.xn" }));
    assert!(!is_error, "{mcp}");
    assert_eq!(cli.as_array().expect("array").len(), 0, "{cli}");
    assert_eq!(scrub(&cli), scrub(&mcp));
}

#[test]
fn project_type_at_answers_identically_on_both_surfaces() {
    // `src/main.xn:5:17` is the `game.player.Player` struct literal; in
    // single-file mode the type reads `<unknown>` and the enclosing function
    // is unqualified.
    let root = fixture_root("vertical");
    let cli = cli_json(
        &root,
        &["query", "type-at", "src/main.xn", "--at", "5:17", "--json"],
    );
    let (is_error, mcp) = mcp_call(
        &root,
        "type_at",
        json!({ "path": "src/main.xn", "line": 5, "column": 17 }),
    );
    assert!(!is_error, "{mcp}");

    assert_eq!(cli["analysis_mode"], "project");
    assert_eq!(cli["type"], "game.player.Player", "{cli}");
    assert_eq!(cli["enclosing_function"], "main.main", "{cli}");
    assert_eq!(scrub(&cli), scrub(&mcp));
}

#[test]
fn project_producers_answer_identically_on_both_surfaces() {
    // A qualified type has no producers at all in single-file mode — the
    // type does not even resolve, which is an error rather than an answer.
    let root = fixture_root("vertical");
    let cli = cli_json(
        &root,
        &[
            "query",
            "producers",
            "src/main.xn",
            "game.player.Player",
            "--json",
        ],
    );
    let (is_error, mcp) = mcp_call(
        &root,
        "producers",
        json!({ "path": "src/main.xn", "type": "game.player.Player" }),
    );
    assert!(!is_error, "{mcp}");

    let entries = cli.as_array().expect("producers are an array");
    assert!(!entries.is_empty(), "{cli}");
    assert_eq!(entries[0]["analysis_mode"], "project");
    assert!(
        entries.iter().any(|p| p["symbol"] == "game.player.award"
            || p["signature"]
                .as_str()
                .is_some_and(|s| s.contains("game.player.award"))),
        "the other module's producer must be listed: {cli}"
    );
    assert_eq!(scrub(&cli), scrub(&mcp));
}

#[test]
fn a_file_outside_any_project_still_answers_in_single_file_mode() {
    // The other half of the contract: no manifest, no project mode, and the
    // response says so rather than claiming one.
    let root = manifest_dir().join("tests/fixtures/diag");
    let cli = cli_json(&root, &["goals", "--json", "xn3001_mismatch.xn"]);
    let (is_error, mcp) = mcp_call(&root, "goals", json!({ "path": "xn3001_mismatch.xn" }));
    assert!(!is_error, "{mcp}");
    assert_eq!(scrub(&cli), scrub(&mcp));

    let cli = cli_json(
        &root,
        &[
            "query",
            "type-at",
            "xn3001_mismatch.xn",
            "--at",
            "2:5",
            "--json",
        ],
    );
    assert_eq!(cli["analysis_mode"], "single_file", "{cli}");
    let (is_error, mcp) = mcp_call(
        &root,
        "type_at",
        json!({ "path": "xn3001_mismatch.xn", "line": 2, "column": 5 }),
    );
    assert!(!is_error, "{mcp}");
    assert_eq!(scrub(&cli), scrub(&mcp));
}

#[test]
fn a_path_outside_the_workspace_root_is_refused_by_the_confined_surface() {
    // The CLI, unconfined, checks the file; the MCP server, confined to the
    // vertical project, must refuse the very same path — the boundary is
    // the server's, not the compiler's.
    let root = fixture_root("vertical");
    let outside = manifest_dir()
        .join("tests/fixtures/diag/xn3001_mismatch.xn")
        .display()
        .to_string();

    let cli = cli_check_json(manifest_dir(), "tests/fixtures/diag/xn3001_mismatch.xn");
    assert!(
        !cli_normalized(manifest_dir(), &cli).is_empty(),
        "the CLI reads it fine"
    );

    let (is_error, payload) = mcp_check(&root, &outside);
    assert!(is_error, "{payload}");
    assert!(
        payload
            .as_str()
            .expect("an error payload is text")
            .contains("outside the workspace root"),
        "{payload}"
    );
}
