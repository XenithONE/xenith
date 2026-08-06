//! End-to-end conformance for the module system (design/0010 §7-1..3),
//! through the real binary over fixture project trees. Layout rules that a
//! Windows file system cannot even represent — case collisions — live in
//! the driver's pure-function tests instead.

use std::path::Path;
use std::process::Command;

fn xenith(args: &[&str]) -> (String, String, Option<i32>) {
    let output = Command::new(env!("CARGO_BIN_EXE_xenith"))
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .args(args)
        .output()
        .expect("the compiler binary runs");
    (
        String::from_utf8(output.stdout).expect("stdout is UTF-8"),
        String::from_utf8(output.stderr).expect("stderr is UTF-8"),
        output.status.code(),
    )
}

fn fixture(name: &str, file: &str) -> String {
    format!("tests/fixtures/projects/{name}/{file}")
}

#[test]
fn a_two_module_project_checks_and_runs() {
    let entry = fixture("two_mod", "src/main.xn");
    let (stdout, _, code) = xenith(&["check", &entry]);
    assert_eq!(stdout, "", "no diagnostics expected:\n{stdout}");
    assert_eq!(code, Some(0));
    let (stdout, _, code) = xenith(&["run", &entry]);
    assert_eq!(stdout, "Hello, modules");
    assert_eq!(code, Some(0));
}

#[test]
fn the_vertical_proof_project_checks_and_runs() {
    // Three modules: a private helper stays internal, a qualified struct is
    // constructed across the boundary, mutation happens only through the
    // owner's pub API, and qualified enum patterns stay exhaustive.
    let entry = fixture("vertical", "src/main.xn");
    let (stdout, _, code) = xenith(&["check", &entry]);
    assert_eq!(stdout, "", "no diagnostics expected:\n{stdout}");
    assert_eq!(code, Some(0));
    let (stdout, _, code) = xenith(&["run", &entry]);
    assert_eq!(stdout, "ada: gold");
    assert_eq!(code, Some(0));
}

#[test]
fn a_cross_module_field_assignment_is_refused() {
    let entry = fixture("refused", "src/main.xn");
    let (stdout, _, code) = xenith(&["check", &entry]);
    assert!(stdout.contains("error[XN7008]"), "{stdout}");
    assert!(
        stdout.contains("cannot be assigned from outside `game.player`"),
        "{stdout}"
    );
    assert_eq!(code, Some(1));
    let (_, stderr, code) = xenith(&["run", &entry]);
    assert!(stderr.contains("not run"), "{stderr}");
    assert_eq!(code, Some(2));
}

#[test]
fn a_library_project_checks_but_does_not_run() {
    let (_, stderr, code) = xenith(&["check", &fixture("lib_only", "src/util.xn")]);
    assert!(stderr.contains("no problems found"), "{stderr}");
    assert_eq!(code, Some(0));
    let (_, stderr, code) = xenith(&["run", &fixture("lib_only", "src/util.xn")]);
    assert!(stderr.contains("no `src/main.xn`"), "{stderr}");
    assert_eq!(code, Some(101));
}

#[test]
fn fn_main_outside_the_entry_module_is_refused() {
    let (stdout, _, code) = xenith(&["check", &fixture("misplaced", "src/main.xn")]);
    assert!(stdout.contains("error[XN7004]"), "{stdout}");
    assert!(
        stdout.contains("other.xn"),
        "the stray main is the one named"
    );
    assert_eq!(code, Some(1));
}

#[test]
fn a_module_path_clashing_with_a_parent_item_is_refused() {
    let (stdout, _, code) = xenith(&["check", &fixture("exclusive", "src/main.xn")]);
    assert!(stdout.contains("error[XN7003]"), "{stdout}");
    assert!(stdout.contains("`game.player`"), "{stdout}");
    assert_eq!(code, Some(1));
}

#[test]
fn a_file_that_cannot_name_a_module_is_refused() {
    let (stdout, _, code) = xenith(&["check", &fixture("badname", "src/main.xn")]);
    assert!(stdout.contains("error[XN7001]"), "{stdout}");
    assert!(stdout.contains("player-v2"), "{stdout}");
    assert_eq!(code, Some(1));
}

#[test]
fn an_import_cycle_between_modules_checks_and_runs() {
    // Two modules `use` each other, types linked through `Option` — legal,
    // and the two-phase order makes it uneventful (design/0010 §5).
    let entry = fixture("cyclic", "src/main.xn");
    let (stdout, _, code) = xenith(&["check", &entry]);
    assert_eq!(
        stdout, "",
        "no diagnostics expected:
{stdout}"
    );
    assert_eq!(code, Some(0));
    let (stdout, _, code) = xenith(&["run", &entry]);
    assert_eq!(stdout, "2,3");
    assert_eq!(code, Some(0));
}

#[test]
fn the_guestbook_example_checks_and_runs() {
    // The shipped multi-module example is documentation, and documentation
    // rots — so it goes through the real pipe here and in CI.
    let entry = "../../../examples/guestbook/src/main.xn";
    let (stdout, _, code) = xenith(&["check", entry]);
    assert_eq!(
        stdout, "",
        "no diagnostics expected:
{stdout}"
    );
    assert_eq!(code, Some(0));
    let (stdout, _, code) = xenith(&["run", entry]);
    assert_eq!(stdout, "guestbook: ada");
    assert_eq!(code, Some(0));
}

#[test]
fn a_file_without_a_manifest_stays_single_file() {
    // The frozen diagnostics fixtures live outside any project; the whole
    // pre-module pipeline — `use std.io;` inertness included — is pinned by
    // their byte-identity tests. Here: no manifest, no module rules.
    assert!(
        !Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/xenith.toml")
            .exists(),
        "the fixture tree must not accidentally become a project"
    );
    let (stdout, _, code) = xenith(&["check", "tests/fixtures/diag/xn3001_mismatch.xn"]);
    assert!(stdout.contains("error[XN3001]"), "{stdout}");
    assert_eq!(code, Some(1));
}
