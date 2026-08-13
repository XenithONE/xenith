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
fn a_qualified_generic_struct_and_unit_variant_construct_from_the_annotation() {
    // Expected-type seeding reaches the qualified spellings too, so a
    // generic type declared in one module is constructible from another.
    let found = analyze(&[
        (
            "game.boxes",
            "pub struct Boxed<T> {\n    item: T,\n}\n\n\
             pub enum Slot<T> {\n    Empty,\n    Filled(T),\n}\n",
        ),
        (
            "main",
            "use game.boxes;\n\n\
             fn main() -> Int {\n    \
                 let b: game.boxes.Boxed<Int> = game.boxes.Boxed { item: 7 };\n    \
                 let s: game.boxes.Slot<Int> = game.boxes.Slot.Empty;\n    \
                 b.item\n}\n",
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
fn a_pub_const_reads_across_the_boundary_and_a_private_one_does_not() {
    let found = analyze(&[
        (
            "game.limits",
            "pub const CEILING: Int = 999;\nconst SECRET: Int = 42;\n",
        ),
        (
            "main",
            "use game.limits;\n\nfn main() -> Int {\n    game.limits.CEILING\n}\n",
        ),
    ]);
    assert!(found[1].is_empty(), "{:#?}", found[1]);

    let found = analyze(&[
        (
            "game.limits",
            "pub const CEILING: Int = 999;\nconst SECRET: Int = 42;\n",
        ),
        (
            "main",
            "use game.limits;\n\nfn main() -> Int {\n    game.limits.SECRET\n}\n",
        ),
    ]);
    let (code, message) = &found[1][0];
    assert_eq!(code, "XN2008");
    assert!(message.contains("private to `game.limits`"), "{message}");
}

#[test]
fn a_const_takes_a_method_call_across_the_boundary() {
    // `game.limits.CEILING.to_text()` reads as const-then-method, not as
    // enum-then-variant — the two spellings are the same shape.
    let found = analyze(&[
        ("game.limits", "pub const CEILING: Int = 999;\n"),
        (
            "main",
            "use game.limits;\n\nfn main() -> String {\n    game.limits.CEILING.to_text()\n}\n",
        ),
    ]);
    assert!(found[1].is_empty(), "{:#?}", found[1]);
}

#[test]
fn a_bare_pub_const_earns_the_use_fix() {
    let found = analyze(&[
        ("game.limits", "pub const CEILING: Int = 999;\n"),
        ("main", "fn main() -> Int {\n    CEILING\n}\n"),
    ]);
    let (code, message) = &found[1][0];
    assert_eq!(code, "XN2002");
    assert!(message.contains("use game.limits;"), "{message}");
}

#[test]
fn a_pub_const_may_not_mention_a_private_type() {
    let found = analyze(&[(
        "game.limits",
        "struct Hidden {\n    value: Int,\n}\n\npub const H: Hidden = 1;\n",
    )]);
    assert!(codes(&found[0]).contains(&"XN7007"), "{:#?}", found[0]);
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

// --------------------------------------------------------- module-call teach

/// Like `analyze`, but with the diagnostics kept whole so teaches and their
/// structure can be asserted.
fn analyze_diagnostics(modules: &[(&str, &str)]) -> Vec<Vec<xenith_diag::Diagnostic>> {
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
    analyze_project(&units).diagnostics
}

const LOCKER: &str = "pub struct Locker {\n    id: Int,\n    var load: Int,\n}\n\n\
                      pub fn make(id: Int) -> Locker {\n    Locker { id: id, load: 0 }\n}\n\n\
                      pub fn stow(locker: Locker, load: Int) -> Int {\n    locker.load + load\n}\n\n\
                      pub fn peek(locker: Locker) -> Int {\n    locker.load\n}\n\n\
                      pub fn transfer(from: Locker, to: Locker) -> Int {\n    from.load + to.load\n}\n\n\
                      fn hidden(locker: Locker) -> Int {\n    locker.id\n}\n";

#[test]
fn an_unknown_member_on_a_module_type_teaches_the_rewrite_bridge() {
    use xenith_diag::TeachKind;
    let found = analyze_diagnostics(&[
        ("depot.locker", LOCKER),
        (
            "main",
            "use depot.locker;\n\n\
             fn main() -> Int {\n    \
                 let locker = depot.locker.make(id: 7);\n    \
                 locker.stow(load: 12)\n}\n",
        ),
    ]);
    let diagnostic = &found[1][0];
    assert_eq!(diagnostic.code.id(), "XN2003");
    // The body itself steers away from the method prior (design/0012 §1).
    assert_eq!(
        diagnostic.message,
        "`depot.locker.Locker` has no method named `stow`; \
         module functions are called as `depot.locker.stow(...)`"
    );
    let teach = &diagnostic.teaches[0];
    assert_eq!(teach.kind, TeachKind::ModuleCall);
    assert_eq!(teach.type_name, "depot.locker.Locker");
    assert_eq!(teach.total_items, 3);
    assert!(!teach.truncated);

    // The name match ranks first and carries the full bridge.
    let stow = &teach.items[0];
    assert_eq!(stow.name, "depot.locker.stow");
    assert_eq!(
        stow.signature,
        "depot.locker.stow(locker: depot.locker.Locker, load: Int) -> Int"
    );
    assert_eq!(stow.receiver_parameter.as_deref(), Some("locker"));
    assert_eq!(
        stow.rewrite.as_deref(),
        Some("depot.locker.stow(locker: <receiver>, load: ...)")
    );

    // Then first-parameter matches in fully-qualified name order.
    assert_eq!(teach.items[1].name, "depot.locker.peek");
    assert_eq!(teach.items[2].name, "depot.locker.transfer");

    // Return-only (`make`) and private (`hidden`) functions never appear.
    assert!(
        teach.items.iter().all(|i| i.name != "depot.locker.make"),
        "return-only matches are excluded: {:#?}",
        teach.items
    );
    assert!(
        teach.items.iter().all(|i| i.name != "depot.locker.hidden"),
        "private functions are excluded: {:#?}",
        teach.items
    );
}

#[test]
fn a_multi_position_candidate_gets_a_signature_without_a_rewrite() {
    let found = analyze_diagnostics(&[
        ("depot.locker", LOCKER),
        (
            "main",
            "use depot.locker;\n\n\
             fn main() -> Int {\n    \
                 let a = depot.locker.make(id: 1);\n    \
                 let b = depot.locker.make(id: 2);\n    \
                 a.transfer(to: b)\n}\n",
        ),
    ]);
    let diagnostic = &found[1][0];
    assert_eq!(diagnostic.code.id(), "XN2003");
    let transfer = &diagnostic.teaches[0].items[0];
    assert_eq!(transfer.name, "depot.locker.transfer");
    assert_eq!(
        transfer.signature,
        "depot.locker.transfer(from: depot.locker.Locker, to: depot.locker.Locker) -> Int"
    );
    assert!(
        transfer.receiver_parameter.is_none() && transfer.rewrite.is_none(),
        "two fitting positions must not guess a bridge: {transfer:#?}"
    );
}

#[test]
fn a_name_match_is_never_displaced_out_of_the_budget() {
    // Seven candidates, six slots. Sorted by name alone, `weigh` would be
    // seventh and vanish; the name-match tier keeps it first, and the budget
    // stays at six (design/0012 §1: the displacement invariant).
    let yard = "pub struct Yard {\n    var mass: Int,\n}\n\n\
                pub fn make() -> Yard {\n    Yard { mass: 0 }\n}\n\n\
                pub fn annex(yard: Yard) -> Int {\n    yard.mass\n}\n\n\
                pub fn budge(yard: Yard) -> Int {\n    yard.mass\n}\n\n\
                pub fn clear(yard: Yard) -> Int {\n    yard.mass\n}\n\n\
                pub fn drain(yard: Yard) -> Int {\n    yard.mass\n}\n\n\
                pub fn evict(yard: Yard) -> Int {\n    yard.mass\n}\n\n\
                pub fn flood(yard: Yard) -> Int {\n    yard.mass\n}\n\n\
                pub fn weigh(yard: Yard, mass: Int) -> Int {\n    yard.mass + mass\n}\n";
    let found = analyze_diagnostics(&[
        ("depot.yard", yard),
        (
            "main",
            "use depot.yard;\n\n\
             fn main() -> Int {\n    \
                 let yard = depot.yard.make();\n    \
                 yard.weigh(mass: 3)\n}\n",
        ),
    ]);
    let teach = &found[1][0].teaches[0];
    assert_eq!(teach.items.len(), 6);
    assert_eq!(teach.total_items, 7);
    assert!(teach.truncated, "the omission is structural, not silent");
    let names: Vec<&str> = teach.items.iter().map(|i| i.name.as_str()).collect();
    assert_eq!(
        names,
        [
            "depot.yard.weigh",
            "depot.yard.annex",
            "depot.yard.budge",
            "depot.yard.clear",
            "depot.yard.drain",
            "depot.yard.evict",
        ],
        "name match first, then first-parameter matches in name order"
    );
}

#[test]
fn the_module_call_dedup_key_is_the_type_and_member_pair() {
    let found = analyze_diagnostics(&[
        ("depot.locker", LOCKER),
        (
            "main",
            "use depot.locker;\n\n\
             fn main() -> Int {\n    \
                 let a = depot.locker.make(id: 1);\n    \
                 let b = depot.locker.make(id: 2);\n    \
                 a.stow(load: 1);\n    \
                 b.stow(load: 2);\n    \
                 a.peek_at();\n    1\n}\n",
        ),
    ]);
    let taught: Vec<bool> = found[1].iter().map(|d| !d.teaches.is_empty()).collect();
    assert_eq!(
        taught,
        [true, false, true],
        "same (type, member) teaches once; a new member earns its own bridge: {:#?}",
        found[1]
    );
    // The message correction is not budgeted: every module XN2003 carries it.
    for diagnostic in &found[1] {
        assert!(
            diagnostic
                .message
                .contains("module functions are called as"),
            "{}",
            diagnostic.message
        );
    }
}

#[test]
fn a_type_variable_receiver_attaches_no_module_teaching() {
    let found = analyze_diagnostics(&[(
        "main",
        "fn main() -> Int {\n    helper(x: 1)\n}\n\n\
         fn helper<T>(x: T) -> Int {\n    x.stow()\n}\n",
    )]);
    let diagnostic = &found[0][0];
    assert_eq!(diagnostic.code.id(), "XN2003");
    assert_eq!(
        diagnostic.message, "`T` has no method named `stow`",
        "no false bridge for a type variable"
    );
    assert!(diagnostic.teaches.is_empty());
}

#[test]
fn a_prelude_receiver_keeps_its_method_catalogue_in_project_mode() {
    use xenith_diag::TeachKind;
    let found = analyze_diagnostics(&[(
        "main",
        "fn main() -> Int {\n    let xs = [1];\n    xs.size()\n}\n",
    )]);
    let diagnostic = &found[0][0];
    assert_eq!(diagnostic.code.id(), "XN2003");
    assert!(
        !diagnostic.message.contains("module functions"),
        "prelude XN2003 text is unchanged: {}",
        diagnostic.message
    );
    assert_eq!(diagnostic.teaches[0].kind, TeachKind::AvailableMethods);
}

#[test]
fn module_call_teaches_share_the_run_budget() {
    let found = analyze_diagnostics(&[
        ("depot.locker", LOCKER),
        (
            "main",
            "use depot.locker;\n\n\
             fn main() -> Int {\n    \
                 let a = depot.locker.make(id: 1);\n    \
                 a.m1();\n    a.m2();\n    a.m3();\n    a.m4();\n    a.m5();\n    a.m6();\n    1\n}\n",
        ),
    ]);
    assert_eq!(found[1].len(), 6);
    let taught = found[1].iter().filter(|d| !d.teaches.is_empty()).count();
    assert_eq!(taught, 5, "the 0009 total budget applies unchanged");
    assert!(found[1][5].teaches.is_empty(), "the sixth arrives too late");
}

#[test]
fn an_own_module_receiver_teaches_the_bare_spelling() {
    let source =
        format!("{LOCKER}\npub fn poke(locker: Locker) -> Int {{\n    locker.peek_hard()\n}}\n");
    let found = analyze_diagnostics(&[("depot.locker", &source)]);
    let diagnostic = &found[0][0];
    assert_eq!(diagnostic.code.id(), "XN2003");
    assert!(
        diagnostic
            .message
            .ends_with("; module functions are called as `peek_hard(...)`"),
        "own items stay bare: {}",
        diagnostic.message
    );
    let teach = &diagnostic.teaches[0];
    // The callee stays fully qualified — it is an identity, not a spelling —
    // while the signature and rewrite use the module's own bare form.
    assert_eq!(teach.items[0].name, "depot.locker.peek");
    assert_eq!(
        teach.items[0].signature,
        "peek(locker: depot.locker.Locker) -> Int"
    );
    assert_eq!(
        teach.items[0].rewrite.as_deref(),
        Some("peek(locker: <receiver>)")
    );
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
