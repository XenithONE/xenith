//! Definitions: the provisional prelude, the module's own types, and every
//! function signature.
//!
//! Signatures are collected **before any body is checked**, so forward
//! references and mutual recursion need no ordering — the precondition for
//! local-only inference (design/0006 §5).
//!
//! The prelude here is provisional: there is no standard library yet, and the
//! handful of built-in types and methods below exist so that realistic
//! programs — the `examples/` directory in particular — can be checked at all.
//! Everything in it moves to `std/` once modules exist.

use std::collections::HashMap;

use xenith_diag::{DiagCode, Diagnostic, Span};
use xenith_syntax::ast;

use crate::ty::{DefId, EffectSet, Type};

/// A sealed type property. The set is closed: user code cannot implement one,
/// which is why checking a bound is a table lookup and never a search.
/// See design/0006 §3.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Property {
    Eq,
    Ord,
    Hash,
    Copy,
    Text,
}

impl Property {
    pub const ALL: &'static [Property] = &[
        Property::Eq,
        Property::Ord,
        Property::Hash,
        Property::Copy,
        Property::Text,
    ];

    pub fn from_name(name: &str) -> Option<Property> {
        let property = match name {
            "Eq" => Property::Eq,
            "Ord" => Property::Ord,
            "Hash" => Property::Hash,
            "Copy" => Property::Copy,
            "Text" => Property::Text,
            _ => return None,
        };
        Some(property)
    }

    pub fn name(self) -> &'static str {
        match self {
            Property::Eq => "Eq",
            Property::Ord => "Ord",
            Property::Hash => "Hash",
            Property::Copy => "Copy",
            Property::Text => "Text",
        }
    }
}

pub struct FieldInfo {
    pub name: String,
    pub ty: Type,
    pub mutable: bool,
}

pub struct VariantInfo {
    pub name: String,
    /// Payload types, written in terms of the enum's own `Type::Param`s.
    pub payload: Vec<Type>,
}

pub enum DefKind {
    /// A prelude type with no user-visible structure: `Io`, `Error`, `List`,
    /// `Map`, `Shared`, `Task`.
    Opaque,
    Struct {
        fields: Vec<FieldInfo>,
    },
    Enum {
        variants: Vec<VariantInfo>,
    },
}

pub struct DefInfo {
    pub name: String,
    /// Type parameter names, e.g. `["T", "E"]` for `Result`.
    pub generics: Vec<String>,
    pub kind: DefKind,
}

pub struct GenericInfo {
    pub name: String,
    pub bounds: Vec<Property>,
}

/// Where a missing effect would be inserted into this function's header —
/// the payload for XN4001's machine-applicable fix.
#[derive(Clone, Copy)]
pub enum UsesInsertion {
    /// `uses { .. }` exists and is non-empty: insert `, <effect>` before `}`.
    Extend { before_close: u32 },
    /// `uses {}` exists but is empty: insert `<effect>` before `}`.
    Fill { before_close: u32 },
    /// No clause: insert `uses {<effect>} ` before the body's `{`.
    Create { before_body: u32 },
    /// Recovery: no body either. No fix is offered.
    Nowhere,
}

pub struct FnSig {
    pub name: String,
    pub generics: Vec<GenericInfo>,
    pub params: Vec<(String, Type)>,
    pub ret: Type,
    pub effects: EffectSet,
    pub is_async: bool,
    /// Span of the function's name, for diagnostics about the signature.
    pub name_span: Span,
    pub uses_insertion: UsesInsertion,
}

/// A built-in method: `(receiver head, name) -> signature`.
///
/// `Self`-position types are written with `Type::Param` names bound by the
/// receiver ("T", "E") or by the method's own generics.
pub struct MethodSig {
    pub name: &'static str,
    /// Generics beyond those bound by the receiver type.
    pub own_generics: &'static [&'static str],
    pub params: Vec<(&'static str, Type)>,
    pub ret: Type,
    pub effects: EffectSet,
    /// Sealed-property bounds on generics, verified at the call site once the
    /// receiver has bound them: `sorted` demands `T: Ord` (design/0006 §3).
    pub bounds: &'static [(&'static str, Property)],
    /// The receiver is written through in place — `push`/`pop`/`replace` —
    /// so it must be a mutable place, same discipline as `=`.
    pub mutates_receiver: bool,
}

