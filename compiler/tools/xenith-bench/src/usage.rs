//! The usage audit and the three-way outcome classifier (design/0016 §0).
//!
//! The tier-6 campaign judged a run by stdout alone. Four reviewers of the
//! 0016 draft independently found the same hole: **a sequential program
//! prints the same bytes as a task program**, so "green" cannot mean "the
//! model reached for the feature". 0016 §0 closes it by adding a second
//! layer — the *usage* audit — beside 0012's delivery audit: the submitted
//! source itself is machine-read for the task vocabulary, and a green run
//! only counts as tier-7 pass@1 when the program that printed the right
//! bytes actually spawned something.
//!
//! Three properties this module keeps deliberately:
//!
//! - **It is text, not a parse.** Most measured attempts are the failing
//!   ones, and a failing attempt frequently does not parse. An audit that
//!   needed a syntax tree would go blind exactly on the rounds the
//!   `used-wrong` class exists to count. Comments and literals are masked
//!   first, so `io.write(text: "spawn")` is not a spawn.
//! - **It is pure.** [`classify`] takes the submitted text and the round's
//!   green/not-green verdict and returns a class; no clocks, no files, no
//!   compiler. That is what makes it testable against fabricated inputs —
//!   including the sequential program that computes the right answer, the
//!   exact case that motivated the RFC.
//! - **It is total.** The RFC names three classes because three are what it
//!   reports, but the audit crosses two booleans, so the fourth cell
//!   (no task syntax, not green) exists whether or not it is named. It is
//!   [`RunClass::BypassedRed`], and `summarize` prints it so the class
//!   columns sum to the number of runs instead of quietly losing some.

/// What one submitted program says about its own use of the task surface.
/// Every field is recorded per round in the ledger (0016 §0), so the
/// post-hoc analysis can ask questions this file did not anticipate.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct UsageAudit {
    /// A `scope` keyword appears.
    pub scope: bool,
    /// `spawn` keywords. The classifier keys on this and nothing else:
    /// `scope {}` alone is inert, and `.await` cannot occur without a Join.
    pub spawns: usize,
    /// `.await` postfixes. Compared against `spawns` this is the RFC's
    /// "await count vs spawn count" — a Join short of an await is XN6008,
    /// a Join awaited twice is XN6006, and both are used-wrong, not bypass.
    pub awaits: usize,
    /// The callee path of each `spawn`, in source order. Shorter than
    /// `spawns` only when a `spawn` is followed by something that is not a
    /// path at all — itself a used-wrong shape (XN6004).
    pub spawned_callees: Vec<String>,
    /// Spawned callees whose `fn` declaration in this same file carries a
    /// non-empty `uses` set — the XN6002 shape, "I put the effect in the
    /// child". Distinct from unresolved: this one was found and was impure.
    pub impure_callees: Vec<String>,
    /// Spawned callees with no `fn` declaration in this file (a typo, a
    /// method form, or a cross-module path). Recorded rather than folded
    /// into `impure_callees`: absence of evidence is not impurity.
    pub unresolved_callees: Vec<String>,
    /// Which of the four `List` combinators appear, in the guide's order.
    /// The closure-era comparison of 0014: tier-6 showed models reach for
    /// `map`/`filter`/`fold` unprompted, and tier-7 asks whether the task
    /// vocabulary reaches the same prior.
    pub list_combinators: Vec<String>,
}

/// The four `List` combinators, in the field guide's order.
const COMBINATORS: [&str; 4] = ["map", "filter", "fold", "find"];

