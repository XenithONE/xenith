//! Byte-level regression fixtures for diagnostic output (design/0009 §6
//! step 1).
//!
//! The goldens under `tests/fixtures/diag/` were rendered by the compiler as
//! it stood before diagnostic teaching existed. They pin two contracts at
//! once: `--diagnostic-teaching=off` must reproduce them byte for byte, and
//! teaching may add nothing anywhere except its own lines and fields.
//!
//! Everything runs through the real binary so the goldens cover the whole
//! pipe — analysis, wire, rendering — not a unit in isolation.

use std::path::Path;
use std::process::Command;

const FIXTURES: &[&str] = &[
    "xn2002_unknown_name",
    "xn2003_list",
    "xn2003_map",
    "xn2003_string",
    "xn3001_mismatch",
    "xn3008_method",
    "xn3008_user_fn",
    "xn4001_effect",
    "xn5001_match",
];

/// Fixtures whose diagnostics carry teaches when teaching is on. Everything
/// else must render identically with teaching on or off.
const TAUGHT: &[&str] = &[
    "xn2003_list",
    "xn2003_map",
    "xn2003_string",
    "xn3008_method",
    "xn3008_user_fn",
];

/// Run `xenith check` on one fixture, from the crate root so the paths in
/// the output are the same relative strings the goldens hold.
fn check(name: &str, args: &[&str]) -> String {
    let fixture = format!("tests/fixtures/diag/{name}.xn");
    let output = Command::new(env!("CARGO_BIN_EXE_xenith"))
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .arg("check")
        .args(args)
        .arg(&fixture)
        .output()
        .expect("the compiler binary runs");
    String::from_utf8(output.stdout).expect("diagnostics are UTF-8")
}

