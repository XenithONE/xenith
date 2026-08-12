use std::path::{Path, PathBuf};

use serde_json::{Value, json};
use xenith_mcp::server::{ServerOptions, handle_message, handle_message_with};

fn request(id: u64, method: &str, params: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params })
}

/// `handle_message` with the temp dir as the workspace root — where all the
/// scratch files live, so the confinement check stays out of the way of
/// everything that is not deliberately testing it.
fn handle(message: &Value) -> Option<Value> {
    handle_message(message, &std::env::temp_dir())
}

/// The text payload of a successful or failed tool call, under `root`.
fn call_in(root: &Path, name: &str, arguments: Value) -> (bool, String) {
    let reply = handle_message(
        &request(
            7,
            "tools/call",
            json!({
                "name": name,
                "arguments": arguments,
            }),
        ),
        root,
    )
    .expect("a call has a response");
    let result = &reply["result"];
    let is_error = result["isError"].as_bool().expect("isError");
    let text = result["content"][0]["text"]
        .as_str()
        .expect("text")
        .to_string();
    (is_error, text)
}

/// The text payload of a successful or failed tool call.
fn call(name: &str, arguments: Value) -> (bool, String) {
    call_in(&std::env::temp_dir(), name, arguments)
}

/// A scratch file the tools can read.
fn scratch(name: &str, source: &str) -> String {
    let path = std::env::temp_dir().join(format!("xenith-mcp-test-{name}.xn"));
    std::fs::write(&path, source).expect("writable temp dir");
    path.display().to_string()
}

/// A scratch directory to serve as a workspace root of its own.
fn scratch_root(name: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!("xenith-mcp-test-root-{name}"));
    std::fs::create_dir_all(&root).expect("writable temp dir");
    root
}

// ------------------------------------------------------------------ handshake

#[test]
fn initialize_echoes_a_known_protocol_version() {
    let reply = handle(&request(
        1,
        "initialize",
        json!({ "protocolVersion": "2025-06-18", "capabilities": {} }),
    ))
    .expect("response");
    assert_eq!(reply["result"]["protocolVersion"], "2025-06-18");
    assert_eq!(reply["result"]["serverInfo"]["name"], "xenith-mcp");
    assert_eq!(reply["id"], 1);
}

#[test]
fn an_unknown_protocol_version_gets_the_oldest_supported_one() {
    let reply = handle(&request(
        1,
        "initialize",
        json!({ "protocolVersion": "1999-01-01" }),
    ))
    .expect("response");
    assert_eq!(reply["result"]["protocolVersion"], "2024-11-05");
}

#[test]
fn notifications_get_no_reply() {
    let notification = json!({
        "jsonrpc": "2.0",
        "method": "notifications/initialized",
    });
    assert!(handle(&notification).is_none());
}

#[test]
fn ping_answers_with_an_empty_result() {
    let reply = handle(&request(2, "ping", json!({}))).expect("response");
    assert_eq!(reply["result"], json!({}));
}

#[test]
fn an_unknown_method_is_a_method_not_found_error() {
    let reply = handle(&request(3, "resources/list", json!({}))).expect("response");
    assert_eq!(reply["error"]["code"], -32601);
}

#[test]
fn a_message_with_an_id_but_no_method_is_invalid() {
    let broken = json!({ "jsonrpc": "2.0", "id": 4 });
    let reply = handle(&broken).expect("response");
    assert_eq!(reply["error"]["code"], -32600);
}

// ------------------------------------------------------------------ tools/list

#[test]
fn the_tool_list_names_all_six_with_schemas() {
    let reply = handle(&request(5, "tools/list", json!({}))).expect("response");
    let tools = reply["result"]["tools"].as_array().expect("array");
    let names: Vec<&str> = tools
        .iter()
        .map(|t| t["name"].as_str().expect("name"))
        .collect();
    assert_eq!(
        names,
        [
            "check",
            "goals",
            "type_at",
            "producers",
            "fmt",
            "explain",
            "run"
        ]
    );
    for tool in tools {
        assert!(
            tool["inputSchema"]["required"].is_array(),
            "{} lacks a schema",
            tool["name"]
        );
        assert!(
            tool["description"].as_str().expect("description").len() > 40,
            "{} deserves a real description",
            tool["name"]
        );
    }
}

