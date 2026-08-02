//! Candidate expressions for a hole: enumerate, filter, rank, diversify.
//!
//! This is enumeration, not synthesis (design/0006 §4). Depth is one — a
//! binding, a constructor, one call — with nested holes standing in for the
//! arguments we cannot fill. That shape is deliberate: reviewers converged on
//! the point that a model is better served by a *partially correct skeleton*
//! with explicit gaps (`try_send(request: ??)`) than by the fifth fully-formed
//! but irrelevant expression. An IDE ranks completions; this ranks scaffolds.
//!
//! Functions whose return type fits but whose effects exceed the budget are
//! reported in `blocked` rather than silently dropped — a model that is not
//! told *why* something is unusable repeats the mistake.

use crate::def::{DefKind, DefTable, GenericInfo};
use crate::ty::{EffectSet, Type, TypeName};

#[derive(Clone, Debug)]
pub struct Candidate {
    /// Legal Xenith source for this position, holes included.
    pub expression: String,
    /// No nested holes: this expression could be accepted as-is.
    pub complete: bool,
    /// Effects the candidate would use (always within the goal's budget).
    pub requires_effects: Vec<String>,
}

struct Scored {
    candidate: Candidate,
    score: i32,
    /// Leading symbol, for diversity re-ranking.
    head: String,
}

