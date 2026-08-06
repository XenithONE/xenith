//! The value-size rule: a struct or enum must not contain itself by value.
//!
//! Values are values, so a type that reaches itself without crossing an
//! indirect container has no finite size. The prelude containers — Option,
//! List, Map and the rest — hold their contents indirectly and are the
//! walls the walk stops at (design/0010 §5). Generic user types are handled
//! precisely: `B<A>` only carries `A` by value if `B` actually holds its
//! parameter by value, computed as a fixpoint so wrappers built on `Option`
//! stay finite.

use std::collections::{HashMap, HashSet};

use crate::def::{DefKind, DefTable};
use crate::ty::{DefId, Type};

/// Every by-value cycle among user types, each as a name list in cycle
/// order rotated to start at its lexicographically smallest member, the
/// whole set sorted — deterministic however the graph was walked.
pub(crate) fn value_cycles(table: &DefTable) -> Vec<Vec<String>> {
    let users: Vec<DefId> = table
        .def_ids()
        .filter(|def| !table.is_prelude_def(*def))
        .collect();

    // Which of each user type's parameters it holds by value, to fixpoint:
    // the flag only ever turns on, so this terminates.
    let mut by_value: HashMap<DefId, Vec<bool>> = users
        .iter()
        .map(|def| (*def, vec![false; table.def(*def).generics.len()]))
        .collect();
    loop {
        let mut changed = false;
        for &def in &users {
            let generics = table.def(def).generics.clone();
            let mut held: Vec<usize> = Vec::new();
            for ty in surface(table, def) {
                walk(table, &by_value, ty, &mut |param| {
                    if let Some(index) = generics.iter().position(|g| g == param) {
                        held.push(index);
                    }
                });
            }
            let flags = by_value.get_mut(&def).expect("seeded above");
            for index in held {
                if !flags[index] {
                    flags[index] = true;
                    changed = true;
                }
            }
        }
        if !changed {
            break;
        }
    }

    // The by-value edge set, then a plain DFS for its cycles.
    let mut edges: HashMap<DefId, Vec<DefId>> = HashMap::new();
    for &def in &users {
        let mut reached: Vec<DefId> = Vec::new();
        for ty in surface(table, def) {
            collect_defs(table, &by_value, ty, &mut reached);
        }
        reached.sort_by_key(|d| table.name_of(*d));
        reached.dedup();
        edges.insert(def, reached);
    }

    let mut state: HashMap<DefId, u8> = HashMap::new();
    let mut stack: Vec<DefId> = Vec::new();
    let mut cycles: HashSet<Vec<String>> = HashSet::new();
    let mut roots = users.clone();
    roots.sort_by_key(|d| table.name_of(*d));
    for root in roots {
        dfs(table, &edges, root, &mut state, &mut stack, &mut cycles);
    }

    let mut out: Vec<Vec<String>> = cycles.into_iter().collect();
    out.sort();
    out
}

/// The types a definition holds directly: field types, variant payloads.
fn surface(table: &DefTable, def: DefId) -> Vec<&Type> {
    match &table.def(def).kind {
        DefKind::Struct { fields } => fields.iter().map(|f| &f.ty).collect(),
        DefKind::Enum { variants } => variants.iter().flat_map(|v| v.payload.iter()).collect(),
        DefKind::Opaque => Vec::new(),
    }
}

/// Walk a type's by-value reach, calling `on_param` for every parameter
/// held directly. Prelude containers end the walk; a user type's arguments
/// are entered only where that type holds the parameter by value.
fn walk(
    table: &DefTable,
    by_value: &HashMap<DefId, Vec<bool>>,
    ty: &Type,
    on_param: &mut impl FnMut(&str),
) {
    match ty {
        Type::Param(name) => on_param(name),
        Type::Named { def, args } => {
            if table.is_prelude_def(*def) {
                return;
            }
            let flags = by_value.get(def);
            for (index, arg) in args.iter().enumerate() {
                let held = flags.and_then(|f| f.get(index)).copied().unwrap_or(true);
                if held {
                    walk(table, by_value, arg, on_param);
                }
            }
        }
        _ => {}
    }
}

/// The user definitions a type reaches by value — the edge targets.
fn collect_defs(
    table: &DefTable,
    by_value: &HashMap<DefId, Vec<bool>>,
    ty: &Type,
    out: &mut Vec<DefId>,
) {
    if let Type::Named { def, args } = ty {
        if table.is_prelude_def(*def) {
            return;
        }
        out.push(*def);
        let flags = by_value.get(def);
        for (index, arg) in args.iter().enumerate() {
            let held = flags.and_then(|f| f.get(index)).copied().unwrap_or(true);
            if held {
                collect_defs(table, by_value, arg, out);
            }
        }
    }
}

fn dfs(
    table: &DefTable,
    edges: &HashMap<DefId, Vec<DefId>>,
    node: DefId,
    state: &mut HashMap<DefId, u8>,
    stack: &mut Vec<DefId>,
    cycles: &mut HashSet<Vec<String>>,
) {
    match state.get(&node) {
        Some(2) => return,
        Some(1) => return, // handled by the caller's on-stack check
        _ => {}
    }
    state.insert(node, 1);
    stack.push(node);
    for &next in edges.get(&node).map(Vec::as_slice).unwrap_or(&[]) {
        if state.get(&next) == Some(&1) {
            if let Some(position) = stack.iter().position(|d| *d == next) {
                let names: Vec<String> = stack[position..]
                    .iter()
                    .map(|d| table.name_of(*d))
                    .collect();
                cycles.insert(rotate_min_first(names));
            }
        } else {
            dfs(table, edges, next, state, stack, cycles);
        }
    }
    stack.pop();
    state.insert(node, 2);
}

/// The canonical spelling of one cycle, so the same loop found from two
/// entry points reports once.
fn rotate_min_first(names: Vec<String>) -> Vec<String> {
    let Some(start) = names
        .iter()
        .enumerate()
        .min_by(|a, b| a.1.cmp(b.1))
        .map(|(index, _)| index)
    else {
        return names;
    };
    let mut rotated = Vec::with_capacity(names.len());
    rotated.extend_from_slice(&names[start..]);
    rotated.extend_from_slice(&names[..start]);
    rotated
}
