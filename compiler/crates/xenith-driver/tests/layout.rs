//! The layout rules of design/0010 §2, over the pure path-indexing core.
//!
//! Case collisions are checked here and only here: a Windows or macOS disk
//! cannot hold `Game.xn` next to `game.xn`, so the rule is only testable as
//! a function over path lists — which is why it is one.

use xenith_driver::project::{discover, index_sources};

fn index(paths: &[&str]) -> (Vec<(String, String)>, Vec<String>) {
    let owned: Vec<String> = paths.iter().map(|p| p.to_string()).collect();
    let (modules, problems) = index_sources(&owned);
    (
        modules,
        problems
            .iter()
            .map(|(_, d)| d.code.id().to_string())
            .collect(),
    )
}

#[test]
fn nested_paths_map_to_dotted_module_ids() {
    let (modules, problems) = index(&["main.xn", "game/player.xn", "game/scores.xn"]);
    assert!(problems.is_empty(), "{problems:?}");
    let ids: Vec<&str> = modules.iter().map(|(m, _)| m.as_str()).collect();
    assert_eq!(
        ids,
        ["game.player", "game.scores", "main"],
        "sorted, dotted"
    );
}

#[test]
fn a_segment_that_is_not_lower_snake_is_refused() {
    let (modules, problems) = index(&["player-v2.xn"]);
    assert!(modules.is_empty());
    assert_eq!(problems, ["XN7001"]);
    // Uppercase anywhere is the same refusal, directories included.
    assert_eq!(index(&["Game/level.xn"]).1, ["XN7001"]);
    assert_eq!(index(&["game/Level.xn"]).1, ["XN7001"]);
}

#[test]
fn case_only_differences_are_refused_on_every_host() {
    let (modules, problems) = index(&["Game.xn", "game.xn"]);
    assert!(modules.is_empty());
    // The first spelling fails the segment rule; the second is refused as a
    // collision rather than silently winning on a case-blind disk.
    assert_eq!(problems, ["XN7001", "XN7002"]);
}

#[test]
fn the_std_root_is_reserved() {
    assert_eq!(index(&["std.xn"]).1, ["XN7005"]);
    assert_eq!(index(&["std/io.xn"]).1, ["XN7005"]);
    // `stdlib` is a different name, not a claim on the root.
    assert!(index(&["stdlib.xn"]).1.is_empty());
}

#[test]
fn discover_walks_up_to_the_manifest_and_no_further() {
    let base = std::env::temp_dir().join(format!("xenith-layout-{}", std::process::id()));
    let nested = base.join("proj").join("src").join("game");
    std::fs::create_dir_all(&nested).expect("temp tree");
    std::fs::write(base.join("proj").join("xenith.toml"), "name = \"t\"\n").expect("manifest");
    std::fs::write(nested.join("player.xn"), "pub fn p() -> Int {\n    1\n}\n").expect("file");

    let found = discover(&nested.join("player.xn")).expect("the manifest is two levels up");
    assert_eq!(
        found.file_name().and_then(|n| n.to_str()),
        Some("proj"),
        "{found:?}"
    );
    assert!(
        discover(&base).is_none(),
        "no manifest above the base means single-file mode"
    );

    let _ = std::fs::remove_dir_all(&base);
}