/// Rank candidates for a hole expecting `expected`.
///
/// `scope` carries real types (innermost shadowing already resolved);
/// `generics` are the enclosing function's parameters, so property bounds
/// resolve for `T`-typed bindings; `enclosing` is excluded from suggestions —
/// a checker that answers "what goes in this unfinished body?" with "call the
/// function you are writing" has answered nothing.
pub fn candidates_for(
    defs: &DefTable,
    expected: &Type,
    scope: &[(String, Type, bool)],
    budget: &EffectSet,
    generics: &[GenericInfo],
    enclosing: &str,
    hole_name: Option<&str>,
) -> (Vec<Candidate>, Vec<String>) {
    // A hole expecting poison has no meaningful candidates.
    if expected.is_unknown() {
        return (Vec::new(), Vec::new());
    }

    let mut pool: Vec<Scored> = Vec::new();
    let mut blocked: Vec<String> = Vec::new();

    // ----- 1. bindings with exactly the required type -----
    for (name, ty, _) in scope {
        if ty == expected {
            pool.push(Scored {
                score: 100 + 40 + 15 + name_affinity(hole_name, name),
                head: name.clone(),
                candidate: Candidate {
                    expression: name.clone(),
                    complete: true,
                    requires_effects: Vec::new(),
                },
            });
        }
    }

    // ----- 2. field projections one step deep -----
    for (name, ty, _) in scope {
        let Type::Named { def, args } = ty else {
            continue;
        };
        let DefKind::Struct { fields } = &defs.def(*def).kind else {
            continue;
        };
        let bindings: Vec<(String, Type)> = defs
            .def(*def)
            .generics
            .iter()
            .cloned()
            .zip(args.iter().cloned())
            .collect();
        for field in fields {
            if field.ty.substitute(&bindings) == *expected {
                let expression = format!("{name}.{}", field.name);
                pool.push(Scored {
                    score: 100 + 40 + 15 - 6 + name_affinity(hole_name, &field.name),
                    head: name.clone(),
                    candidate: Candidate {
                        expression,
                        complete: true,
                        requires_effects: Vec::new(),
                    },
                });
            }
        }
    }

    // ----- 3. constructors of the expected type -----
    if let Type::Named { def, args } = expected {
        let info = defs.def(*def);
        let type_bindings: Vec<(String, Type)> = info
            .generics
            .iter()
            .cloned()
            .zip(args.iter().cloned())
            .collect();

        match &info.kind {
            DefKind::Enum { variants } => {
                let unqualified = *def == defs.option || *def == defs.result;
                for variant in variants {
                    let shown = if unqualified {
                        variant.name.clone()
                    } else {
                        format!("{}.{}", info.name, variant.name)
                    };
                    let (expression, nested, filled) =
                        apply_payload(&shown, &variant.payload, &type_bindings, scope);
                    pool.push(Scored {
                        score: 100 + 15 + if nested == 0 { 40 } else { -25 * nested } + filled * 10
                            - 6 * (1 + variant.payload.len() as i32),
                        head: shown,
                        candidate: Candidate {
                            expression,
                            complete: nested == 0,
                            requires_effects: Vec::new(),
                        },
                    });
                }
            }
            DefKind::Struct { fields } => {
                // A literal skeleton with every field named, scope values
                // filled where a type matches exactly.
                let mut nested = 0;
                let mut filled = 0;
                let parts: Vec<String> = fields
                    .iter()
                    .map(|field| {
                        let concrete = field.ty.substitute(&type_bindings);
                        match scope.iter().find(|(_, ty, _)| *ty == concrete) {
                            Some((name, _, _)) => {
                                filled += 1;
                                format!("{}: {name}", field.name)
                            }
                            None => {
                                nested += 1;
                                format!("{}: ??", field.name)
                            }
                        }
                    })
                    .collect();
                let expression = format!("{} {{ {} }}", info.name, parts.join(", "));
                pool.push(Scored {
                    score: 100 + 15 + if nested == 0 { 40 } else { -25 * nested } + filled * 10
                        - 6 * (1 + fields.len() as i32),
                    head: info.name.clone(),
                    candidate: Candidate {
                        expression,
                        complete: nested == 0,
                        requires_effects: Vec::new(),
                    },
                });
            }
            DefKind::Opaque => {}
        }
    }

    // ----- 4. module functions whose return type fits -----
    for sig in &defs.fns {
        if sig.name == enclosing {
            // "Call the function you are writing" answers nothing; genuine
            // recursion is a decision, not a completion.
            continue;
        }
        let ret = if sig.is_async {
            Type::Named {
                def: defs.task,
                args: vec![sig.ret.clone()],
            }
        } else {
            sig.ret.clone()
        };
        let mut bindings: Vec<(String, Type)> = Vec::new();
        if !return_matches(&ret, expected, &mut bindings) {
            continue;
        }

        if !sig.effects.is_subset_of(budget) {
            let missing: Vec<&str> = sig.effects.missing_from(budget);
            blocked.push(format!(
                "{} — needs {{{}}}, not permitted here",
                sig.name,
                missing.join(", ")
            ));
            continue;
        }

        let mut nested = 0;
        let mut filled = 0;
        let rendered_args: Vec<String> = sig
            .params
            .iter()
            .map(|(param, ty)| {
                let concrete = ty.substitute(&bindings);
                match scope.iter().find(|(_, t, _)| *t == concrete) {
                    Some((name, _, _)) => {
                        filled += 1;
                        format!("{param}: {name}")
                    }
                    None => {
                        nested += 1;
                        format!("{param}: ??")
                    }
                }
            })
            .collect();
        let expression = format!("{}({})", sig.name, rendered_args.join(", "));

        let convention = convention_bonus(&sig.name, expected, defs);
        let effect_bonus = if sig.effects.is_empty() { 15 } else { 0 };
        pool.push(Scored {
            score: 100
                + if nested == 0 { 40 } else { -25 * nested }
                + filled * 10
                + convention
                + effect_bonus
                + name_affinity(hole_name, &sig.name)
                - 6 * (1 + sig.params.len() as i32),
            head: sig.name.clone(),
            candidate: Candidate {
                expression,
                complete: nested == 0,
                requires_effects: sig.effects.iter().map(String::from).collect(),
            },
        });
    }

    // ----- 5. prelude methods on in-scope bindings -----
    for (name, ty, mutable) in scope {
        for method in defs.methods_of(ty) {
            // The binding's own type arguments fix the receiver generics
            // (`xs: List<Int>` fixes T = Int); the method's extra generics
            // bind from the expected type.
            let mut bindings: Vec<(String, Type)> = Vec::new();
            if let Type::Named { def, args } = ty {
                for (generic, arg) in defs.def(*def).generics.iter().zip(args.iter()) {
                    bindings.push((generic.clone(), arg.clone()));
                }
            }
            if !return_matches(&method.ret.substitute(&bindings), expected, &mut bindings) {
                continue;
            }

            if !method.effects.is_subset_of(budget) {
                let missing: Vec<&str> = method.effects.missing_from(budget);
                blocked.push(format!(
                    "{name}.{} — needs {{{}}}, not permitted here",
                    method.name,
                    missing.join(", ")
                ));
                continue;
            }

            // A bound the receiver's element type cannot satisfy makes the
            // method unusable however well its return type fits.
            let violated = method.bounds.iter().find_map(|(generic, property)| {
                let (_, concrete) = bindings.iter().find(|(n, _)| n == generic)?;
                if defs.has_property(concrete, *property, generics) {
                    None
                } else {
                    Some((*generic, *property, concrete.clone()))
                }
            });
            if let Some((generic, property, concrete)) = violated {
                let name_of = |id| defs.name_of(id);
                let rendered = TypeName {
                    ty: &concrete,
                    name_of: &name_of,
                }
                .to_string();
                blocked.push(format!(
                    "{name}.{} — requires `{generic}: {}`, but `{rendered}` does not satisfy it",
                    method.name,
                    property.name()
                ));
                continue;
            }

            // A mutator on a `let` binding would be rejected by the checker;
            // offering it anyway teaches the model a wrong move.
            if method.mutates_receiver && !mutable {
                blocked.push(format!(
                    "{name}.{} — mutates its receiver, and `{name}` is not a `var` binding",
                    method.name
                ));
                continue;
            }

            let mut nested = 0;
            let mut filled = 0;
            let rendered_args: Vec<String> = method
                .params
                .iter()
                .map(|(param, ty)| {
                    let concrete = ty.substitute(&bindings);
                    match scope.iter().find(|(_, t, _)| *t == concrete) {
                        Some((found, _, _)) => {
                            filled += 1;
                            format!("{param}: {found}")
                        }
                        None => {
                            nested += 1;
                            format!("{param}: ??")
                        }
                    }
                })
                .collect();
            let expression = format!("{name}.{}({})", method.name, rendered_args.join(", "));

            let convention = convention_bonus(method.name, expected, defs);
            let effect_bonus = if method.effects.is_empty() { 15 } else { 0 };
            pool.push(Scored {
                score: 100
                    + if nested == 0 { 40 } else { -25 * nested }
                    + filled * 10
                    + convention
                    + effect_bonus
                    + name_affinity(hole_name, method.name)
                    - 6 * (1 + method.params.len() as i32),
                head: name.clone(),
                candidate: Candidate {
                    expression,
                    complete: nested == 0,
                    requires_effects: method.effects.iter().map(String::from).collect(),
                },
            });
        }
    }

    // ----- rank, then diversify -----
    pool.sort_by(|a, b| b.score.cmp(&a.score).then(a.head.cmp(&b.head)));

    // Round-robin across distinct heads so five answers are not five variants
    // of one idea (design/0006 §4: diversity re-rank).
    let mut by_head: Vec<(String, Vec<Candidate>)> = Vec::new();
    for scored in pool {
        match by_head.iter_mut().find(|(head, _)| *head == scored.head) {
            Some((_, list)) => list.push(scored.candidate),
            None => by_head.push((scored.head, vec![scored.candidate])),
        }
    }
    let mut picked = Vec::new();
    let mut round = 0;
    while picked.len() < 5 {
        let mut any = false;
        for (_, list) in &by_head {
            if let Some(candidate) = list.get(round) {
                picked.push(candidate.clone());
                any = true;
                if picked.len() == 5 {
                    break;
                }
            }
        }
        if !any {
            break;
        }
        round += 1;
    }

    blocked.sort();
    (picked, blocked)
}