pub struct DefTable {
    defs: Vec<DefInfo>,
    by_name: HashMap<String, DefId>,
    pub fns: Vec<FnSig>,
    fn_by_name: HashMap<String, usize>,

    // Prelude ids, resolved once.
    pub list: DefId,
    pub option: DefId,
    pub result: DefId,
    pub map: DefId,
    pub shared: DefId,
    pub task: DefId,
}

impl DefTable {
    pub fn def(&self, id: DefId) -> &DefInfo {
        &self.defs[id.0 as usize]
    }

    pub fn lookup(&self, name: &str) -> Option<DefId> {
        self.by_name.get(name).copied()
    }

    pub fn fn_named(&self, name: &str) -> Option<&FnSig> {
        self.fn_by_name.get(name).map(|&i| &self.fns[i])
    }

    pub fn name_of(&self, id: DefId) -> String {
        self.defs[id.0 as usize].name.clone()
    }

    /// Search every enum for a variant with this name. Used for the
    /// unqualified spellings `Some`, `None`, `Ok`, `Err` — and, scrutinee
    /// permitting, for variant patterns inside `match`.
    pub fn variant_named(&self, def: DefId, variant: &str) -> Option<&VariantInfo> {
        match &self.def(def).kind {
            DefKind::Enum { variants } => variants.iter().find(|v| v.name == variant),
            _ => None,
        }
    }

    /// The prelude enums whose variants may be used without qualification.
    pub fn unqualified_variant(&self, name: &str) -> Option<(DefId, &VariantInfo)> {
        for def in [self.option, self.result] {
            if let Some(variant) = self.variant_named(def, name) {
                return Some((def, variant));
            }
        }
        None
    }

