//! Module resolution, visibility and the use rules (design/0010 §1, §3, §4)
//! — straight through `analyze_project`, no file system involved.

use xenith_sema::{ModuleUnit, analyze_project};
use xenith_syntax::parse;

/// Analyze named modules; returns per-module (code, message) lists in input
/// order. Fixtures must parse cleanly so every diagnostic is semantic.
fn analyze(modules: &[(&str, &str)]) -> Vec<Vec<(String, String)>> {
    let parsed: Vec<(String, xenith_syntax::Parsed)> = modules
        .iter()
        .map(|(path, source)| (path.to_string(), parse(source)))
        .collect();
    for (path, p) in &parsed {
        assert!(
            p.diagnostics.is_empty(),
            "module `{path}` must parse cleanly: {:?}",
            p.diagnostics
        );
    }
    let units: Vec<ModuleUnit> = parsed
        .iter()
        .map(|(path, p)| ModuleUnit {
            path: path.clone(),
            module: &p.module,
        })
        .collect();
    analyze_project(&units)
        .diagnostics
        .iter()
        .map(|per| {
            per.iter()
                .map(|d| (d.code.id().to_string(), d.message.clone()))
                .collect()
        })
        .collect()
}

fn codes(per: &[(String, String)]) -> Vec<&str> {
    per.iter().map(|(c, _)| c.as_str()).collect()
}

const PLAYER: &str = "pub struct Player {\n    name: String,\n    var score: Int,\n}\n\n\
                      pub fn make(name: String) -> Player {\n    Player { name: name, score: 0 }\n}\n\n\
                      fn secret() -> Int {\n    7\n}\n";

// ---------------------------------------------------------------- resolution

#[test]
fn a_used_modules_pub_items_resolve_fully_qualified() {
    let found = analyze(&[
        ("game.player", PLAYER),
        (
            "main",
            "use game.player;\n\n\
             fn main() -> Int {\n    \
                 let p = game.player.make(name: \"ada\");\n    \
                 let q = game.player.Player { name: \"grace\", score: 1 };\n    \
                 p.score + q.score\n}\n",
        ),
    ]);
    assert!(found.iter().all(|per| per.is_empty()), "{found:#?}");
}

#[test]
fn a_qualified_reference_without_use_names_the_missing_use() {
    let found = analyze(&[
        ("game.player", PLAYER),
        (
            "main",
            "fn main() -> Int {\n    let p = game.player.make(name: \"ada\");\n    p.score\n}\n",
        ),
    ]);
    let (code, message) = &found[1][0];
    assert_eq!(code, "XN2007");
    assert!(message.contains("add `use game.player;`"), "{message}");
}

#[test]
fn a_use_naming_no_module_is_refused() {
    let found = analyze(&[(
        "main",
        "use game.mystery;\n\nfn main() -> Int {\n    1\n}\n",
    )]);
    assert_eq!(codes(&found[0]), ["XN2007"]);
}

#[test]
fn an_unused_use_is_a_hard_error() {
    let found = analyze(&[
        ("game.player", PLAYER),
        ("main", "use game.player;\n\nfn main() -> Int {\n    1\n}\n"),
    ]);
    let (code, message) = &found[1][0];
    assert_eq!(code, "XN2009");
    assert!(message.contains("never used"), "{message}");
}

#[test]
fn a_duplicate_use_is_refused() {
    let found = analyze(&[
        ("game.player", PLAYER),
        (
            "main",
            "use game.player;\nuse game.player;\n\n\
             fn main() -> Int {\n    game.player.make(name: \"a\").score\n}\n",
        ),
    ]);
    assert_eq!(codes(&found[1]), ["XN2010"]);
}

// ---------------------------------------------------------------- visibility

#[test]
fn a_private_item_is_inaccessible_across_the_boundary() {
    let found = analyze(&[
        ("game.player", PLAYER),
        (
            "main",
            "use game.player;\n\nfn main() -> Int {\n    game.player.secret()\n}\n",
        ),
    ]);
    let (code, message) = &found[1][0];
    assert_eq!(code, "XN2008");
    assert!(message.contains("private to `game.player`"), "{message}");
}

#[test]
fn a_pub_signature_may_not_mention_a_private_type() {
    let found = analyze(&[(
        "game.player",
        "struct Hidden {\n    value: Int,\n}\n\n\
         pub fn leak() -> Hidden {\n    Hidden { value: 1 }\n}\n",
    )]);
    let (code, message) = &found[0][0];
    assert_eq!(code, "XN7007");
    assert!(message.contains("game.player.Hidden"), "{message}");
}