impl UsageAudit {
    /// Audit one submitted source text. Pure: same bytes in, same audit out.
    pub fn of(source: &str) -> UsageAudit {
        let text = mask_comments_and_literals(source);
        let mut audit = UsageAudit {
            scope: !keyword_offsets(&text, "scope").is_empty(),
            spawns: keyword_offsets(&text, "spawn").len(),
            awaits: count_awaits(&text),
            ..UsageAudit::default()
        };
        for path in spawned_callees(&text) {
            // `a.b.f` is declared as `fn f` wherever it lives; the last
            // segment is the name a declaration search can match.
            let name = path.rsplit('.').next().unwrap_or(&path).to_string();
            match declared_uses(&text, &name) {
                Some(Uses::NonEmpty) => push_once(&mut audit.impure_callees, &path),
                Some(Uses::Empty) => {}
                None => push_once(&mut audit.unresolved_callees, &path),
            }
            audit.spawned_callees.push(path);
        }
        for name in COMBINATORS {
            if is_method_called(&text, name) {
                audit.list_combinators.push(name.to_string());
            }
        }
        audit
    }

    /// Did this program reach for the task surface at all? A `spawn` is the
    /// whole test: `scope { }` with nothing in it is not a task, and an
    /// `.await` without a `spawn` has no Join to consume. Deliberately not
    /// "and it did so correctly" — a wrong spawn is [`RunClass::UsedWrong`],
    /// which is a different finding from never trying (0016 §0).
    pub fn uses_tasks(&self) -> bool {
        self.spawns > 0
    }

    /// The 0016 §0 class of a run whose final program this is.
    pub fn classify(&self, green: bool) -> RunClass {
        match (self.uses_tasks(), green) {
            (true, true) => RunClass::UsedGreen,
            (true, false) => RunClass::UsedWrong,
            (false, true) => RunClass::BypassedGreen,
            (false, false) => RunClass::BypassedRed,
        }
    }
}

/// The 0016 §0 outcome classes. `UsedGreen` is the only one the tier-7
/// pass@1 column counts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RunClass {
    /// Task syntax present and stdout matched — (C), the measurement.
    UsedGreen,
    /// Task syntax present, but diagnostics or the wrong bytes — (B).
    UsedWrong,
    /// stdout matched with no task syntax at all — (A). A result about the
    /// prior, not about aptitude.
    BypassedGreen,
    /// Neither: no task syntax, and not green. Unnamed by the RFC, printed
    /// anyway so the columns account for every run.
    BypassedRed,
}

impl RunClass {
    /// Every class, in the order the summary columns use.
    pub const ALL: [RunClass; 4] = [
        RunClass::UsedGreen,
        RunClass::UsedWrong,
        RunClass::BypassedGreen,
        RunClass::BypassedRed,
    ];

    /// The class's column index. Spelled out rather than cast from the
    /// discriminant, so reordering the variants cannot silently reshuffle
    /// the reported numbers.
    pub fn index(self) -> usize {
        match self {
            RunClass::UsedGreen => 0,
            RunClass::UsedWrong => 1,
            RunClass::BypassedGreen => 2,
            RunClass::BypassedRed => 3,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            RunClass::UsedGreen => "used-green",
            RunClass::UsedWrong => "used-wrong",
            RunClass::BypassedGreen => "bypassed-green",
            RunClass::BypassedRed => "bypassed-red",
        }
    }
}

/// The classifier, as one pure function of the two things it is allowed to
/// see: the submitted source and whether that round was green.
pub fn classify(source: &str, green: bool) -> RunClass {
    UsageAudit::of(source).classify(green)
}

// ------------------------------------------------------------------ scanning

/// Replace every comment and literal character with a space. Xenith has
/// line comments only (`//`, `///`) and `"…"` / `'…'` literals with
/// backslash escapes, so this is the whole masking job. Byte offsets are
/// not preserved and are never used against the original text.
fn mask_comments_and_literals(source: &str) -> String {
    let mut out = String::with_capacity(source.len());
    let mut chars = source.chars().peekable();
    while let Some(ch) = chars.next() {
        match ch {
            '/' if chars.peek() == Some(&'/') => {
                out.push(' ');
                for c in chars.by_ref() {
                    if c == '\n' {
                        out.push('\n');
                        break;
                    }
                    out.push(' ');
                }
            }
            '"' | '\'' => {
                out.push(' ');
                let quote = ch;
                while let Some(c) = chars.next() {
                    out.push(if c == '\n' { '\n' } else { ' ' });
                    if c == '\\' {
                        if let Some(escaped) = chars.next() {
                            out.push(if escaped == '\n' { '\n' } else { ' ' });
                        }
                        continue;
                    }
                    if c == quote {
                        break;
                    }
                }
            }
            other => out.push(other),
        }
    }
    out
}