#[test]
fn an_unknown_tool_is_invalid_params() {
    let reply = handle(&request(
        6,
        "tools/call",
        json!({ "name": "compile_to_wasm", "arguments": {} }),
    ))
    .expect("response");
    assert_eq!(reply["error"]["code"], -32602);
}

// ------------------------------------------------------------------ the tools

#[test]
fn goals_reports_the_hole_with_candidates() {
    let path = scratch(
        "goals",
        "enum ApiError {\n    Down,\n}\n\nfn try_fetch() -> Result<Int, ApiError> {\n    ??body\n}\n",
    );
    let (is_error, text) = call("goals", json!({ "path": path }));
    assert!(!is_error, "{text}");
    let parsed: Value = serde_json::from_str(&text).expect("payload is JSON");
    let goal = &parsed[0];
    assert_eq!(goal["schema_version"], 1);
    assert_eq!(goal["hole"], "body");
    assert_eq!(goal["expected"], "Result<Int, ApiError>");
    let candidates = goal["candidates"].as_array().expect("candidates");
    assert!(!candidates.is_empty(), "{text}");
}

#[test]
fn check_carries_the_effect_fix() {
    let path = scratch(
        "check",
        "fn log(io: Io, text: String) -> Result<Unit, Error> {\n    io.write(text: text)\n}\n",
    );
    let (is_error, text) = call("check", json!({ "path": path }));
    assert!(!is_error, "tool ran; problems live in the payload");
    let parsed: Value = serde_json::from_str(&text).expect("payload is JSON");
    assert_eq!(parsed["schema_version"], 1);
    let diagnostic = &parsed["diagnostics"][0];
    assert_eq!(diagnostic["code"], "XN4001");
    assert!(
        diagnostic["fix"]["edits"][0]["replacement"]
            .as_str()
            .expect("fix")
            .contains("uses {Io.write}"),
        "{text}"
    );
}

#[test]
fn type_at_answers_for_a_binding() {
    let path = scratch(
        "typeat",
        "fn f() -> Int {\n    let total = 1 + 2;\n    total\n}\n",
    );
    let (is_error, text) = call("type_at", json!({ "path": path, "line": 2, "column": 9 }));
    assert!(!is_error, "{text}");
    let parsed: Value = serde_json::from_str(&text).expect("payload is JSON");
    assert_eq!(parsed["schema_version"], 1);
    assert_eq!(parsed["type"], "Int");
    assert_eq!(parsed["enclosing_function"], "f");
}

#[test]
fn producers_lists_functions_and_variants() {
    let path = scratch(
        "producers",
        "struct Player {\n    name: String,\n}\n\nenum ScoreError {\n    Overflow,\n}\n\n\
         fn try_award(player: Player, points: Int) -> Result<Player, ScoreError> {\n    ??x\n}\n",
    );
    let (is_error, text) = call(
        "producers",
        json!({ "path": path, "type": "Result<Player, ScoreError>" }),
    );
    assert!(!is_error, "{text}");
    let parsed: Value = serde_json::from_str(&text).expect("payload is JSON");
    let entries = parsed.as_array().expect("array");
    for entry in entries {
        assert_eq!(entry["schema_version"], 1, "{entry}");
    }
    let symbols: Vec<&str> = entries
        .iter()
        .map(|p| p["symbol"].as_str().expect("symbol"))
        .collect();
    assert!(symbols.contains(&"try_award"), "{symbols:?}");
    assert!(symbols.contains(&"Ok"), "{symbols:?}");
}

#[test]
fn producers_of_an_unknown_type_is_a_tool_error() {
    let path = scratch("producers-err", "fn f() -> Int {\n    1\n}\n");
    let (is_error, text) = call("producers", json!({ "path": path, "type": "Mystery" }));
    assert!(is_error);
    assert!(text.contains("Mystery"), "{text}");
}

#[test]
fn fmt_without_write_reports_and_returns_but_does_not_touch_the_file() {
    let messy = "fn   f( )->Int{ 1 }";
    let path = scratch("fmt", messy);
    let (is_error, text) = call("fmt", json!({ "path": path }));
    assert!(!is_error, "{text}");
    let parsed: Value = serde_json::from_str(&text).expect("payload is JSON");
    assert_eq!(parsed["changed"], true);
    assert!(
        parsed["formatted"]
            .as_str()
            .expect("formatted")
            .starts_with("fn f() -> Int {"),
        "{text}"
    );
    assert_eq!(
        std::fs::read_to_string(&path).expect("readable"),
        messy,
        "write=false must not modify the file"
    );
}