    /// Built-in methods for a receiver type head. Provisional — the seed of
    /// `std` (see the module comment).
    pub fn methods_of(&self, receiver: &Type) -> Vec<MethodSig> {
        let string = || Type::Str;
        match receiver {
            Type::Int => vec![
                MethodSig {
                    name: "checked_add",
                    own_generics: &[],
                    params: vec![("other", Type::Int)],
                    ret: Type::Named {
                        def: self.option,
                        args: vec![Type::Int],
                    },
                    effects: EffectSet::empty(),
                    bounds: &[],
                    mutates_receiver: false,
                },
                MethodSig {
                    name: "to_text",
                    own_generics: &[],
                    params: vec![],
                    ret: string(),
                    effects: EffectSet::empty(),
                    bounds: &[],
                    mutates_receiver: false,
                },
            ],
            // String — `concat` predates 0007; the rest is the §3 table.
            // `len` counts Unicode scalar values (D2).
            Type::Str => vec![
                MethodSig {
                    name: "concat",
                    own_generics: &[],
                    params: vec![("other", string())],
                    ret: string(),
                    effects: EffectSet::empty(),
                    bounds: &[],
                    mutates_receiver: false,
                },
                MethodSig {
                    name: "len",
                    own_generics: &[],
                    params: vec![],
                    ret: Type::Int,
                    effects: EffectSet::empty(),
                    bounds: &[],
                    mutates_receiver: false,
                },
                MethodSig {
                    name: "split",
                    own_generics: &[],
                    params: vec![("sep", string())],
                    ret: Type::Named {
                        def: self.list,
                        args: vec![string()],
                    },
                    effects: EffectSet::empty(),
                    bounds: &[],
                    mutates_receiver: false,
                },
                MethodSig {
                    name: "trim",
                    own_generics: &[],
                    params: vec![],
                    ret: string(),
                    effects: EffectSet::empty(),
                    bounds: &[],
                    mutates_receiver: false,
                },
                MethodSig {
                    name: "try_to_int",
                    own_generics: &[],
                    params: vec![],
                    ret: Type::Named {
                        def: self.result,
                        args: vec![
                            Type::Int,
                            Type::Named {
                                def: self.lookup("Error").expect("prelude Error"),
                                args: vec![],
                            },
                        ],
                    },
                    effects: EffectSet::empty(),
                    bounds: &[],
                    mutates_receiver: false,
                },
                MethodSig {
                    name: "starts_with",
                    own_generics: &[],
                    params: vec![("prefix", string())],
                    ret: Type::Bool,
                    effects: EffectSet::empty(),
                    bounds: &[],
                    mutates_receiver: false,
                },
                MethodSig {
                    name: "contains",
                    own_generics: &[],
                    params: vec![("sub", string())],
                    ret: Type::Bool,
                    effects: EffectSet::empty(),
                    bounds: &[],
                    mutates_receiver: false,
                },
            ],
            // List<T> — the surface fixed by design/0007 §3. Reads are value
            // copies (D1); the three mutators are the only in-place writes.
            Type::Named { def, .. } if *def == self.list => {
                let t = || Type::Param("T".into());
                let list_t = || Type::Named {
                    def: self.list,
                    args: vec![t()],
                };
                let option_t = || Type::Named {
                    def: self.option,
                    args: vec![t()],
                };
                vec![
                    MethodSig {
                        name: "len",
                        own_generics: &[],
                        params: vec![],
                        ret: Type::Int,
                        effects: EffectSet::empty(),
                        bounds: &[],
                        mutates_receiver: false,
                    },
                    MethodSig {
                        name: "is_empty",
                        own_generics: &[],
                        params: vec![],
                        ret: Type::Bool,
                        effects: EffectSet::empty(),
                        bounds: &[],
                        mutates_receiver: false,
                    },
                    MethodSig {
                        name: "push",
                        own_generics: &[],
                        params: vec![("item", t())],
                        ret: Type::Unit,
                        effects: EffectSet::empty(),
                        bounds: &[],
                        mutates_receiver: true,
                    },
                    MethodSig {
                        name: "pop",
                        own_generics: &[],
                        params: vec![],
                        ret: option_t(),
                        effects: EffectSet::empty(),
                        bounds: &[],
                        mutates_receiver: true,
                    },
                    MethodSig {
                        name: "get",
                        own_generics: &[],
                        params: vec![("index", Type::Int)],
                        ret: option_t(),
                        effects: EffectSet::empty(),
                        bounds: &[],
                        mutates_receiver: false,
                    },
                    MethodSig {
                        name: "replace",
                        own_generics: &[],
                        params: vec![("index", Type::Int), ("value", t())],
                        ret: option_t(),
                        effects: EffectSet::empty(),
                        bounds: &[],
                        mutates_receiver: true,
                    },
                    MethodSig {
                        name: "contains",
                        own_generics: &[],
                        params: vec![("item", t())],
                        ret: Type::Bool,
                        effects: EffectSet::empty(),
                        bounds: &[("T", Property::Eq)],
                        mutates_receiver: false,
                    },
                    MethodSig {
                        name: "sorted",
                        own_generics: &[],
                        params: vec![],
                        ret: list_t(),
                        effects: EffectSet::empty(),
                        bounds: &[("T", Property::Ord)],
                        mutates_receiver: false,
                    },
                    MethodSig {
                        name: "concat",
                        own_generics: &[],
                        params: vec![("other", list_t())],
                        ret: list_t(),
                        effects: EffectSet::empty(),
                        bounds: &[],
                        mutates_receiver: false,
                    },
                    MethodSig {
                        // Non-restrictive while `Text` is total (0006 §3-5);
                        // declared anyway so the surface holds when it narrows.
                        name: "join",
                        own_generics: &[],
                        params: vec![("sep", string())],
                        ret: string(),
                        effects: EffectSet::empty(),
                        bounds: &[("T", Property::Text)],
                        mutates_receiver: false,
                    },
                ]
            }
            // Map<K, V> — design/0007 §3. Every method demands `K: Eq + Hash`
            // here, at the receiver: container type arguments are not
            // bound-checked anywhere else yet, so this is where a `Float` key
            // is refused.
            Type::Named { def, .. } if *def == self.map => {
                let k = || Type::Param("K".into());
                let v = || Type::Param("V".into());
                let option_v = || Type::Named {
                    def: self.option,
                    args: vec![v()],
                };
                const KEY_BOUNDS: &[(&str, Property)] =
                    &[("K", Property::Eq), ("K", Property::Hash)];
                vec![
                    MethodSig {
                        name: "len",
                        own_generics: &[],
                        params: vec![],
                        ret: Type::Int,
                        effects: EffectSet::empty(),
                        bounds: KEY_BOUNDS,
                        mutates_receiver: false,
                    },
                    MethodSig {
                        name: "is_empty",
                        own_generics: &[],
                        params: vec![],
                        ret: Type::Bool,
                        effects: EffectSet::empty(),
                        bounds: KEY_BOUNDS,
                        mutates_receiver: false,
                    },
                    MethodSig {
                        name: "insert",
                        own_generics: &[],
                        params: vec![("key", k()), ("value", v())],
                        ret: option_v(),
                        effects: EffectSet::empty(),
                        bounds: KEY_BOUNDS,
                        mutates_receiver: true,
                    },
                    MethodSig {
                        name: "get",
                        own_generics: &[],
                        params: vec![("key", k())],
                        ret: option_v(),
                        effects: EffectSet::empty(),
                        bounds: KEY_BOUNDS,
                        mutates_receiver: false,
                    },
                    MethodSig {
                        name: "remove",
                        own_generics: &[],
                        params: vec![("key", k())],
                        ret: option_v(),
                        effects: EffectSet::empty(),
                        bounds: KEY_BOUNDS,
                        mutates_receiver: true,
                    },
                    MethodSig {
                        name: "has_key",
                        own_generics: &[],
                        params: vec![("key", k())],
                        ret: Type::Bool,
                        effects: EffectSet::empty(),
                        bounds: KEY_BOUNDS,
                        mutates_receiver: false,
                    },
                    MethodSig {
                        name: "keys",
                        own_generics: &[],
                        params: vec![],
                        ret: Type::Named {
                            def: self.list,
                            args: vec![k()],
                        },
                        effects: EffectSet::empty(),
                        bounds: KEY_BOUNDS,
                        mutates_receiver: false,
                    },
                ]
            }
            Type::Named { def, .. } if *def == self.option => vec![MethodSig {
                // Option<T>.to_result(error: E) -> Result<T, E>
                name: "to_result",
                own_generics: &["E"],
                params: vec![("error", Type::Param("E".into()))],
                ret: Type::Named {
                    def: self.result,
                    args: vec![Type::Param("T".into()), Type::Param("E".into())],
                },
                effects: EffectSet::empty(),
                bounds: &[],
                mutates_receiver: false,
            }],
            Type::Named { def, .. } if self.def(*def).name == "Io" => vec![MethodSig {
                name: "write",
                own_generics: &[],
                params: vec![("text", string())],
                ret: Type::Named {
                    def: self.result,
                    args: vec![
                        Type::Unit,
                        Type::Named {
                            def: self.lookup("Error").expect("prelude Error"),
                            args: vec![],
                        },
                    ],
                },
                effects: EffectSet::new(["Io.write".to_string()]),
                bounds: &[],
                mutates_receiver: false,
            }],
            _ => Vec::new(),
        }
    }

