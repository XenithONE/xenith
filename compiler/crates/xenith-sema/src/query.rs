//! Compiler queries: the questions a tool or a model can ask directly.
//!
//! `type_at` answers "what is this, and what is around it?" for any position.
//! It is implemented as a probe riding the ordinary bidirectional traversal —
//! the same claim the holes make: the answer to "what is required here?" is
//! the checker's current state, and a query is a hole the author did not have
//! to write.
//!
//! `producers` answers "what in this module can make me a T?" — the
//! hallucination killer. A model that can ask this does not need to guess
//! function names; the ranked candidates at holes are built from the same
//! machinery.

use xenith_syntax::{ast, parse};

use crate::candidates::return_matches;
use crate::check::{Probe, analyze_at};
use crate::def::{self, DefKind, DefTable};
use crate::ty::{Type, TypeName};

/// The type and surroundings of the innermost expression containing `offset`.
///
/// `None` when the offset is not inside any expression — on a keyword, a type
/// annotation, or blank space. Partial programs answer like any other: the
/// checker runs over recovery nodes too.
pub fn type_at(module: &ast::Module, offset: u32) -> Option<Probe> {
    analyze_at(module, Some(offset)).1
}

/// One way to obtain the queried type.
#[derive(Clone, Debug)]
pub struct Producer {
    /// `"function"`, `"variant"`, or `"struct"`.
    pub kind: &'static str,
    pub symbol: String,
    /// The full shape, as the reader would write it.
    pub signature: String,
    pub effects: Vec<String>,
}

/// Everything in `module` that can produce `type_text`.
///
/// The type is written the way source writes it — `Result<Player, ScoreError>`
/// — and parsed with the module's own definitions, so the answer uses the same
/// names the asker used.
pub fn producers(module: &ast::Module, type_text: &str) -> Result<Vec<Producer>, String> {
    let (table, _) = def::collect(module);
    let target = parse_type(type_text, &table)?;

    let render = |ty: &Type| -> String {
        let name_of = |id| table.name_of(id);
        TypeName {
            ty,
            name_of: &name_of,
        }
        .to_string()
    };

    let mut found = Vec::new();

    // ----- functions whose return type matches -----
    for sig in &table.fns {
        let ret = if sig.is_async {
            Type::Named {
                def: table.task,
                args: vec![sig.ret.clone()],
            }
        } else {
            sig.ret.clone()
        };
        let mut bindings: Vec<(String, Type)> = Vec::new();
        if !return_matches(&ret, &target, &mut bindings) {
            continue;
        }
        let params: Vec<String> = sig
            .params
            .iter()
            .map(|(name, ty)| format!("{name}: {}", render(&ty.substitute(&bindings))))
            .collect();
        let mut signature = format!(
            "{}({}) -> {}",
            sig.name,
            params.join(", "),
            render(&ret.substitute(&bindings))
        );
        if !sig.effects.is_empty() {
            signature.push_str(&format!(" uses {}", sig.effects));
        }
        found.push(Producer {
            kind: "function",
            symbol: sig.name.clone(),
            signature,
            effects: sig.effects.iter().map(String::from).collect(),
        });
    }

    // ----- constructors of the type itself -----
    if let Type::Named { def, args } = &target {
        let info = table.def(*def);
        let bindings: Vec<(String, Type)> = info
            .generics
            .iter()
            .cloned()
            .zip(args.iter().cloned())
            .collect();
        match &info.kind {
            DefKind::Enum { variants } => {
                let unqualified = *def == table.option || *def == table.result;
                for variant in variants {
                    let shown = if unqualified {
                        variant.name.clone()
                    } else {
                        format!("{}.{}", info.name, variant.name)
                    };
                    let signature = if variant.payload.is_empty() {
                        shown.clone()
                    } else {
                        format!(
                            "{shown}({})",
                            variant
                                .payload
                                .iter()
                                .map(|p| render(&p.substitute(&bindings)))
                                .collect::<Vec<_>>()
                                .join(", ")
                        )
                    };
                    found.push(Producer {
                        kind: "variant",
                        symbol: shown,
                        signature,
                        effects: Vec::new(),
                    });
                }
            }
            DefKind::Struct { fields } => {
                let rendered: Vec<String> = fields
                    .iter()
                    .map(|f| format!("{}: {}", f.name, render(&f.ty.substitute(&bindings))))
                    .collect();
                found.push(Producer {
                    kind: "struct",
                    symbol: info.name.clone(),
                    signature: format!("{} {{ {} }}", info.name, rendered.join(", ")),
                    effects: Vec::new(),
                });
            }
            DefKind::Opaque => {}
        }
    }

    found.sort_by(|a, b| a.kind.cmp(b.kind).then(a.symbol.cmp(&b.symbol)));
    Ok(found)
}

/// Parse a type the way source spells it, against the module's definitions.
///
/// Reuses the real parser by wrapping the text as a parameter annotation, so
/// query syntax and language syntax can never drift apart.
fn parse_type(type_text: &str, table: &DefTable) -> Result<Type, String> {
    let wrapped = format!("fn __query(x: {type_text}) {{}}");
    let parsed = parse(&wrapped);
    if !parsed.diagnostics.is_empty() {
        return Err(format!("`{type_text}` does not parse as a type"));
    }
    let ast::ItemKind::Fn(f) = &parsed.module.items[0].kind else {
        return Err(format!("`{type_text}` does not parse as a type"));
    };
    let Some(param) = f.params.first() else {
        return Err(format!("`{type_text}` does not parse as a type"));
    };

    let mut diagnostics = Vec::new();
    let lowered = def::lower_type(&param.ty, table, &[], &mut diagnostics);
    if !diagnostics.is_empty() || lowered == Type::Error {
        return Err(format!(
            "`{type_text}` names something this module does not declare"
        ));
    }
    Ok(lowered)
}