#[test]
fn fmt_with_write_rewrites_the_file() {
    let path = scratch("fmt-write", "fn   f( )->Int{ 1 }");
    let (is_error, _) = call("fmt", json!({ "path": path, "write": true }));
    assert!(!is_error);
    let now = std::fs::read_to_string(&path).expect("readable");
    assert!(now.starts_with("fn f() -> Int {"), "{now}");
}

#[test]
fn explain_answers_case_insensitively() {
    let (is_error, text) = call("explain", json!({ "code": "xn4001" }));
    assert!(!is_error);
    assert!(text.starts_with("XN4001"), "{text}");
    assert!(text.contains("uses"), "{text}");
}

#[test]
fn run_executes_main_and_captures_stdout() {
    let path = scratch(
        "run",
        "fn main(io: Io) -> Result<Unit, Error> uses {Io.write} {\n    \
             io.write(text: \"hi from xenith\")?;\n    return Ok(unit);\n}\n",
    );
    let (is_error, text) = call("run", json!({ "path": path }));
    assert!(!is_error, "{text}");
    let parsed: Value = serde_json::from_str(&text).expect("payload is JSON");
    assert_eq!(parsed["exit_code"], 0);
    assert_eq!(parsed["stdout"], "hi from xenith");
}

#[test]
fn run_refuses_a_file_with_diagnostics() {
    let path = scratch("run-refuse", "fn main() -> Int {\n    true\n}\n");
    let (is_error, text) = call("run", json!({ "path": path }));
    assert!(!is_error);
    let parsed: Value = serde_json::from_str(&text).expect("payload is JSON");
    assert_eq!(parsed["exit_code"], 2);
}

#[test]
fn run_traps_on_a_hole_and_names_it() {
    let path = scratch(
        "run-hole",
        "fn main() -> Int {\n    let x: Int = ??start;\n    x\n}\n",
    );
    let (is_error, text) = call("run", json!({ "path": path }));
    assert!(!is_error, "{text}");
    let parsed: Value = serde_json::from_str(&text).expect("payload is JSON");
    assert_eq!(parsed["exit_code"], 101);
    assert!(
        parsed["error"].as_str().expect("error").contains("??start"),
        "{text}"
    );
}

#[test]
fn a_missing_file_is_a_tool_error_not_a_crash() {
    let (is_error, text) = call("check", json!({ "path": "no-such-file.xn" }));
    assert!(is_error);
    assert!(text.contains("no-such-file.xn"), "{text}");
}

// ------------------------------------------------------- workspace confinement
//
// Symlinks are covered by construction rather than by a test: the server
// canonicalizes before the containment check, and canonicalization resolves
// links, so a symlink inside the root pointing outside compares as its target
// and is refused. Exercising that here would require creating a symlink,
// which on Windows needs administrator rights or developer mode.

#[test]
fn a_relative_path_resolves_against_the_workspace_root() {
    let root = scratch_root("inside");
    std::fs::write(root.join("inside.xn"), "fn f() -> Int {\n    1\n}\n").expect("writable root");
    let (is_error, text) = call_in(&root, "check", json!({ "path": "inside.xn" }));
    assert!(!is_error, "{text}");
    let parsed: Value = serde_json::from_str(&text).expect("payload is JSON");
    assert_eq!(parsed["schema_version"], 1);
    assert_eq!(parsed["diagnostics"], json!([]));
}

#[test]
fn a_dot_dot_escape_is_rejected() {
    let root = scratch_root("escape");
    // The target exists and is well-formed, so the only reason to refuse it
    // is where it lives.
    scratch("escape-target", "fn f() -> Int {\n    1\n}\n");
    let (is_error, text) = call_in(
        &root,
        "check",
        json!({ "path": "../xenith-mcp-test-escape-target.xn" }),
    );
    assert!(is_error, "{text}");
    assert!(text.contains("outside the workspace root"), "{text}");
}

