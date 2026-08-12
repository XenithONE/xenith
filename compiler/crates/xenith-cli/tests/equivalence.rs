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

/// One MCP `check` call with `root` as the workspace root, parsed payload.
fn mcp_check(root: &Path, path: &str) -> (bool, Value) {
    let reply = handle_message(
        &json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": { "name": "check", "arguments": { "path": path } },
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