#[test]
fn a_pub_struct_field_may_not_mention_a_private_type() {
    let found = analyze(&[(
        "game.player",
        "struct Hidden {\n    value: Int,\n}\n\n\
         pub struct Wrapper {\n    inner: Hidden,\n}\n",
    )]);
    assert_eq!(codes(&found[0]), ["XN7007"]);
}

#[test]
fn a_cross_module_field_assignment_is_refused_var_or_not() {
    let found = analyze(&[
        ("game.player", PLAYER),
        (
            "main",
            "use game.player;\n\n\
             fn main() -> Int {\n    \
                 var p = game.player.make(name: \"ada\");\n    \
                 p.score = 99;\n    p.score\n}\n",
        ),
    ]);
    let (code, message) = &found[1][0];
    assert_eq!(code, "XN7008");
    assert!(
        message.contains("cannot be assigned from outside `game.player`"),
        "{message}"
    );
}

#[test]
fn cross_module_reads_construction_and_matching_stay_open() {
    let found = analyze(&[
        (
            "game.player",
            "pub struct Player {\n    name: String,\n    var score: Int,\n}\n\n\
             pub enum Rank {\n    Bronze,\n    Gold,\n}\n",
        ),
        (
            "main",
            "use game.player;\n\n\
             fn main() -> Int {\n    \
                 let p = game.player.Player { name: \"ada\", score: 5 };\n    \
                 let r = game.player.Rank.Gold;\n    \
                 let bonus = match r {\n        \
                     game.player.Rank.Gold => 10,\n        \
                     game.player.Rank.Bronze => 0,\n    };\n    \
                 match p {\n        \
                     game.player.Player { score } => score + bonus,\n    }\n}\n",
        ),
    ]);
    assert!(found.iter().all(|per| per.is_empty()), "{found:#?}");
}

#[test]
fn a_mutating_method_through_a_foreign_field_is_refused() {
    let found = analyze(&[
        (
            "game.deck",
            "pub struct Deck {\n    var cards: List<Int>,\n}\n\n\
             pub fn fresh() -> Deck {\n    Deck { cards: [1] }\n}\n",
        ),
        (
            "main",
            "use game.deck;\n\n\
             fn main() -> Int {\n    \
                 var d = game.deck.fresh();\n    \
                 d.cards.push(item: 2);\n    d.cards.len()\n}\n",
        ),
    ]);
    assert_eq!(codes(&found[1]), ["XN7008"]);
}

// ----------------------------------------------------------------- use-fix

#[test]
fn a_bare_name_matching_one_pub_item_gets_the_use_fix() {
    let parsed_player = parse(PLAYER);
    let parsed_main = parse("fn main() -> Int {\n    make(name: \"ada\").score\n}\n");
    let units = [
        ModuleUnit {
            path: "game.player".to_string(),
            module: &parsed_player.module,
        },
        ModuleUnit {
            path: "main".to_string(),
            module: &parsed_main.module,
        },
    ];
    let analysis = analyze_project(&units);
    let diagnostic = &analysis.diagnostics[1][0];
    assert_eq!(diagnostic.code.id(), "XN2002");
    assert!(
        diagnostic
            .message
            .contains("`use game.player;` would provide it"),
        "{}",
        diagnostic.message
    );
    let fix = diagnostic
        .fix
        .as_ref()
        .expect("the unique match earns a fix");
    assert_eq!(fix.edits[0].replacement, "use game.player;\n\n");
    assert_eq!(
        fix.edits[0].span.start, 0,
        "no uses yet, so the insertion is at the top"
    );
}

#[test]
fn several_pub_matches_list_candidates_without_a_fix() {
    let a = parse("pub fn best() -> Int {\n    1\n}\n");
    let b = parse("pub fn best() -> Int {\n    2\n}\n");
    let main = parse("fn main() -> Int {\n    best()\n}\n");
    let units = [
        ModuleUnit {
            path: "game.scores".to_string(),
            module: &a.module,
        },
        ModuleUnit {
            path: "game.stats".to_string(),
            module: &b.module,
        },
        ModuleUnit {
            path: "main".to_string(),
            module: &main.module,
        },
    ];
    let analysis = analyze_project(&units);
    let diagnostic = &analysis.diagnostics[2][0];
    assert_eq!(diagnostic.code.id(), "XN2002");
    assert!(diagnostic.fix.is_none(), "ambiguity earns no fix");
    let teach = &diagnostic.teaches[0];
    assert_eq!(teach.kind, xenith_diag::TeachKind::UseCandidates);
    assert_eq!(teach.items[0].signature, "use game.scores;");
    assert_eq!(teach.items[1].signature, "use game.stats;");
}