fn is_ident_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// Byte offsets of `word` where it stands as a whole identifier.
fn word_offsets(text: &str, word: &str) -> Vec<usize> {
    let mut found = Vec::new();
    let mut from = 0;
    while let Some(at) = text[from..].find(word) {
        let at = from + at;
        from = at + word.len();
        let before_ok = !text[..at].chars().next_back().is_some_and(is_ident_char);
        let after_ok = !text[at + word.len()..]
            .chars()
            .next()
            .is_some_and(is_ident_char);
        if before_ok && after_ok {
            found.push(at);
        }
    }
    found
}

/// The character before `at`, ignoring whitespace.
fn preceding_char(text: &str, at: usize) -> Option<char> {
    text[..at].chars().rev().find(|c| !c.is_whitespace())
}

/// Offsets of `word` used as a **keyword** — the whole identifier, and not
/// reached through a dot. The dot rule is load-bearing rather than
/// defensive: every task program declares `uses {Task.spawn}`, so an audit
/// that counted bare word matches would report a spawn in a file that has
/// only the effect name.
fn keyword_offsets(text: &str, word: &str) -> Vec<usize> {
    word_offsets(text, word)
        .into_iter()
        .filter(|at| preceding_char(text, *at) != Some('.'))
        .collect()
}

/// `.await` postfixes: the word `await` whose nearest preceding non-space
/// character *is* a dot. Canonical form writes no space there, but a
/// model's draft may.
fn count_awaits(text: &str) -> usize {
    word_offsets(text, "await")
        .into_iter()
        .filter(|at| preceding_char(text, *at) == Some('.'))
        .count()
}

/// The callee path after each `spawn`, in source order.
fn spawned_callees(text: &str) -> Vec<String> {
    keyword_offsets(text, "spawn")
        .into_iter()
        .filter_map(|at| leading_path(text[at + "spawn".len()..].trim_start()))
        .collect()
}

/// A dotted path at the head of `text`: `f`, `mill.rules.keeps`. `None`
/// when the head is not an identifier at all.
fn leading_path(text: &str) -> Option<String> {
    let end = text
        .find(|c: char| !(is_ident_char(c) || c == '.'))
        .unwrap_or(text.len());
    let path = text[..end].trim_end_matches('.');
    if path.is_empty() || path.starts_with(|c: char| c.is_ascii_digit()) {
        None
    } else {
        Some(path.to_string())
    }
}

fn push_once(list: &mut Vec<String>, value: &str) {
    if !list.iter().any(|existing| existing == value) {
        list.push(value.to_string());
    }
}

enum Uses {
    Empty,
    NonEmpty,
}

/// The effect set declared by `fn name` in this text: an absent `uses`
/// clause and `uses {}` are both [`Uses::Empty`] — the language treats them
/// as the same set, and so must the audit. `None` when no such declaration
/// is in this file.
fn declared_uses(text: &str, name: &str) -> Option<Uses> {
    for at in word_offsets(text, "fn") {
        let rest = text[at + 2..].trim_start();
        if !rest.starts_with(name) {
            continue;
        }
        let after_name = &rest[name.len()..];
        if after_name.starts_with(is_ident_char) {
            continue;
        }
        let Some(after_params) = skip_parenthesised(after_name) else {
            continue;
        };
        return Some(uses_clause(after_params));
    }
    None
}

/// The text after the parameter list: the balanced `( … )` beginning at the
/// first paren. Type parameters (`<T>`) are stepped over on the way, and a
/// declaration whose body brace arrives before any paren is malformed —
/// that tail is returned as-is, and reads as an empty effect set.
fn skip_parenthesised(text: &str) -> Option<&str> {
    let open = text.find(['(', '{'])?;
    if text[open..].starts_with('{') {
        return Some(&text[open..]);
    }
    let mut depth = 0usize;
    for (offset, ch) in text[open..].char_indices() {
        match ch {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    return Some(&text[open + offset + 1..]);
                }
            }
            _ => {}
        }
    }
    None
}