#[test]
fn an_absolute_path_outside_the_root_is_rejected_even_for_fmt_write() {
    let root = scratch_root("absolute");
    let messy = "fn   f( )->Int{ 1 }";
    let outside = scratch("outside-abs", messy);
    let (is_error, text) = call_in(&root, "fmt", json!({ "path": outside, "write": true }));
    assert!(is_error, "{text}");
    assert!(text.contains("outside the workspace root"), "{text}");
    assert_eq!(
        std::fs::read_to_string(&outside).expect("readable"),
        messy,
        "a refused fmt must not touch the file"
    );
}

// ------------------------------------------------------------- argument ranges

#[test]
fn an_out_of_range_line_or_column_is_rejected() {
    let path = scratch("range", "fn f() -> Int {\n    1\n}\n");
    let too_big: u64 = u64::from(u32::MAX) + 1;

    let (is_error, text) = call(
        "type_at",
        json!({ "path": path, "line": too_big, "column": 1 }),
    );
    assert!(is_error);
    assert!(text.contains("`line` out of range"), "{text}");

    let (is_error, text) = call(
        "type_at",
        json!({ "path": path, "line": 1, "column": too_big }),
    );
    assert!(is_error);
    assert!(text.contains("`column` out of range"), "{text}");
}

// -------------------------------------------------------------- atomic writes

// ---------------------------------------------------------- project mode
//
// The design/0013 §1 contract: `mode` on check/run/goals/type_at/producers,
// auto = project when a manifest governs the file, explicit errors for every
// discovery failure, responses that say which mode actually ran, and
// requested-file-first ordering.