#[test]
fn the_use_fix_inserts_in_dictionary_order_among_existing_uses() {
    let a = parse("pub fn early() -> Int {\n    1\n}\n");
    let z = parse("pub fn late() -> Int {\n    2\n}\n");
    let main = parse("use zoo;\n\nfn main() -> Int {\n    early() + zoo.late()\n}\n");
    let units = [
        ModuleUnit {
            path: "alpha".to_string(),
            module: &a.module,
        },
        ModuleUnit {
            path: "zoo".to_string(),
            module: &z.module,
        },
        ModuleUnit {
            path: "main".to_string(),
            module: &main.module,
        },
    ];
    let analysis = analyze_project(&units);
    let diagnostic = analysis.diagnostics[2]
        .iter()
        .find(|d| d.code.id() == "XN2002")
        .expect("the bare `early` is unknown");
    let fix = diagnostic.fix.as_ref().expect("unique match");
    assert_eq!(fix.edits[0].replacement, "use alpha;\n");
    assert_eq!(fix.edits[0].span.start, 0, "before `use zoo;`");
}

// -------------------------------------------------------------------- cycles

#[test]
fn import_cycles_between_modules_check_cleanly() {
    // Xenith has no module initialisers, so mutual `use` carries no
    // execution-order question (design/0010 §5).
    let found = analyze(&[
        (
            "alpha",
            "use beta;

pub struct Alpha {
    next: Option<beta.Beta>,
}

             pub fn depth(a: Alpha) -> Int {
    match a.next {
                     Some(b) => 1 + beta.deeper(b: b),
        None => 1,
    }
}
",
        ),
        (
            "beta",
            "use alpha;

pub struct Beta {
    next: Option<alpha.Alpha>,
}

             pub fn deeper(b: Beta) -> Int {
    match b.next {
                     Some(a) => 1 + alpha.depth(a: a),
        None => 1,
    }
}
",
        ),
    ]);
    assert!(found.iter().all(|per| per.is_empty()), "{found:#?}");
}

#[test]
fn a_value_cycle_across_modules_is_refused() {
    let found = analyze(&[
        (
            "alpha",
            "use beta;

pub struct Alpha {
    b: beta.Beta,
}
",
        ),
        (
            "beta",
            "use alpha;

pub struct Beta {
    a: alpha.Alpha,
}
",
        ),
    ]);
    let (code, message) = &found[0][0];
    assert_eq!(code, "XN3011");
    assert!(
        message.contains("alpha.Alpha -> beta.Beta -> alpha.Alpha"),
        "{message}"
    );
    assert!(found[1].is_empty(), "one cycle, one diagnostic: {found:#?}");
}

#[test]
fn own_functions_render_bare_in_arity_errors() {
    let found = analyze(&[(
        "game.util",
        "fn helper(a: Int, b: Int) -> Int {
    a + b
}

         pub fn caller() -> Int {
    helper(1)
}
",
    )]);
    let (code, message) = &found[0][0];
    assert_eq!(code, "XN3002");
    assert!(
        message.contains("`helper` takes"),
        "own names stay bare: {message}"
    );
    assert!(!message.contains("game.util.helper"), "{message}");
}

// ------------------------------------------------------------- entry + misc

#[test]
fn fn_main_outside_the_main_module_is_refused() {
    let found = analyze(&[("game.player", "fn main() -> Int {\n    1\n}\n")]);
    assert_eq!(codes(&found[0]), ["XN7004"]);
}

#[test]
fn a_parent_item_clashing_with_a_child_module_is_refused() {
    let found = analyze(&[
        ("game", "pub fn player() -> Int {\n    1\n}\n"),
        ("game.player", "pub fn level() -> Int {\n    2\n}\n"),
    ]);
    let (code, message) = &found[0][0];
    assert_eq!(code, "XN7003");
    assert!(message.contains("exclusive under one parent"), "{message}");
}

#[test]
fn a_qualified_type_annotation_resolves_through_use() {
    let found = analyze(&[
        ("game.player", PLAYER),
        (
            "game.scores",
            "use game.player;\n\n\
             pub fn value(player: game.player.Player) -> Int {\n    player.score\n}\n",
        ),
    ]);
    assert!(found.iter().all(|per| per.is_empty()), "{found:#?}");
}
