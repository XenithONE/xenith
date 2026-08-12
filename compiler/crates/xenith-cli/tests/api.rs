//! `xenith api` (design/0013 §2), through the real binary over the vertical
//! fixture: the ApiSurface model rendered as text and JSON, module scoping,
//! and the refusals — no manifest, unknown module.

use std::path::Path;
use std::process::Command;

fn xenith_in(dir: &Path, args: &[&str]) -> (String, String, Option<i32>) {
    let output = Command::new(env!("CARGO_BIN_EXE_xenith"))
        .current_dir(dir)
        .args(args)
        .output()
        .expect("the compiler binary runs");
    (
        String::from_utf8(output.stdout).expect("stdout is UTF-8"),
        String::from_utf8(output.stderr).expect("stderr is UTF-8"),
        output.status.code(),
    )
}

fn xenith(args: &[&str]) -> (String, String, Option<i32>) {
    xenith_in(Path::new(env!("CARGO_MANIFEST_DIR")), args)
}

const VERTICAL: &str = "tests/fixtures/projects/vertical";

#[test]
fn api_renders_every_module_of_the_project() {
    let (stdout, _, code) = xenith(&["api", VERTICAL]);
    assert_eq!(code, Some(0));
    assert!(stdout.contains("module game.player\n"), "{stdout}");
    assert!(stdout.contains("module game.scores\n"), "{stdout}");
    // The full surface, private items excluded.
    assert!(
        stdout.contains("pub fn award(player: Player, points: Int) -> Player"),
        "{stdout}"
    );
    assert!(
        stdout.contains("pub struct Player {\n    name: String,\n    var score: Int,\n}"),
        "{stdout}"
    );
    assert!(
        !stdout.contains("clamp"),
        "private items stay out:\n{stdout}"
    );
    // The entry module is part of the project's surface listing, honestly
    // empty — only the bench dump renderer excludes it.
    assert!(
        stdout.contains("module main\n\n(no public items)\n"),
        "{stdout}"
    );
}

#[test]
fn api_accepts_any_path_inside_the_project() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join(VERTICAL);
    let (from_root, _, _) = xenith(&["api", VERTICAL]);
    let (from_file, _, code) = xenith_in(&root, &["api", "src/game/player.xn"]);
    assert_eq!(code, Some(0));
    assert_eq!(from_root, from_file, "one project, one surface");
}

#[test]
fn api_scopes_to_a_module_subtree() {
    let (stdout, _, code) = xenith(&["api", VERTICAL, "--module", "game.player"]);
    assert_eq!(code, Some(0));
    assert!(stdout.contains("module game.player\n"), "{stdout}");
    assert!(!stdout.contains("module game.scores"), "{stdout}");
    assert!(!stdout.contains("module main"), "{stdout}");

    // The parent path scopes to the whole subtree.
    let (subtree, _, code) = xenith(&["api", VERTICAL, "--module", "game"]);
    assert_eq!(code, Some(0));
    assert!(subtree.contains("module game.player\n"), "{subtree}");
    assert!(subtree.contains("module game.scores\n"), "{subtree}");
}

#[test]
fn api_refuses_an_unknown_module_with_the_known_list() {
    let (_, stderr, code) = xenith(&["api", VERTICAL, "--module", "game.nowhere"]);
    assert_eq!(code, Some(1));
    assert!(stderr.contains("no module `game.nowhere`"), "{stderr}");
    assert!(stderr.contains("game.player"), "{stderr}");
}

#[test]
fn api_json_carries_its_own_schema_version() {
    let (stdout, _, code) = xenith(&["api", VERTICAL, "--module", "game.player", "--json"]);
    assert_eq!(code, Some(0));
    let payload: serde_json::Value = serde_json::from_str(&stdout).expect("JSON output");
    assert_eq!(payload["api_schema_version"], 1);
    assert!(
        payload.get("schema_version").is_none(),
        "the api payload versions itself, independent of the wire"
    );
    let module = &payload["modules"][0];
    assert_eq!(module["module"], "game.player");
    let functions = module["functions"].as_array().expect("functions");
    assert!(
        functions
            .iter()
            .any(|f| f["signature"] == "pub fn rank_of(player: Player) -> Rank"),
        "{stdout}"
    );
    let structs = module["structs"].as_array().expect("structs");
    assert_eq!(structs[0]["fields"][1]["mutable"], true, "{stdout}");
}

#[test]
fn api_without_a_manifest_is_an_error() {
    let (_, stderr, code) = xenith(&["api", "tests/fixtures/diag"]);
    assert_eq!(code, Some(1));
    assert!(stderr.contains("no `xenith.toml`"), "{stderr}");
}