/// Read the signature tail between the parameter list and the body. A
/// `uses` clause carries a brace of its own, so it counts only when the
/// keyword stands before the first brace in the tail — otherwise a return
/// type that merely ends in those four letters (`-> Houses {`) would read
/// the whole function body as an effect set.
fn uses_clause(tail: &str) -> Uses {
    let Some(brace) = tail.find('{') else {
        return Uses::Empty;
    };
    let Some(keyword) = word_offsets(tail, "uses").into_iter().next() else {
        return Uses::Empty;
    };
    if keyword > brace {
        return Uses::Empty;
    }
    let Some(close) = tail[brace..].find('}') else {
        return Uses::Empty;
    };
    if tail[brace + 1..brace + close].trim().is_empty() {
        Uses::Empty
    } else {
        Uses::NonEmpty
    }
}

/// Is `name` called as a method — a dot before it, a call paren after?
fn is_method_called(text: &str, name: &str) -> bool {
    word_offsets(text, name).into_iter().any(|at| {
        let dotted = text[..at]
            .chars()
            .rev()
            .find(|c| !c.is_whitespace())
            .is_some_and(|c| c == '.');
        let called = text[at + name.len()..].trim_start().starts_with('(');
        dotted && called
    })
}

// --------------------------------------------------------------------- tests

#[cfg(test)]
mod tests {
    use super::*;

    /// The program the whole RFC exists to catch: it prints the required
    /// bytes of a fan-out task with no task in it anywhere.
    const SEQUENTIAL_GREEN: &str = r#"fn sum_to(limit: Int) -> Int {
    var total = 0;
    var i = 1;
    while i <= limit {
        total = total + i;
        i = i + 1;
    }
    total
}

fn main(io: Io) -> Result<Unit, Error> uses {Io.write} {
    io.write(text: sum_to(limit: 40).to_text())?;
    return Ok(unit);
}
"#;

    const TASK_GREEN: &str = r#"fn plan(n: Int) -> Int {
    n * n
}

fn main(io: Io) -> Result<Unit, Error> uses {Io.write, Task.spawn} {
    let total = scope {
        let a = spawn plan(n: 4);
        let b = spawn plan(n: 3);
        a.await + b.await
    };
    io.write(text: total.to_text())?;
    return Ok(unit);
}
"#;

    #[test]
    fn a_sequential_program_that_prints_the_right_answer_is_a_bypass() {
        // The four-reviewer finding, as an assertion: right bytes, no task.
        let audit = UsageAudit::of(SEQUENTIAL_GREEN);
        assert!(!audit.uses_tasks());
        assert_eq!(audit.spawns, 0);
        assert_eq!(audit.awaits, 0);
        assert!(!audit.scope);
        assert_eq!(classify(SEQUENTIAL_GREEN, true), RunClass::BypassedGreen);
        assert_eq!(classify(SEQUENTIAL_GREEN, false), RunClass::BypassedRed);
    }

    #[test]
    fn a_task_program_that_prints_the_right_answer_is_the_measurement() {
        let audit = UsageAudit::of(TASK_GREEN);
        assert!(audit.scope);
        assert_eq!(audit.spawns, 2);
        assert_eq!(audit.awaits, 2);
        assert_eq!(audit.spawned_callees, ["plan", "plan"]);
        assert!(audit.impure_callees.is_empty());
        assert!(audit.unresolved_callees.is_empty());
        assert_eq!(classify(TASK_GREEN, true), RunClass::UsedGreen);
        assert_eq!(classify(TASK_GREEN, false), RunClass::UsedWrong);
    }

    #[test]
    fn the_four_classes_are_the_two_by_two_and_nothing_else() {
        // Exhaustive over the crossing, so no run can fall outside.
        for (source, green, class) in [
            (TASK_GREEN, true, RunClass::UsedGreen),
            (TASK_GREEN, false, RunClass::UsedWrong),
            (SEQUENTIAL_GREEN, true, RunClass::BypassedGreen),
            (SEQUENTIAL_GREEN, false, RunClass::BypassedRed),
        ] {
            assert_eq!(classify(source, green), class);
        }
        assert_eq!(RunClass::UsedGreen.label(), "used-green");
        assert_eq!(RunClass::UsedWrong.label(), "used-wrong");
        assert_eq!(RunClass::BypassedGreen.label(), "bypassed-green");
        assert_eq!(RunClass::BypassedRed.label(), "bypassed-red");
    }

    #[test]
    fn an_effectful_child_is_used_wrong_not_bypassed() {
        // The XN6002 shape: the model put the capability in the child. It
        // reached for the feature, so it is (B), never (A).
        let source = r#"fn emit(io: Io, n: Int) -> Result<Unit, Error> uses {Io.write} {
    io.write(text: n.to_text())
}