fn golden(name: &str, kind: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/diag")
        .join(format!("{name}.{kind}.golden"));
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

#[test]
fn teaching_off_reproduces_the_frozen_text_byte_for_byte() {
    for name in FIXTURES {
        assert_eq!(
            check(name, &["--diagnostic-teaching=off"]),
            golden(name, "text"),
            "{name} drifted from its frozen rendering"
        );
    }
}

#[test]
fn teaching_off_reproduces_the_frozen_json_byte_for_byte() {
    for name in FIXTURES {
        assert_eq!(
            check(name, &["--json", "--diagnostic-teaching=off"]),
            golden(name, "json"),
            "{name} drifted from its frozen wire shape"
        );
    }
}

#[test]
fn teaching_changes_nothing_for_untaught_codes() {
    for name in FIXTURES.iter().filter(|n| !TAUGHT.contains(n)) {
        assert_eq!(
            check(name, &[]),
            golden(name, "text"),
            "{name} has no teach, so teaching on must not move a byte"
        );
    }
}

#[test]
fn taught_text_matches_its_snapshot_and_differs_only_by_teach_lines() {
    for name in TAUGHT {
        let taught = check(name, &[]);
        assert_eq!(
            taught,
            golden(name, "taught"),
            "{name} drifted from its taught rendering"
        );
        // Removing the teach lines must restore the frozen output exactly —
        // teaching adds lines and changes nothing else.
        let stripped: String = taught
            .lines()
            .filter(|line| {
                !(line.starts_with("  call shape: ")
                    || line.starts_with("  methods of ")
                    || line.starts_with("      "))
            })
            .map(|line| format!("{line}\n"))
            .collect();
        assert_eq!(stripped, golden(name, "text"), "{name}");
    }
}

#[test]
fn taught_json_differs_from_frozen_only_by_teaches_and_features() {
    for name in TAUGHT {
        let on: serde_json::Value =
            serde_json::from_str(&check(name, &["--json"])).expect("wire output parses");
        let off: serde_json::Value =
            serde_json::from_str(&golden(name, "json")).expect("golden parses");

        let mut scrubbed = on.clone();
        for report in scrubbed.as_array_mut().expect("reports are an array") {
            let map = report.as_object_mut().expect("a report is an object");
            map.remove("features");
            for diagnostic in map["diagnostics"].as_array_mut().expect("array") {
                diagnostic
                    .as_object_mut()
                    .expect("object")
                    .remove("teaches");
            }
        }
        assert_eq!(scrubbed, off, "{name}: more than teaches/features changed");

        // And the removed fields were really there to remove.
        assert_eq!(on[0]["features"][0], "diagnostic_teaching_v1", "{name}");
        assert!(
            on[0]["diagnostics"][0]["teaches"].is_array(),
            "{name}: the first diagnostic should teach"
        );
    }
}

#[test]
fn taught_output_is_deterministic_across_runs() {
    for args in [&[][..], &["--json"][..]] {
        assert_eq!(
            check("xn2003_map", args),
            check("xn2003_map", args),
            "two runs must agree to the byte"
        );
        assert_eq!(
            check_project("teach_module_fat", args),
            check_project("teach_module_fat", args),
            "module-call ranking must agree to the byte across runs"
        );
    }
}

// ------------------------------------------------------- module-call (0012)
//
// The same two contracts, extended to the module-call teach: teaching off
// reproduces the pre-0012 diagnostic — message sentence included, because
// the sentence *is* teaching — byte for byte, and teaching on is pinned by
// its own golden. Project mode prints absolute paths and the host's
// separators, so outputs are normalised to root-relative, `/`-separated
// paths before comparison; every other byte is compared exactly.

/// The three golden cases of design/0012 §1: the rewrite bridge, the
/// multi-position candidate without one, and the fat module whose
/// name-matched function survives displacement.
const MODULE_FIXTURES: &[&str] = &[
    "teach_module_call",
    "teach_module_multi",
    "teach_module_fat",
];

/// Run `xenith check` on a fixture project's entry point and normalise the
/// paths in its output.
fn check_project(name: &str, args: &[&str]) -> String {
    let entry = format!("tests/fixtures/projects/{name}/src/main.xn");
    let output = Command::new(env!("CARGO_BIN_EXE_xenith"))
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .arg("check")
        .args(args)
        .arg(&entry)
        .output()
        .expect("the compiler binary runs");
    normalized(&String::from_utf8(output.stdout).expect("diagnostics are UTF-8"))
}

/// Strip the absolute crate root and unify separators to `/`, leaving every
/// other byte alone. JSON escapes `\` as `\\`, so the doubled spelling is
/// stripped first.
fn normalized(output: &str) -> String {
    let root = env!("CARGO_MANIFEST_DIR");
    let escaped = root.replace('\\', "\\\\");
    output
        .replace(&format!("{escaped}\\\\"), "")
        .replace(&format!("{root}\\"), "")
        .replace(&format!("{root}/"), "")
        .replace("\\\\", "/")
        .replace('\\', "/")
}

/// Compare against a golden under `tests/fixtures/projects/`, or rewrite it
/// when `XENITH_BLESS` is set — the normalisation lives in this file, so the
/// goldens are regenerated through the exact code that verifies them.
fn assert_module_golden(name: &str, kind: &str, actual: &str) {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/projects")
        .join(format!("{name}.{kind}.golden"));
    if std::env::var_os("XENITH_BLESS").is_some() {
        std::fs::write(&path, actual).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        return;
    }
    let golden =
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    assert_eq!(actual, golden, "{name} drifted from {}", path.display());
}

#[test]
fn module_call_teaching_off_is_byte_identical_to_the_pre_teaching_text() {
    for name in MODULE_FIXTURES {
        let off = check_project(name, &["--diagnostic-teaching=off"]);
        assert_module_golden(name, "text", &off);
        assert!(
            !off.contains("module functions"),
            "{name}: off mode must not carry the teaching sentence:\n{off}"
        );
    }
}

#[test]
fn module_call_taught_text_matches_its_snapshot() {
    for name in MODULE_FIXTURES {
        assert_module_golden(name, "taught", &check_project(name, &[]));
    }
    // The three cases, pinned beyond the goldens so a blessed regeneration
    // cannot silently invert them.
    let bridged = check_project("teach_module_call", &[]);
    assert!(
        bridged.contains("rewrite: depot.locker.stow(locker: <receiver>, load: ...)"),
        "{bridged}"
    );
    let multi = check_project("teach_module_multi", &[]);
    assert!(
        multi.contains("depot.locker.transfer(from: depot.locker.Locker, to: depot.locker.Locker)"),
        "{multi}"
    );
    assert!(
        !multi.contains("rewrite:"),
        "two fitting positions must not guess a bridge:\n{multi}"
    );
    let fat = check_project("teach_module_fat", &[]);
    assert!(
        fat.contains("(6 of 7):") && fat.contains("depot.yard.weigh("),
        "the name match survives displacement:\n{fat}"
    );
    assert!(!fat.contains("depot.yard.flood"), "{fat}");
}

#[test]
fn module_call_taught_json_matches_its_snapshot() {
    assert_module_golden(
        "teach_module_call",
        "json",
        &check_project("teach_module_call", &["--json"]),
    );
}

#[test]
fn module_call_json_off_differs_from_on_only_by_the_teaching() {
    for name in MODULE_FIXTURES {
        let on: serde_json::Value =
            serde_json::from_str(&check_project(name, &["--json"])).expect("wire output parses");
        let off: serde_json::Value = serde_json::from_str(&check_project(
            name,
            &["--json", "--diagnostic-teaching=off"],
        ))
        .expect("wire output parses");

        let mut scrubbed = on.clone();
        for report in scrubbed.as_array_mut().expect("reports are an array") {
            let map = report.as_object_mut().expect("a report is an object");
            map.remove("features");
            for diagnostic in map["diagnostics"].as_array_mut().expect("array") {
                let entry = diagnostic.as_object_mut().expect("object");
                entry.remove("teaches");
                // The module-call sentence is teaching too: strip it the way
                // `--diagnostic-teaching=off` does.
                if let Some(serde_json::Value::String(message)) = entry.get_mut("message") {
                    if let Some(cut) = message.find("; module functions are called as") {
                        message.truncate(cut);
                    }
                }
            }
        }
        assert_eq!(scrubbed, off, "{name}: more than the teaching changed");

        // And what was scrubbed was really there.
        let entry = &on[1]["diagnostics"][0];
        assert_eq!(entry["teaches"][0]["kind"], "module_call", "{name}");
        assert!(
            entry["message"]
                .as_str()
                .expect("a message")
                .contains("module functions are called as"),
            "{name}"
        );
        assert_eq!(on[1]["features"][1], "module_call_teach_v1", "{name}");
    }
}