/// A scratch project: manifest plus the given `src/`-relative files.
fn scratch_project(name: &str, files: &[(&str, &str)]) -> PathBuf {
    let root = std::env::temp_dir().join(format!("xenith-mcp-proj-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("src")).expect("writable temp dir");
    std::fs::write(root.join("xenith.toml"), "name = \"mcp-proj\"\n").expect("manifest");
    for (rel, content) in files {
        let mut path = root.join("src");
        for part in rel.split('/') {
            path.push(part);
        }
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("tree");
        }
        std::fs::write(&path, content).expect("file");
    }
    root
}

/// The player module most of these projects share.
const PLAYER: &str = "pub struct Player {\n    score: Int,\n}\n\n\
                      pub fn fresh() -> Player {\n    Player { score: 7 }\n}\n";

#[test]
fn check_auto_inside_a_project_reports_every_file_requested_first() {
    let root = scratch_project(
        "check-auto",
        &[
            (
                "main.xn",
                "use game.player;\n\nfn main() -> Int {\n    game.player.fresh().score\n}\n",
            ),
            ("game/player.xn", PLAYER),
        ],
    );
    // The *dependency* is requested, so it must lead the file list even
    // though `src/main.xn` sorts first lexicographically.
    let (is_error, text) = call_in(&root, "check", json!({ "path": "src/game/player.xn" }));
    assert!(!is_error, "{text}");
    let parsed: Value = serde_json::from_str(&text).expect("payload is JSON");
    assert_eq!(parsed["schema_version"], 1);
    assert_eq!(parsed["analysis_mode"], "project");
    assert_eq!(parsed["project_root"], ".");
    assert_eq!(parsed["requested"], "src/game/player.xn");
    let features: Vec<&str> = parsed["features"]
        .as_array()
        .expect("features")
        .iter()
        .map(|f| f.as_str().expect("feature"))
        .collect();
    assert!(features.contains(&"project_mode_v1"), "{features:?}");
    let files: Vec<&str> = parsed["files"]
        .as_array()
        .expect("files")
        .iter()
        .map(|f| f["file"].as_str().expect("file"))
        .collect();
    assert_eq!(files, ["src/game/player.xn", "src/main.xn"]);
    for file in parsed["files"].as_array().expect("files") {
        assert_eq!(file["diagnostics"], json!([]), "{file}");
    }
}

#[test]
fn check_in_project_mode_carries_cross_file_diagnostics() {
    let root = scratch_project(
        "check-diag",
        &[
            (
                "main.xn",
                "use game.player;\n\nfn main() -> Int {\n    game.player.missing()\n}\n",
            ),
            ("game/player.xn", PLAYER),
        ],
    );
    let (is_error, text) = call_in(&root, "check", json!({ "path": "src/main.xn" }));
    assert!(!is_error, "{text}");
    let parsed: Value = serde_json::from_str(&text).expect("payload is JSON");
    assert_eq!(parsed["files"][0]["file"], "src/main.xn");
    let diagnostic = &parsed["files"][0]["diagnostics"][0];
    assert_eq!(diagnostic["severity"], "error");
    assert!(diagnostic["line"].is_u64(), "{diagnostic}");
}

#[test]
fn single_file_mode_is_honored_inside_a_project() {
    let root = scratch_project(
        "check-single",
        &[
            ("main.xn", "fn main() -> Int {\n    1\n}\n"),
            ("game/player.xn", PLAYER),
        ],
    );
    let (is_error, text) = call_in(
        &root,
        "check",
        json!({ "path": "src/main.xn", "mode": "single_file" }),
    );
    assert!(!is_error, "{text}");
    let parsed: Value = serde_json::from_str(&text).expect("payload is JSON");
    assert_eq!(parsed["analysis_mode"], "single_file");
    assert_eq!(parsed["file"], "src/main.xn");
    assert!(parsed["diagnostics"].is_array(), "{text}");
    assert!(parsed.get("files").is_none(), "single-file keeps its shape");
}

#[test]
fn demanded_project_mode_without_a_manifest_is_a_tool_error() {
    let root = scratch_root("no-manifest");
    std::fs::write(root.join("lone.xn"), "fn f() -> Int {\n    1\n}\n").expect("writable root");
    let (is_error, text) = call_in(
        &root,
        "check",
        json!({ "path": "lone.xn", "mode": "project" }),
    );
    assert!(is_error, "{text}");
    assert!(text.contains("no `xenith.toml`"), "{text}");
}

#[test]
fn an_invalid_mode_is_a_tool_error() {
    let root = scratch_root("bad-mode");
    std::fs::write(root.join("f.xn"), "fn f() -> Int {\n    1\n}\n").expect("writable root");
    let (is_error, text) = call_in(&root, "check", json!({ "path": "f.xn", "mode": "psychic" }));
    assert!(is_error);
    assert!(text.contains("`mode` must be"), "{text}");
}

#[test]
fn an_invalid_layout_is_a_tool_error_never_a_fallback() {
    let root = scratch_project(
        "bad-layout",
        &[
            ("main.xn", "fn main() -> Int {\n    1\n}\n"),
            ("player-v2.xn", "pub fn p() -> Int {\n    1\n}\n"),
        ],
    );
    let (is_error, text) = call_in(&root, "check", json!({ "path": "src/main.xn" }));
    assert!(
        is_error,
        "an invalid layout must not check quietly:\n{text}"
    );
    assert!(text.contains("invalid layout"), "{text}");
    assert!(text.contains("player-v2"), "{text}");
    // The sanctioned way to look at one file under a broken layout.
    let (is_error, text) = call_in(
        &root,
        "check",
        json!({ "path": "src/main.xn", "mode": "single_file" }),
    );
    assert!(!is_error, "{text}");
}

#[test]
fn a_project_reaching_above_the_workspace_root_is_refused() {
    // The manifest sits *above* the workspace root: discovery finds it,
    // containment refuses it — and no silent single-file answer happens
    // (design/0013 §1).
    let project = scratch_project(
        "above-root",
        &[("main.xn", "fn main() -> Int {\n    1\n}\n")],
    );
    let boundary = project.join("src");
    let (is_error, text) = call_in(&boundary, "check", json!({ "path": "main.xn" }));
    assert!(is_error, "{text}");
    assert!(text.contains("outside the workspace root"), "{text}");
}

#[test]
fn run_executes_the_whole_project_from_any_member_file() {
    let root = scratch_project(
        "run-proj",
        &[
            (
                "main.xn",
                "use game.dep;\n\nfn main(io: Io) -> Result<Unit, Error> uses {Io.write} {\n    \
                 io.write(text: game.dep.tag())?;\n    return Ok(unit);\n}\n",
            ),
            (
                "game/dep.xn",
                "pub fn tag() -> String {\n    \"from dep\"\n}\n",
            ),
        ],
    );
    // Naming the dependency still runs the project's `src/main.xn`.
    let (is_error, text) = call_in(&root, "run", json!({ "path": "src/game/dep.xn" }));
    assert!(!is_error, "{text}");
    let parsed: Value = serde_json::from_str(&text).expect("payload is JSON");
    assert_eq!(parsed["analysis_mode"], "project");
    assert_eq!(parsed["project_root"], ".");
    assert_eq!(parsed["exit_code"], 0);
    assert_eq!(parsed["stdout"], "from dep");
}

#[test]
fn run_refuses_a_project_with_diagnostics_anywhere() {
    let root = scratch_project(
        "run-refuse",
        &[
            ("main.xn", "fn main() -> Int {\n    1\n}\n"),
            ("game/dep.xn", "pub fn broken() -> Int {\n    true\n}\n"),
        ],
    );
    let (is_error, text) = call_in(&root, "run", json!({ "path": "src/main.xn" }));
    assert!(!is_error, "{text}");
    let parsed: Value = serde_json::from_str(&text).expect("payload is JSON");
    assert_eq!(parsed["exit_code"], 2);
    assert_eq!(parsed["analysis_mode"], "project");
    assert!(
        parsed["error"]
            .as_str()
            .expect("error")
            .contains("the project has diagnostics"),
        "{text}"
    );
}

#[test]
fn goals_in_project_mode_lists_the_requested_files_holes_first() {
    let root = scratch_project(
        "goals-proj",
        &[
            ("main.xn", "fn main() -> Int {\n    ??main_hole\n}\n"),
            (
                "game/dep.xn",
                "pub fn depth() -> Int {\n    ??dep_hole\n}\n",
            ),
        ],
    );
    let (is_error, text) = call_in(&root, "goals", json!({ "path": "src/main.xn" }));
    assert!(!is_error, "{text}");
    let parsed: Value = serde_json::from_str(&text).expect("payload is JSON");
    let entries = parsed.as_array().expect("goals stay an array");
    assert_eq!(entries.len(), 2, "{text}");
    assert_eq!(entries[0]["hole"], "main_hole", "requested file first");
    assert_eq!(entries[0]["file"], "src/main.xn");
    assert_eq!(entries[0]["analysis_mode"], "project");
    assert_eq!(entries[0]["project_root"], ".");
    assert_eq!(entries[1]["hole"], "dep_hole");
    assert_eq!(entries[1]["file"], "src/game/dep.xn");
}

#[test]
fn goals_in_single_file_mode_declares_itself() {
    let path = scratch("goals-mode", "fn f() -> Int {\n    ??body\n}\n");
    let (is_error, text) = call("goals", json!({ "path": path }));
    assert!(!is_error, "{text}");
    let parsed: Value = serde_json::from_str(&text).expect("payload is JSON");
    assert_eq!(parsed[0]["analysis_mode"], "single_file");
    assert!(parsed[0].get("project_root").is_none());
}

#[test]
fn type_at_in_project_mode_answers_with_qualified_cross_module_types() {
    let root = scratch_project(
        "typeat-proj",
        &[
            (
                "main.xn",
                "use game.player;\n\nfn main() -> Int {\n    let hero = game.player.fresh();\n    \
                 hero.score\n}\n",
            ),
            ("game/player.xn", PLAYER),
        ],
    );
    let (is_error, text) = call_in(
        &root,
        "type_at",
        json!({ "path": "src/main.xn", "line": 4, "column": 9 }),
    );
    assert!(!is_error, "{text}");
    let parsed: Value = serde_json::from_str(&text).expect("payload is JSON");
    assert_eq!(parsed["analysis_mode"], "project");
    assert_eq!(parsed["project_root"], ".");
    assert_eq!(
        parsed["type"], "game.player.Player",
        "the project's declarations are in view: {text}"
    );
}

#[test]
fn producers_in_project_mode_answers_from_the_used_modules_pub_surface() {
    let root = scratch_project(
        "producers-proj",
        &[
            (
                "main.xn",
                "use game.player;\n\nfn main() -> Int {\n    game.player.fresh().score\n}\n",
            ),
            ("game/player.xn", PLAYER),
        ],
    );
    let (is_error, text) = call_in(
        &root,
        "producers",
        json!({ "path": "src/main.xn", "type": "game.player.Player" }),
    );
    assert!(!is_error, "{text}");
    let parsed: Value = serde_json::from_str(&text).expect("payload is JSON");
    let entries = parsed.as_array().expect("array");
    let symbols: Vec<&str> = entries
        .iter()
        .map(|p| p["symbol"].as_str().expect("symbol"))
        .collect();
    assert!(symbols.contains(&"game.player.fresh"), "{symbols:?}");
    assert!(symbols.contains(&"game.player.Player"), "{symbols:?}");
    for entry in entries {
        assert_eq!(entry["analysis_mode"], "project", "{entry}");
        assert_eq!(entry["project_root"], ".", "{entry}");
    }

    // The same question in single-file mode cannot even name the type —
    // the difference between the modes is real, and declared.
    let (is_error, text) = call_in(
        &root,
        "producers",
        json!({ "path": "src/main.xn", "type": "game.player.Player", "mode": "single_file" }),
    );
    assert!(is_error, "{text}");
}

// ------------------------------------------------------ experimental surface

#[test]
fn api_surface_is_absent_by_default_and_present_behind_the_flag() {
    // Absent from the default list — asserted exactly by
    // `the_tool_list_names_all_six_with_schemas` — and unknown to call.
    let (_, text) = {
        let reply = handle(&request(
            9,
            "tools/call",
            json!({ "name": "api_surface", "arguments": {} }),
        ))
        .expect("response");
        (
            reply["error"]["code"].clone(),
            reply["error"]["message"].as_str().unwrap_or("").to_string(),
        )
    };
    assert!(text.contains("no tool named"), "{text}");

    // Behind the flag: listed, described with the 0011 wiring caveat, and
    // callable.
    let options = ServerOptions {
        experimental_api_surface: true,
    };
    let root = scratch_project(
        "api-surface",
        &[
            ("main.xn", "fn main() -> Int {\n    1\n}\n"),
            ("game/player.xn", PLAYER),
        ],
    );
    let listed = handle_message_with(&request(10, "tools/list", json!({})), &root, &options)
        .expect("response");
    let tools = listed["result"]["tools"].as_array().expect("array");
    let surface = tools
        .iter()
        .find(|t| t["name"] == "api_surface")
        .expect("the flag exposes the tool");
    let description = surface["description"].as_str().expect("description");
    assert!(
        description.contains("not a substitute for wiring"),
        "{description}"
    );
    assert!(description.contains("0/28"), "{description}");

    let reply = handle_message_with(
        &request(
            11,
            "tools/call",
            json!({ "name": "api_surface", "arguments": { "module": "game.player" } }),
        ),
        &root,
        &options,
    )
    .expect("response");
    let result = &reply["result"];
    assert_eq!(result["isError"], false, "{reply}");
    let payload: Value = serde_json::from_str(result["content"][0]["text"].as_str().expect("text"))
        .expect("payload is JSON");
    assert_eq!(payload["api_schema_version"], 1);
    assert_eq!(payload["project_root"], ".");
    assert_eq!(payload["modules"][0]["module"], "game.player");
    assert_eq!(
        payload["modules"][0]["functions"][0]["signature"],
        "pub fn fresh() -> Player"
    );
    assert!(
        payload["modules"]
            .as_array()
            .expect("modules")
            .iter()
            .all(|m| m["module"] != "main"),
        "the module filter holds: {payload}"
    );
}

#[test]
fn api_surface_with_an_unknown_module_is_a_tool_error() {
    let options = ServerOptions {
        experimental_api_surface: true,
    };
    let root = scratch_project(
        "api-unknown",
        &[("main.xn", "fn main() -> Int {\n    1\n}\n")],
    );
    let reply = handle_message_with(
        &request(
            12,
            "tools/call",
            json!({ "name": "api_surface", "arguments": { "module": "nowhere" } }),
        ),
        &root,
        &options,
    )
    .expect("response");
    assert_eq!(reply["result"]["isError"], true, "{reply}");
    let text = reply["result"]["content"][0]["text"]
        .as_str()
        .expect("text");
    assert!(text.contains("no module `nowhere`"), "{text}");
}

#[test]
fn fmt_write_leaves_no_temporary_file_behind() {
    let path = scratch("fmt-atomic", "fn   f( )->Int{ 1 }");
    let (is_error, text) = call("fmt", json!({ "path": path, "write": true }));
    assert!(!is_error, "{text}");
    let now = std::fs::read_to_string(&path).expect("readable");
    assert!(now.starts_with("fn f() -> Int {"), "{now}");

    // The temp file is a dot-prefixed sibling named after the target; after a
    // successful rename none may remain.
    let leftovers: Vec<String> = std::fs::read_dir(std::env::temp_dir())
        .expect("listable temp dir")
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| name.starts_with(".xenith-mcp-test-fmt-atomic.xn."))
        .collect();
    assert_eq!(leftovers, Vec::<String>::new());
}
