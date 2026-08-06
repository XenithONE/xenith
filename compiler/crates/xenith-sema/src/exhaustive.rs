//! `match` exhaustiveness: find a value no arm covers.
//!
//! The shape is Maranget's usefulness check asked a single question — after
//! every unguarded arm, would a wildcard row still be useful? If it would,
//! the witness assembled on the way back up is a value the `match` misses,
//! rendered in source syntax: "`Rank.Gold` is not covered" is actionable,
//! "not exhaustive" is not.
//!
//! Guarded arms contribute nothing to coverage: a guard can be false at
//! runtime, so the value must land somewhere else regardless (design/0008
//! §1). `Int`, `Float`, `String` and `Char` cannot be enumerated by
//! literals, so their columns are covered only by a wildcard or binding —
//! their witness renders as `_`.

use std::sync::LazyLock;

use xenith_diag::Span;
use xenith_syntax::ast;

use crate::def::{DefKind, DefTable};
use crate::ty::Type;

/// Stands in for the payload columns a wildcard-like pattern covers.
static WILDCARD: LazyLock<ast::Pattern> = LazyLock::new(|| ast::Pattern {
    kind: ast::PatternKind::Wildcard,
    span: Span::EMPTY,
});

/// A value the arms do not cover, rendered in source syntax. `None` means
/// the `match` is exhaustive — or the scrutinee type is unknown, which stays
/// silent like every other operation on poison.
pub(crate) fn missing_witness(
    defs: &DefTable,
    scrutinee: &Type,
    arms: &[ast::MatchArm],
) -> Option<String> {
    if scrutinee.is_unknown() {
        return None;
    }
    let rows: Vec<Vec<&ast::Pattern>> = arms
        .iter()
        .filter(|arm| arm.guard.is_none())
        .map(|arm| vec![&arm.pattern])
        .collect();
    witness(defs, std::slice::from_ref(scrutinee), rows).map(|mut parts| parts.remove(0))
}