    /// Does `ty` satisfy `property`? Structural, recursive, no user extension
    /// point. `bounds` supplies the properties promised for generic parameters
    /// currently in scope.
    pub fn has_property(&self, ty: &Type, property: Property, bounds: &[GenericInfo]) -> bool {
        // Debug rendering is total by design (0006 §3-5): constraints exist
        // for algorithms, not for diagnostics output.
        if property == Property::Text {
            return true;
        }
        match ty {
            // Silence over poison and holes; a missing type is never the
            // thing to report a property violation about.
            Type::Error | Type::Hole(_) => true,

            Type::Int | Type::Bool | Type::Char | Type::Unit => true,

            // IEEE NaN: equality is honest, ordering and hashing are not.
            Type::Float => matches!(property, Property::Eq | Property::Copy),

            // Heap-backed, so not Copy; everything else holds.
            Type::Str => property != Property::Copy,

            Type::Param(name) => bounds
                .iter()
                .find(|g| g.name == *name)
                .is_some_and(|g| g.bounds.contains(&property)),

            // Functions compare as nothing and copy as nothing.
            Type::Fn { .. } => false,

            Type::Named { def, args } => {
                if *def == self.shared {
                    // Shared identity is compared with `is`, never `==`.
                    return false;
                }
                match property {
                    // Aggregate ordering derived from declaration order would
                    // change when fields are reordered (0006 §3-4).
                    Property::Ord => false,
                    // Aggregates always move.
                    Property::Copy => false,
                    Property::Eq | Property::Hash => {
                        self.components_satisfy(*def, args, property, bounds)
                    }
                    Property::Text => unreachable!("handled above"),
                }
            }
        }
    }