fn main(io: Io) -> Result<Unit, Error> uses {Io.write, Task.spawn} {
    scope {
        let j = spawn emit(io: io, n: 1);
        j.await?;
    }
    return Ok(unit);
}
"#;
        let audit = UsageAudit::of(source);
        assert_eq!(audit.spawned_callees, ["emit"]);
        assert_eq!(audit.impure_callees, ["emit"]);
        assert!(audit.uses_tasks());
        assert_eq!(classify(source, false), RunClass::UsedWrong);
    }

    #[test]
    fn an_empty_uses_set_reads_as_pure_exactly_like_an_absent_one() {
        let written = "fn plan(n: Int) -> Int uses {} {\n    n\n}\n\
                       fn main() {\n    scope { let a = spawn plan(n: 1); a.await; }\n}\n";
        let omitted = "fn plan(n: Int) -> Int {\n    n\n}\n\
                       fn main() {\n    scope { let a = spawn plan(n: 1); a.await; }\n}\n";
        assert!(UsageAudit::of(written).impure_callees.is_empty());
        assert!(UsageAudit::of(omitted).impure_callees.is_empty());
        assert!(UsageAudit::of(written).unresolved_callees.is_empty());
    }

    #[test]
    fn a_spawn_of_a_name_this_file_never_declares_is_unresolved_not_impure() {
        let source = "fn main() {\n    scope {\n        let a = spawn absent(n: 1);\n        a.await;\n    }\n}\n";
        let audit = UsageAudit::of(source);
        assert_eq!(audit.unresolved_callees, ["absent"]);
        assert!(audit.impure_callees.is_empty());
        assert!(audit.uses_tasks());
    }

    #[test]
    fn task_words_inside_comments_and_strings_do_not_count() {
        // Without masking, this program would audit as a two-spawn task —
        // the exact way a text audit lies if it is written carelessly.
        let source = r#"// spawn a child here later, inside a scope, then .await it
fn main(io: Io) -> Result<Unit, Error> uses {Io.write} {
    io.write(text: "spawn scope await")?;
    return Ok(unit);
}
"#;
        let audit = UsageAudit::of(source);
        assert_eq!(audit.spawns, 0);
        assert_eq!(audit.awaits, 0);
        assert!(!audit.scope);
        assert_eq!(classify(source, true), RunClass::BypassedGreen);
    }

    #[test]
    fn the_effect_name_task_dot_spawn_is_not_a_spawn() {
        // Every task program declares `uses {Task.spawn}`, so an audit that
        // counted bare word matches would score this bypass as a task.
        let source = r#"fn main(io: Io) -> Result<Unit, Error> uses {Io.write, Task.spawn} {
    io.write(text: "1")?;
    return Ok(unit);
}
"#;
        let audit = UsageAudit::of(source);
        assert_eq!(audit.spawns, 0);
        assert!(!audit.uses_tasks());
        assert_eq!(classify(source, true), RunClass::BypassedGreen);
    }

    #[test]
    fn a_return_type_ending_in_the_uses_letters_is_not_an_effect_set() {
        // `-> Houses {` must not make the function body read as a `uses`
        // clause; the child stays pure and the run stays classifiable.
        let source = "fn plan(n: Int) -> Houses {\n    build(n: n)\n}\n\
                      fn main() {\n    scope { let a = spawn plan(n: 1); a.await; }\n}\n";
        assert!(UsageAudit::of(source).impure_callees.is_empty());
    }

    #[test]
    fn identifiers_that_merely_contain_the_keywords_do_not_count() {
        let source = "fn respawn_all(scoped: Int) -> Int {\n    scoped + 1\n}\n";
        let audit = UsageAudit::of(source);
        assert_eq!(audit.spawns, 0);
        assert!(!audit.scope);
    }

    #[test]
    fn awaits_are_counted_against_spawns() {
        // A Join short of an await (XN6008) and a Join awaited twice
        // (XN6006) are both used-wrong, and the ledger must show which.
        let short = "fn plan(n: Int) -> Int { n }\nfn main() {\n    scope {\n        let a = spawn plan(n: 1);\n        let b = spawn plan(n: 2);\n        a.await;\n    }\n}\n";
        let audit = UsageAudit::of(short);
        assert_eq!(audit.spawns, 2);
        assert_eq!(audit.awaits, 1);

        let twice = "fn plan(n: Int) -> Int { n }\nfn main() {\n    scope {\n        let a = spawn plan(n: 1);\n        a.await + a.await;\n    }\n}\n";
        let audit = UsageAudit::of(twice);
        assert_eq!(audit.spawns, 1);
        assert_eq!(audit.awaits, 2);
    }

    #[test]
    fn a_field_named_await_is_not_an_await_and_a_bare_one_is_not_either() {
        // `.await` needs the dot; the bare word (in a language that has no
        // such statement) must not inflate the count.
        assert_eq!(UsageAudit::of("let x = await;").awaits, 0);
        assert_eq!(UsageAudit::of("let x = j . await;").awaits, 1);
    }

    #[test]
    fn list_combinators_are_recorded_for_the_closure_era_comparison() {
        let source = "fn main() {\n    let t = [1, 2].map(|x| x * 3).filter(|x| x > 2)\n        .fold(init: 0, f: |a, x| a + x);\n}\n";
        assert_eq!(
            UsageAudit::of(source).list_combinators,
            ["map", "filter", "fold"]
        );
        // A local `fn map` that is never called as a method is not a
        // combinator use, and a bare mention is not a call.
        assert!(
            UsageAudit::of("fn map(n: Int) -> Int { n }\n")
                .list_combinators
                .is_empty()
        );
        assert!(
            UsageAudit::of("let f = xs.map;\n")
                .list_combinators
                .is_empty()
        );
    }

    #[test]
    fn a_spawn_of_a_qualified_path_resolves_by_its_last_segment() {
        let source = "fn keeps(n: Int) -> Int { n }\n\
                      fn main() {\n    scope { let a = spawn mill.rules.keeps(n: 1); a.await; }\n}\n";
        let audit = UsageAudit::of(source);
        assert_eq!(audit.spawned_callees, ["mill.rules.keeps"]);
        assert!(audit.impure_callees.is_empty());
        assert!(audit.unresolved_callees.is_empty());
    }

    #[test]
    fn a_spawn_with_no_callee_at_all_still_counts_as_reaching_for_tasks() {
        // XN6004's shape — a computed callee. There is no path to record,
        // but the model plainly tried, so it is not a bypass.
        let source = "fn main() {\n    scope { let a = spawn (pick())(n: 1); a.await; }\n}\n";
        let audit = UsageAudit::of(source);
        assert_eq!(audit.spawns, 1);
        assert!(audit.spawned_callees.is_empty());
        assert!(audit.uses_tasks());
        assert_eq!(classify(source, false), RunClass::UsedWrong);
    }

    #[test]
    fn the_audit_is_a_pure_function_of_the_bytes() {
        assert_eq!(UsageAudit::of(TASK_GREEN), UsageAudit::of(TASK_GREEN));
        assert_ne!(UsageAudit::of(TASK_GREEN), UsageAudit::of(SEQUENTIAL_GREEN));
    }
}