/// A witness vector for `types` that no row matches, or `None` when the rows
/// cover the whole space. One recursion step consumes the first column.
fn witness<'p>(
    defs: &DefTable,
    types: &[Type],
    rows: Vec<Vec<&'p ast::Pattern>>,
) -> Option<Vec<String>> {
    let Some((head, rest)) = types.split_first() else {
        // No columns left: any surviving row matches everything.
        return if rows.is_empty() {
            Some(Vec::new())
        } else {
            None
        };
    };

    // OR alternatives become rows of their own before anything looks at the
    // first column.
    let mut expanded: Vec<Vec<&'p ast::Pattern>> = Vec::new();
    for row in rows {
        expand_first(row, &mut expanded);
    }

    // The walk is bounded by the patterns, not the types — otherwise a
    // recursive shape behind `Option` would send it down forever. No rows:
    // the first value of every column is a witness. Rows that all ignore
    // the column: no constructor distinguishes them, so the column is `_`.
    if expanded.is_empty() {
        let mut found = vec![sample(defs, head, &mut Vec::new())];
        found.extend(rest.iter().map(|ty| sample(defs, ty, &mut Vec::new())));
        return Some(found);
    }
    if expanded
        .iter()
        .all(|row| is_wildcard_like(row[0], head, defs))
    {
        let specialized = expanded.iter().map(|row| row[1..].to_vec()).collect();
        let mut found = witness(defs, rest, specialized)?;
        found.insert(0, "_".to_string());
        return Some(found);
    }

    match head {
        // Poison never witnesses a gap: treat the column as covered by
        // every row, and blame nothing here.
        Type::Error | Type::Hole(_) => {
            let specialized = expanded.iter().map(|row| row[1..].to_vec()).collect();
            let mut found = witness(defs, rest, specialized)?;
            found.insert(0, "_".to_string());
            Some(found)
        }

        Type::Bool => {
            for value in [true, false] {
                let mut specialized = Vec::new();
                for row in &expanded {
                    let covers = match &row[0].kind {
                        ast::PatternKind::Literal(expr) => {
                            matches!(expr.kind, ast::ExprKind::Bool(b) if b == value)
                        }
                        _ => is_wildcard_like(row[0], head, defs),
                    };
                    if covers {
                        specialized.push(row[1..].to_vec());
                    }
                }
                if let Some(mut found) = witness(defs, rest, specialized) {
                    found.insert(0, value.to_string());
                    return Some(found);
                }
            }
            None
        }

        Type::Unit => {
            // One value; the `unit` literal and wildcard-likes all cover it.
            let mut specialized = Vec::new();
            for row in &expanded {
                let covers = matches!(
                    &row[0].kind,
                    ast::PatternKind::Literal(expr) if matches!(expr.kind, ast::ExprKind::Unit)
                ) || is_wildcard_like(row[0], head, defs);
                if covers {
                    specialized.push(row[1..].to_vec());
                }
            }
            let mut found = witness(defs, rest, specialized)?;
            found.insert(0, "unit".to_string());
            Some(found)
        }

        Type::Named { def, args } => {
            let info = defs.def(*def);
            let bindings: Vec<(String, Type)> = info
                .generics
                .iter()
                .cloned()
                .zip(args.iter().cloned())
                .collect();
            match &info.kind {
                DefKind::Enum { variants } => {
                    for variant in variants {
                        let payload: Vec<Type> = variant
                            .payload
                            .iter()
                            .map(|p| p.substitute(&bindings))
                            .collect();
                        let arity = payload.len();
                        let mut specialized = Vec::new();
                        for row in &expanded {
                            if let Some(front) =
                                specialize_variant(row[0], &variant.name, arity, head, defs)
                            {
                                let mut new_row = front;
                                new_row.extend_from_slice(&row[1..]);
                                specialized.push(new_row);
                            }
                        }
                        let mut sub_types = payload.clone();
                        sub_types.extend_from_slice(rest);
                        if let Some(found) = witness(defs, &sub_types, specialized) {
                            let shown = if *def == defs.option || *def == defs.result {
                                variant.name.clone()
                            } else {
                                format!("{}.{}", info.name, variant.name)
                            };
                            let rendered = if arity == 0 {
                                shown
                            } else {
                                format!("{shown}({})", found[..arity].join(", "))
                            };
                            let mut out = vec![rendered];
                            out.extend_from_slice(&found[arity..]);
                            return Some(out);
                        }
                    }
                    None
                }
                DefKind::Struct { fields } => {
                    // A struct is an enum with one constructor; struct
                    // patterns may list any subset of fields, unlisted ones
                    // being implicit wildcards.
                    let field_types: Vec<Type> =
                        fields.iter().map(|f| f.ty.substitute(&bindings)).collect();
                    let arity = field_types.len();
                    let mut specialized = Vec::new();
                    for row in &expanded {
                        let front: Option<Vec<&ast::Pattern>> = match &row[0].kind {
                            ast::PatternKind::Struct { fields: listed, .. } => Some(
                                fields
                                    .iter()
                                    .map(|declared| {
                                        listed
                                            .iter()
                                            .find(|f| f.name.name == declared.name)
                                            .and_then(|f| f.pattern.as_ref())
                                            .unwrap_or(&WILDCARD)
                                    })
                                    .collect(),
                            ),
                            _ if is_wildcard_like(row[0], head, defs) => {
                                Some(vec![&*WILDCARD; arity])
                            }
                            _ => None,
                        };
                        if let Some(front) = front {
                            let mut new_row = front;
                            new_row.extend_from_slice(&row[1..]);
                            specialized.push(new_row);
                        }
                    }
                    let mut sub_types = field_types;
                    sub_types.extend_from_slice(rest);
                    let found = witness(defs, &sub_types, specialized)?;
                    // Fields whose witness is `_` say nothing; leave them out,
                    // which is legal pattern syntax here.
                    let interesting: Vec<String> = fields
                        .iter()
                        .zip(&found[..arity])
                        .filter(|(_, sub)| *sub != "_")
                        .map(|(field, sub)| format!("{}: {sub}", field.name))
                        .collect();
                    let rendered = if interesting.is_empty() {
                        format!("{} {{ }}", info.name)
                    } else {
                        format!("{} {{ {} }}", info.name, interesting.join(", "))
                    };
                    let mut out = vec![rendered];
                    out.extend_from_slice(&found[arity..]);
                    Some(out)
                }
                // Opaque containers cannot be enumerated by patterns.
                DefKind::Opaque => non_enumerable(defs, head, rest, &expanded),
            }
        }

        // Int, Float, String, Char, type parameters, function values: no
        // literal set enumerates them, so only `_` or a binding covers.
        _ => non_enumerable(defs, head, rest, &expanded),
    }
}