/// `Variant(??, x)` — payload slots filled from scope where the type matches
/// exactly, nested holes elsewhere. Returns (expression, nested, filled).
fn apply_payload(
    shown: &str,
    payload: &[Type],
    type_bindings: &[(String, Type)],
    scope: &[(String, Type, bool)],
) -> (String, i32, i32) {
    if payload.is_empty() {
        return (shown.to_string(), 0, 0);
    }
    let mut nested = 0;
    let mut filled = 0;
    let parts: Vec<String> = payload
        .iter()
        .map(|ty| {
            let concrete = ty.substitute(type_bindings);
            match scope.iter().find(|(_, t, _)| *t == concrete) {
                Some((name, _, _)) => {
                    filled += 1;
                    name.clone()
                }
                None => {
                    nested += 1;
                    "??".to_string()
                }
            }
        })
        .collect();
    (format!("{shown}({})", parts.join(", ")), nested, filled)
}

/// One-way structural match binding `Type::Param`s in `ret` from `expected`.
/// Shared with the `producers` query, which asks the same question.
pub(crate) fn return_matches(
    ret: &Type,
    expected: &Type,
    bindings: &mut Vec<(String, Type)>,
) -> bool {
    match (ret, expected) {
        (Type::Param(name), _) => {
            if let Some((_, bound)) = bindings.iter().find(|(n, _)| n == name) {
                bound == expected
            } else {
                bindings.push((name.clone(), expected.clone()));
                true
            }
        }
        (Type::Named { def: a, args: xs }, Type::Named { def: b, args: ys }) => {
            a == b
                && xs.len() == ys.len()
                && xs
                    .iter()
                    .zip(ys)
                    .all(|(x, y)| return_matches(x, y, bindings))
        }
        (a, b) => a == b,
    }
}

/// The naming rules are guessable by design; reward candidates whose spelling
/// agrees with the shape they produce (design/0006 §4).
fn convention_bonus(name: &str, expected: &Type, defs: &DefTable) -> i32 {
    let head = match expected {
        Type::Named { def, .. } => Some(*def),
        Type::Bool => {
            return if name.starts_with("is_") || name.starts_with("has_") {
                20
            } else {
                0
            };
        }
        _ => None,
    };
    match head {
        Some(def) if def == defs.result && name.starts_with("try_") => 20,
        Some(def)
            if def == defs.option && (name.starts_with("get") || name.starts_with("checked_")) =>
        {
            20
        }
        _ => 0,
    }
}

/// Overlap between the hole's own name and a candidate symbol: `??config`
/// should surface `config` and `load_config` before unrelated matches.
fn name_affinity(hole: Option<&str>, symbol: &str) -> i32 {
    let Some(hole) = hole else { return 0 };
    if hole.is_empty() {
        return 0;
    }
    let hole = hole.to_ascii_lowercase();
    let symbol = symbol.to_ascii_lowercase();
    if symbol == hole {
        25
    } else if symbol.contains(&hole) || hole.contains(&symbol) {
        15
    } else {
        0
    }
}