    fn components_satisfy(
        &self,
        def: DefId,
        args: &[Type],
        property: Property,
        bounds: &[GenericInfo],
    ) -> bool {
        let info = self.def(def);
        let instantiate = |ty: &Type| -> Type {
            let bindings: Vec<(String, Type)> = info
                .generics
                .iter()
                .cloned()
                .zip(args.iter().cloned())
                .collect();
            ty.substitute(&bindings)
        };
        match &info.kind {
            // Opaque containers (List, Option, Result, Map, Task): the
            // property holds when it holds for every type argument.
            DefKind::Opaque => args.iter().all(|a| self.has_property(a, property, bounds)),
            DefKind::Struct { fields } => fields
                .iter()
                .all(|f| self.has_property(&instantiate(&f.ty), property, bounds)),
            DefKind::Enum { variants } => variants.iter().all(|v| {
                v.payload
                    .iter()
                    .all(|p| self.has_property(&instantiate(p), property, bounds))
            }),
        }
    }
}

/// Pass A: register every type name. Pass B: lower fields, variants and
/// signatures against the complete name table.
pub fn collect(module: &ast::Module) -> (DefTable, Vec<Diagnostic>) {
    let mut diagnostics = Vec::new();
    let mut defs: Vec<DefInfo> = Vec::new();
    let mut by_name: HashMap<String, DefId> = HashMap::new();

    let register = |name: &str,
                    generics: Vec<String>,
                    defs: &mut Vec<DefInfo>,
                    by_name: &mut HashMap<String, DefId>|
     -> DefId {
        let id = DefId(defs.len() as u32);
        defs.push(DefInfo {
            name: name.to_string(),
            generics,
            kind: DefKind::Opaque,
        });
        by_name.insert(name.to_string(), id);
        id
    };

    // ----- prelude -----
    let list = register("List", vec!["T".into()], &mut defs, &mut by_name);
    let option = register("Option", vec!["T".into()], &mut defs, &mut by_name);
    let result = register(
        "Result",
        vec!["T".into(), "E".into()],
        &mut defs,
        &mut by_name,
    );
    let map = register("Map", vec!["K".into(), "V".into()], &mut defs, &mut by_name);
    let shared = register("Shared", vec!["T".into()], &mut defs, &mut by_name);
    let task = register("Task", vec!["T".into()], &mut defs, &mut by_name);
    register("Io", vec![], &mut defs, &mut by_name);
    register("Error", vec![], &mut defs, &mut by_name);

    defs[option.0 as usize].kind = DefKind::Enum {
        variants: vec![
            VariantInfo {
                name: "Some".into(),
                payload: vec![Type::Param("T".into())],
            },
            VariantInfo {
                name: "None".into(),
                payload: vec![],
            },
        ],
    };
    defs[result.0 as usize].kind = DefKind::Enum {
        variants: vec![
            VariantInfo {
                name: "Ok".into(),
                payload: vec![Type::Param("T".into())],
            },
            VariantInfo {
                name: "Err".into(),
                payload: vec![Type::Param("E".into())],
            },
        ],
    };

    // ----- pass A: user type names -----
    for item in &module.items {
        let (name, span, generics) = match &item.kind {
            ast::ItemKind::Struct(s) => (&s.name, s.name.span, &s.generics),
            ast::ItemKind::Enum(e) => (&e.name, e.name.span, &e.generics),
            _ => continue,
        };
        if name.name.is_empty() {
            continue; // parser recovery
        }
        if by_name.contains_key(&name.name) {
            diagnostics.push(Diagnostic::error(
                DiagCode::DuplicateDefinition,
                span,
                format!("`{}` is declared more than once", name.name),
            ));
            continue;
        }
        register(
            &name.name,
            generics.iter().map(|g| g.name.name.clone()).collect(),
            &mut defs,
            &mut by_name,
        );
    }

    let mut table = DefTable {
        defs,
        by_name,
        fns: Vec::new(),
        fn_by_name: HashMap::new(),
        list,
        option,
        result,
        map,
        shared,
        task,
    };

    // ----- prelude functions -----
    //
    // Map construction is a generic free function (design/0007 D4): there is
    // no associated-function syntax to hang a `Map.new()` on. The type
    // arguments come from expected-type seeding, or fail closed.
    table
        .fn_by_name
        .insert("empty_map".to_string(), table.fns.len());
    table.fns.push(FnSig {
        name: "empty_map".to_string(),
        generics: vec![
            GenericInfo {
                name: "K".to_string(),
                bounds: vec![Property::Eq, Property::Hash],
            },
            GenericInfo {
                name: "V".to_string(),
                bounds: Vec::new(),
            },
        ],
        params: Vec::new(),
        ret: Type::Named {
            def: map,
            args: vec![Type::Param("K".into()), Type::Param("V".into())],
        },
        effects: EffectSet::empty(),
        is_async: false,
        name_span: Span::EMPTY,
        uses_insertion: UsesInsertion::Nowhere,
    });

    // ----- pass B: bodies of type declarations -----
    for item in &module.items {
        match &item.kind {
            ast::ItemKind::Struct(s) => {
                let Some(id) = table.lookup(&s.name.name) else {
                    continue;
                };
                let generic_names: Vec<String> =
                    s.generics.iter().map(|g| g.name.name.clone()).collect();
                let fields = s
                    .fields
                    .iter()
                    .map(|f| FieldInfo {
                        name: f.name.name.clone(),
                        ty: lower_type(&f.ty, &table, &generic_names, &mut diagnostics),
                        mutable: f.mutable,
                    })
                    .collect();
                table.defs[id.0 as usize].kind = DefKind::Struct { fields };
            }
            ast::ItemKind::Enum(e) => {
                let Some(id) = table.lookup(&e.name.name) else {
                    continue;
                };
                let generic_names: Vec<String> =
                    e.generics.iter().map(|g| g.name.name.clone()).collect();
                let variants = e
                    .variants
                    .iter()
                    .map(|v| VariantInfo {
                        name: v.name.name.clone(),
                        payload: v
                            .payload
                            .iter()
                            .map(|t| lower_type(t, &table, &generic_names, &mut diagnostics))
                            .collect(),
                    })
                    .collect();
                table.defs[id.0 as usize].kind = DefKind::Enum { variants };
            }
            _ => {}
        }
    }

    // ----- pass B: function signatures -----
    for item in &module.items {
        let ast::ItemKind::Fn(f) = &item.kind else {
            continue;
        };
        if f.name.name.is_empty() {
            continue;
        }
        if table.fn_by_name.contains_key(&f.name.name) {
            diagnostics.push(Diagnostic::error(
                DiagCode::DuplicateDefinition,
                f.name.span,
                format!("`{}` is declared more than once", f.name.name),
            ));
            continue;
        }

        let generic_names: Vec<String> = f.generics.iter().map(|g| g.name.name.clone()).collect();

        let generics = f
            .generics
            .iter()
            .map(|g| GenericInfo {
                name: g.name.name.clone(),
                bounds: g
                    .bounds
                    .iter()
                    .filter_map(|b| {
                        let property = Property::from_name(&b.name);
                        if property.is_none() {
                            diagnostics.push(Diagnostic::error(
                                DiagCode::UnknownProperty,
                                b.span,
                                format!(
                                    "`{}` is not a sealed property; the set is Eq, Ord, Hash, Copy, Text",
                                    b.name
                                ),
                            ));
                        }
                        property
                    })
                    .collect(),
            })
            .collect();

        let params = f
            .params
            .iter()
            .map(|p| {
                (
                    p.name.name.clone(),
                    lower_type(&p.ty, &table, &generic_names, &mut diagnostics),
                )
            })
            .collect();

        let ret = match &f.return_type {
            Some(ty) => lower_type(ty, &table, &generic_names, &mut diagnostics),
            None => Type::Unit,
        };

        let effects = EffectSet::new(f.effects.iter().flat_map(|set| {
            set.effects.iter().map(|path| {
                path.segments
                    .iter()
                    .map(|s| s.name.as_str())
                    .collect::<Vec<_>>()
                    .join(".")
            })
        }));

        let uses_insertion = match (&f.effects, &f.body) {
            (Some(set), _) if set.effects.is_empty() => UsesInsertion::Fill {
                before_close: set.span.end.saturating_sub(1),
            },
            (Some(set), _) => UsesInsertion::Extend {
                before_close: set.span.end.saturating_sub(1),
            },
            (None, Some(body)) => UsesInsertion::Create {
                before_body: body.span.start,
            },
            (None, None) => UsesInsertion::Nowhere,
        };

        table
            .fn_by_name
            .insert(f.name.name.clone(), table.fns.len());
        table.fns.push(FnSig {
            name: f.name.name.clone(),
            generics,
            params,
            ret,
            effects,
            is_async: f.is_async,
            name_span: f.name.span,
            uses_insertion,
        });
    }

    (table, diagnostics)
}