/// The first value of a type, rendered in source syntax — what an empty
/// row set is missing. Recursive shapes are cut at `_` by the visited set,
/// which is what keeps this finite where the pattern space is not.
fn sample(defs: &DefTable, ty: &Type, visited: &mut Vec<crate::ty::DefId>) -> String {
    match ty {
        Type::Bool => "true".to_string(),
        Type::Unit => "unit".to_string(),
        Type::Named { def, args } if !visited.contains(def) => {
            let info = defs.def(*def);
            let bindings: Vec<(String, Type)> = info
                .generics
                .iter()
                .cloned()
                .zip(args.iter().cloned())
                .collect();
            match &defs.def(*def).kind {
                DefKind::Enum { variants } => {
                    let Some(variant) = variants.first() else {
                        return "_".to_string();
                    };
                    let shown = if *def == defs.option || *def == defs.result {
                        variant.name.clone()
                    } else {
                        format!("{}.{}", info.name, variant.name)
                    };
                    if variant.payload.is_empty() {
                        shown
                    } else {
                        visited.push(*def);
                        let parts: Vec<String> = variant
                            .payload
                            .iter()
                            .map(|p| sample(defs, &p.substitute(&bindings), visited))
                            .collect();
                        visited.pop();
                        format!("{shown}({})", parts.join(", "))
                    }
                }
                DefKind::Struct { fields } => {
                    visited.push(*def);
                    let interesting: Vec<String> = fields
                        .iter()
                        .map(|field| {
                            (
                                field.name.clone(),
                                sample(defs, &field.ty.substitute(&bindings), visited),
                            )
                        })
                        .filter(|(_, value)| value != "_")
                        .map(|(name, value)| format!("{name}: {value}"))
                        .collect();
                    visited.pop();
                    if interesting.is_empty() {
                        format!("{} {{ }}", info.name)
                    } else {
                        format!("{} {{ {} }}", info.name, interesting.join(", "))
                    }
                }
                DefKind::Opaque => "_".to_string(),
            }
        }
        _ => "_".to_string(),
    }
}

/// Split an OR pattern in the first column into one row per alternative.
fn expand_first<'p>(row: Vec<&'p ast::Pattern>, out: &mut Vec<Vec<&'p ast::Pattern>>) {
    match &row[0].kind {
        ast::PatternKind::Or(alternatives) => {
            for alternative in alternatives {
                let mut split = row.clone();
                split[0] = alternative;
                expand_first(split, out);
            }
        }
        _ => out.push(row),
    }
}

/// The default matrix: only wildcard-like rows survive, and the witness for
/// this column renders as `_` — "a value distinct from every literal listed".
fn non_enumerable(
    defs: &DefTable,
    head: &Type,
    rest: &[Type],
    expanded: &[Vec<&ast::Pattern>],
) -> Option<Vec<String>> {
    let mut specialized = Vec::new();
    for row in expanded {
        if is_wildcard_like(row[0], head, defs) {
            specialized.push(row[1..].to_vec());
        }
    }
    let mut found = witness(defs, rest, specialized)?;
    found.insert(0, "_".to_string());
    Some(found)
}

/// Whether `pattern` covers everything at a column of type `ty`. A binding
/// usually does — unless it names a variant of the column's enum, where the
/// checker (and the runtime) treat it as a variant pattern instead.
fn is_wildcard_like(pattern: &ast::Pattern, ty: &Type, defs: &DefTable) -> bool {
    match &pattern.kind {
        ast::PatternKind::Wildcard | ast::PatternKind::Error => true,
        ast::PatternKind::Binding(ident) => match ty {
            Type::Named { def, .. } => defs.variant_named(*def, &ident.name).is_none(),
            _ => true,
        },
        _ => false,
    }
}

/// The payload columns `pattern` contributes when specialized to the variant
/// `name`/`arity`, or `None` when the pattern cannot match that variant.
fn specialize_variant<'p>(
    pattern: &'p ast::Pattern,
    name: &str,
    arity: usize,
    ty: &Type,
    defs: &DefTable,
) -> Option<Vec<&'p ast::Pattern>> {
    match &pattern.kind {
        _ if is_wildcard_like(pattern, ty, defs) => Some(vec![&*WILDCARD; arity]),
        // A binding that names a variant is that variant, matched bare. When
        // the variant carries a payload the checker has already reported it;
        // counting it as covering avoids a second complaint about one arm.
        ast::PatternKind::Binding(ident) if ident.name == name => Some(vec![&*WILDCARD; arity]),
        // `Rank.Gold` — matched by variant identity, payload unexamined.
        ast::PatternKind::Path(path) if path.segments.last().is_some_and(|s| s.name == name) => {
            Some(vec![&*WILDCARD; arity])
        }
        ast::PatternKind::Variant { path, elements }
            if path.segments.last().is_some_and(|s| s.name == name) =>
        {
            // An arity mismatch is already reported; pad or truncate rather
            // than compound it.
            let mut front: Vec<&ast::Pattern> = elements.iter().take(arity).collect();
            while front.len() < arity {
                front.push(&WILDCARD);
            }
            Some(front)
        }
        _ => None,
    }
}
