//! The one request pipeline of design/0013 §1: discovery, containment and
//! mode selection in exactly one place. These tests pin the mode-selection
//! table — every (mode, layout) cell — and the containment rule that a
//! project reaching past the boundary is an error, never a quiet
//! single-file run.

use std::path::{Path, PathBuf};

use xenith_driver::project::{
    ModeRequest, ProjectRequest, ProjectSnapshot, SnapshotError, snapshot,
};

/// A scratch tree, one per test: the suite runs in parallel.
fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("xenith-snapshot-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("writable temp dir");
    dir
}

/// A minimal two-module project under `base/proj`.
fn project_in(base: &Path) -> PathBuf {
    let root = base.join("proj");
    std::fs::create_dir_all(root.join("src/game")).expect("temp tree");
    std::fs::write(root.join("xenith.toml"), "name = \"snap\"\n").expect("manifest");
    std::fs::write(
        root.join("src/main.xn"),
        "use game.dep;\n\nfn main() -> Int {\n    game.dep.one()\n}\n",
    )
    .expect("main");
    std::fs::write(
        root.join("src/game/dep.xn"),
        "pub fn one() -> Int {\n    1\n}\n",
    )
    .expect("dep");
    root
}

fn request(path: &Path, mode: ModeRequest) -> Result<ProjectSnapshot, SnapshotError> {
    snapshot(&ProjectRequest {
        path,
        mode,
        containment: None,
    })
}

#[test]
fn auto_without_a_manifest_is_single_file() {
    let base = scratch("auto-lone");
    let file = base.join("lone.xn");
    std::fs::write(&file, "fn f() -> Int {\n    1\n}\n").unwrap();

    let snapshot = request(&file, ModeRequest::Auto).expect("resolves");
    assert_eq!(snapshot.analysis_mode(), "single_file");
    let ProjectSnapshot::SingleFile { path, source } = snapshot else {
        panic!("a lone file is single-file");
    };
    assert_eq!(path, file);
    assert!(source.contains("fn f()"));
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn auto_inside_a_project_resolves_the_project_and_the_requested_file() {
    let base = scratch("auto-proj");
    let root = project_in(&base);

    let snapshot = request(&root.join("src/game/dep.xn"), ModeRequest::Auto).expect("resolves");
    assert_eq!(snapshot.analysis_mode(), "project");
    let ProjectSnapshot::Project { project, requested } = snapshot else {
        panic!("a source file resolves to its project");
    };
    assert_eq!(project.files.len(), 2);
    let index = requested.expect("the entry maps to a module");
    assert_eq!(project.files[index].rel, "game/dep.xn");
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn auto_with_a_stray_file_next_to_the_manifest_is_single_file() {
    // Not in `src/` means not in the project (design/0010 §2) — a layout
    // fact, not a discovery failure, so `auto` reads the file alone.
    let base = scratch("auto-stray");
    let root = project_in(&base);
    let stray = root.join("scratch.xn");
    std::fs::write(&stray, "fn s() -> Int {\n    2\n}\n").unwrap();

    let snapshot = request(&stray, ModeRequest::Auto).expect("resolves");
    assert_eq!(snapshot.analysis_mode(), "single_file");
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn demanded_project_mode_fails_loudly_without_a_manifest() {
    let base = scratch("demand-none");
    let file = base.join("lone.xn");
    std::fs::write(&file, "fn f() -> Int {\n    1\n}\n").unwrap();

    let Err(error) = request(&file, ModeRequest::Project) else {
        panic!("no manifest must refuse demanded project mode");
    };
    assert!(matches!(error, SnapshotError::NoManifest(_)), "{error}");
    assert!(error.to_string().contains("no `xenith.toml`"), "{error}");
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn demanded_project_mode_fails_loudly_for_a_stray_file() {
    let base = scratch("demand-stray");
    let root = project_in(&base);
    let stray = root.join("scratch.xn");
    std::fs::write(&stray, "fn s() -> Int {\n    2\n}\n").unwrap();

    let Err(error) = request(&stray, ModeRequest::Project) else {
        panic!("a stray file must refuse demanded project mode");
    };
    assert!(matches!(error, SnapshotError::OutsideSources(_)), "{error}");
    assert!(error.to_string().contains("source set"), "{error}");
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn demanded_single_file_mode_stays_single_file_inside_a_project() {
    let base = scratch("demand-single");
    let root = project_in(&base);

    let snapshot = request(&root.join("src/main.xn"), ModeRequest::SingleFile).expect("resolves");
    assert_eq!(snapshot.analysis_mode(), "single_file");
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn a_missing_file_reports_the_path_as_given() {
    let base = scratch("io");
    let missing = base.join("nowhere.xn");
    let Err(error) = request(&missing, ModeRequest::Auto) else {
        panic!("a missing file must not resolve");
    };
    assert!(matches!(error, SnapshotError::Io(_)), "{error}");
    assert!(
        error
            .to_string()
            .starts_with(&missing.display().to_string()),
        "{error}"
    );
    let _ = std::fs::remove_dir_all(&base);
}

// ---------------------------------------------------------------- containment

fn confined(
    path: &Path,
    mode: ModeRequest,
    boundary: &Path,
) -> Result<ProjectSnapshot, SnapshotError> {
    snapshot(&ProjectRequest {
        path,
        mode,
        containment: Some(boundary),
    })
}

#[test]
fn a_relative_path_resolves_against_the_boundary() {
    let base = scratch("confine-rel");
    let root = project_in(&base);

    let snapshot = confined(Path::new("src/main.xn"), ModeRequest::Auto, &root)
        .expect("relative to the boundary");
    assert_eq!(snapshot.analysis_mode(), "project");
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn an_entry_escape_is_refused() {
    let base = scratch("confine-escape");
    let root = project_in(&base);
    let outside = base.join("outside.xn");
    std::fs::write(&outside, "fn o() -> Int {\n    3\n}\n").unwrap();

    let Err(error) = confined(Path::new("../outside.xn"), ModeRequest::Auto, &root) else {
        panic!("an escaping entry must be refused");
    };
    assert!(matches!(error, SnapshotError::Containment(_)), "{error}");
    assert!(
        error.to_string().contains("outside the workspace root"),
        "{error}"
    );
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn a_project_reaching_above_the_boundary_is_an_error_not_a_fallback() {
    // The boundary is `proj/src`; the manifest sits above it. Discovery
    // finds the project, the project escapes the boundary — and the answer
    // must be a containment error, never a silent single-file run
    // (design/0013 §1: the root cause of the two-truths bug).
    let base = scratch("confine-above");
    let root = project_in(&base);
    let boundary = root.join("src");

    let Err(error) = confined(Path::new("main.xn"), ModeRequest::Auto, &boundary) else {
        panic!("a project past the boundary must be refused, not degraded");
    };
    assert!(matches!(error, SnapshotError::Containment(_)), "{error}");
    assert!(
        error.to_string().contains("outside the workspace root"),
        "{error}"
    );

    // And demanding single-file mode is the sanctioned way to look at the
    // file alone under that boundary.
    let snapshot = confined(Path::new("main.xn"), ModeRequest::SingleFile, &boundary)
        .expect("single-file mode reads just the file");
    assert_eq!(snapshot.analysis_mode(), "single_file");
    let _ = std::fs::remove_dir_all(&base);
}