/// Lower a syntactic type to a semantic one. `generics` are the type
/// parameters in scope. Unknown names become [`Type::Error`] after one
/// diagnostic; type-position holes become [`Type::Hole`] and are collected as
/// goals by the checker.
pub fn lower_type(
    ty: &ast::Type,
    table: &DefTable,
    generics: &[String],
    diagnostics: &mut Vec<Diagnostic>,
) -> Type {
    match &ty.kind {
        ast::TypeKind::Unit => Type::Unit,
        ast::TypeKind::Hole { .. } => {
            // The checker assigns the HoleId and records the goal; at pure
            // lowering time the placeholder is enough.
            Type::Hole(crate::ty::HoleId(u32::MAX))
        }
        ast::TypeKind::Error => Type::Error,
        ast::TypeKind::Fn { .. } => {
            // Parsed for recovery, not shipped (design/0008 §1): function
            // values do not exist (0007 D3), so no annotation may name their
            // type until the closure-effects RFC lands.
            diagnostics.push(Diagnostic::error(
                DiagCode::UnshippedConstruct,
                ty.span,
                "function types are not part of the language yet",
            ));
            Type::Error
        }
        ast::TypeKind::Named { path, args } => {
            let lowered: Vec<Type> = args
                .iter()
                .map(|a| lower_type(a, table, generics, diagnostics))
                .collect();

            // Single-segment names: builtins, generic parameters, then defs.
            if path.segments.len() == 1 {
                let name = path.segments[0].name.as_str();
                let builtin = match name {
                    "Int" => Some(Type::Int),
                    "Float" => Some(Type::Float),
                    "Bool" => Some(Type::Bool),
                    "String" => Some(Type::Str),
                    "Char" => Some(Type::Char),
                    "Unit" => Some(Type::Unit),
                    _ => None,
                };
                if let Some(builtin) = builtin {
                    if !lowered.is_empty() {
                        diagnostics.push(Diagnostic::error(
                            DiagCode::WrongArgumentCount,
                            ty.span,
                            format!("`{name}` takes no type arguments"),
                        ));
                    }
                    return builtin;
                }
                if generics.contains(&name.to_string()) {
                    if !lowered.is_empty() {
                        diagnostics.push(Diagnostic::error(
                            DiagCode::WrongArgumentCount,
                            ty.span,
                            format!("type parameter `{name}` takes no type arguments"),
                        ));
                    }
                    return Type::Param(name.to_string());
                }
                if let Some(def) = table.lookup(name) {
                    let expected = table.def(def).generics.len();
                    if lowered.len() != expected {
                        diagnostics.push(Diagnostic::error(
                            DiagCode::WrongArgumentCount,
                            ty.span,
                            format!(
                                "`{name}` takes {expected} type argument(s), {} given",
                                lowered.len()
                            ),
                        ));
                        return Type::Error;
                    }
                    return Type::Named { def, args: lowered };
                }
            }

            let rendered = path
                .segments
                .iter()
                .map(|s| s.name.as_str())
                .collect::<Vec<_>>()
                .join(".");
            diagnostics.push(Diagnostic::error(
                DiagCode::UnknownType,
                ty.span,
                format!("`{rendered}` does not name a type"),
            ));
            Type::Error
        }
    }
}
