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
    }
}
