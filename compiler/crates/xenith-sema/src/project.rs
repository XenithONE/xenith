//! Project-wide analysis: many files, one declaration table, two phases.
//!
//! Phase one indexes every module's declaration headers; phase two checks
//! bodies. Module order is the dictionary order of module paths — not
//! topological order, which is only deterministic once its tie-break is
//! spelled out anyway (design/0010 §5). Import cycles are legal: Xenith has
//! no module initialisers, so mutual reference across files carries no
//! execution-order question.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};

use xenith_diag::{DiagCode, Diagnostic, Span};
use xenith_syntax::ast;

use crate::check::{self, Goal, ModuleCtx, Probe, TeachBudget};
use crate::def::{self, CollectUnit, DefTable};

/// One parsed module handed to project analysis. The loader has already
/// validated the path ("main", "game.player") against the layout rules.
pub struct ModuleUnit<'a> {
    pub path: String,
    pub module: &'a ast::Module,
}

/// The result: diagnostics and hole goals per input unit, in the input's
/// order, plus the shared table the interpreter executes against.
pub struct ProjectAnalysis {
    pub diagnostics: Vec<Vec<Diagnostic>>,
    pub goals: Vec<Vec<Goal>>,
    pub table: DefTable,
}

pub fn analyze_project(units: &[ModuleUnit]) -> ProjectAnalysis {
    analyze_project_at(units, None).0
}

/// As [`analyze_project`], additionally capturing the checker's state at an
/// offset inside one unit: `probe_at` is `(unit index, byte offset)`. This is
/// `type_at` answered with the whole project's declarations in view, so a
/// cross-module type renders qualified instead of failing to resolve
/// (design/0013 §1).
pub fn analyze_project_at(
    units: &[ModuleUnit],
    probe_at: Option<(usize, u32)>,
) -> (ProjectAnalysis, Option<Probe>) {
    let mut out: Vec<Vec<Diagnostic>> = units.iter().map(|_| Vec::new()).collect();

    // Dictionary order of module paths decides every cross-module sequence.
    let mut order: Vec<usize> = (0..units.len()).collect();
    order.sort_by(|a, b| units[*a].path.cmp(&units[*b].path));

    // ----- use extraction and validation (design/0010 §3) -----
    let module_set: HashSet<&str> = units.iter().map(|u| u.path.as_str()).collect();
    let mut uses_per: Vec<Vec<(String, Span)>> = Vec::with_capacity(units.len());
    for (index, unit) in units.iter().enumerate() {
        let mut seen: Vec<(String, Span)> = Vec::new();
        for item in &unit.module.items {
            let ast::ItemKind::Use(use_item) = &item.kind else {
                continue;
            };
            let path = use_item
                .path
                .segments
                .iter()
                .map(|s| s.name.as_str())
                .collect::<Vec<_>>()
                .join(".");
            if seen.iter().any(|(p, _)| *p == path) {
                out[index].push(Diagnostic::error(
                    DiagCode::DuplicateUse,
                    item.span,
                    format!("`use {path};` appears more than once"),
                ));
                continue;
            }
            if !module_set.contains(path.as_str()) {
                out[index].push(Diagnostic::error(
                    DiagCode::UnknownModule,
                    use_item.path.span,
                    format!("`{path}` is not a module in this project"),
                ));
                continue;
            }
            seen.push((path, item.span));
        }
        // Canonical order — also where the use-fix inserts (0010 §3).
        seen.sort_by(|a, b| a.0.cmp(&b.0));
        uses_per.push(seen);
    }

    // ----- phase one: declaration headers, all modules -----
    let used_cells: Vec<RefCell<HashSet<String>>> =
        units.iter().map(|_| RefCell::new(HashSet::new())).collect();
    let use_paths: Vec<Vec<String>> = uses_per
        .iter()
        .map(|uses| uses.iter().map(|(p, _)| p.clone()).collect())
        .collect();
    let sorted: Vec<CollectUnit> = order
        .iter()
        .map(|&i| CollectUnit {
            prefix: &units[i].path,
            module: units[i].module,
            uses: &use_paths[i],
            used: Some(&used_cells[i]),
        })
        .collect();
    let (table, collected) = def::collect_units(&sorted);
    for (sorted_index, diagnostic) in collected {
        out[order[sorted_index]].push(diagnostic);
    }

    // Infinite-size cycles, attributed to the module that owns the first
    // member (design/0010 §5).
    for cycle in crate::recursion::value_cycles(&table) {
        let first = &cycle[0];
        let (owner, bare) = first.rsplit_once('.').unwrap_or(("", first.as_str()));
        let Some(unit_index) = units.iter().position(|u| u.path == owner) else {
            continue;
        };
        let span = units[unit_index]
            .module
            .items
            .iter()
            .find_map(|item| match &item.kind {
                ast::ItemKind::Struct(s) if s.name.name == bare => Some(s.name.span),
                ast::ItemKind::Enum(e) if e.name.name == bare => Some(e.name.span),
                _ => None,
            })
            .unwrap_or(Span::EMPTY);
        out[unit_index].push(check::infinite_size_diagnostic(&cycle, span));
    }

    // ----- the project's public surface, for the XN2002 use-fix -----
    let mut pub_index: HashMap<String, Vec<String>> = HashMap::new();
    for info in table.defs_iter() {
        if info.is_pub {
            if let Some((owner, bare)) = info.name.rsplit_once('.') {
                pub_index
                    .entry(bare.to_string())
                    .or_default()
                    .push(owner.to_string());
            }
        }
    }
    for sig in &table.fns {
        if sig.is_pub {
            if let Some((owner, bare)) = sig.name.rsplit_once('.') {
                pub_index
                    .entry(bare.to_string())
                    .or_default()
                    .push(owner.to_string());
            }
        }
    }
    for owners in pub_index.values_mut() {
        owners.sort();
        owners.dedup();
    }

    // ----- phase two: bodies, dictionary order, one teach budget per run -----
    let mut goals: Vec<Vec<Goal>> = units.iter().map(|_| Vec::new()).collect();
    let mut probe: Option<Probe> = None;
    let mut teach_budget = TeachBudget::new();
    for &i in &order {
        let ctx = ModuleCtx {
            prefix: units[i].path.clone(),
            uses: uses_per[i].clone(),
            used: RefCell::new(used_cells[i].borrow().clone()),
            pub_index: pub_index.clone(),
            first_item_offset: units[i]
                .module
                .items
                .first()
                .map(|item| item.span.start)
                .unwrap_or(0),
        };
        let body_probe = match probe_at {
            Some((target, offset)) if target == i => Some(check::BodyProbe {
                offset,
                out: &mut probe,
            }),
            _ => None,
        };
        check::check_module_bodies(
            &table,
            units[i].module,
            &ctx,
            &mut teach_budget,
            &mut out[i],
            &mut goals[i],
            body_probe,
        );

        // The `use` list is the file's exact dependency list; an entry
        // neither signatures nor bodies consumed is dead weight (0010 §1).
        let used = ctx.used.borrow();
        for (path, span) in &ctx.uses {
            if !used.contains(path) {
                out[i].push(Diagnostic::error(
                    DiagCode::UnusedUse,
                    *span,
                    format!("`use {path};` is never used in this file"),
                ));
            }
        }
    }

    (
        ProjectAnalysis {
            diagnostics: out,
            goals,
            table,
        },
        probe,
    )
}
