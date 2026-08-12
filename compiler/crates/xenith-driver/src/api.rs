//! The ApiSurface semantic model and its renderers (design/0013 §2).
//!
//! What is shared is the *model* — the reachable public API of a project as
//! structure: modules, their pub functions, structs, enums, consts and
//! effect sets, in deterministic order. Text, JSON and the frozen bench dump
//! are three renderers over it. The bench renderer is a compatibility layer:
//! its format is frozen by the design/0011 artifacts and must not constrain
//! the model, so its quirks — the hash header, the excluded entry module —
//! live in [`render_bench_dump`] and nowhere else.
//!
//! Types render as their exact source spelling, captured when the model is
//! built: the surface describes what a caller must write, and a re-print
//! cannot drift from the source it was sliced from.

use serde_json::{Value, json};

use crate::project::Project;

/// The version of the JSON rendering's shape. Independent of the wire
/// `schema_version` — the api payload is its own contract, versioned in the
/// payload itself, and a breaking change bumps this number (design/0013 §2).
pub const API_SCHEMA_VERSION: u32 = 1;

/// The first line of a bench dump. Recorded in every dump so a stale
/// artifact names the generator that made it (design/0011 §7).
pub const BENCH_DUMP_VERSION: &str = "xenith-bench api-dump v1";

/// The public API of one project: modules sorted by path.
pub struct ApiSurface {
    pub modules: Vec<ApiModule>,
}

/// One module's public items, each kind sorted by its rendered form —
/// deterministic, and byte-compatible with the frozen dump ordering.
pub struct ApiModule {
    /// Dotted module path ("game.player").
    pub path: String,
    pub structs: Vec<ApiStruct>,
    pub enums: Vec<ApiEnum>,
    pub consts: Vec<ApiConst>,
    pub fns: Vec<ApiFn>,
}

impl ApiModule {
    pub fn is_empty(&self) -> bool {
        self.structs.is_empty()
            && self.enums.is_empty()
            && self.consts.is_empty()
            && self.fns.is_empty()
    }
}

pub struct ApiGeneric {
    pub name: String,
    pub bounds: Vec<String>,
}

pub struct ApiParam {
    pub name: String,
    /// The type as source spells it.
    pub ty: String,
}

pub struct ApiFn {
    pub name: String,
    pub generics: Vec<ApiGeneric>,
    pub params: Vec<ApiParam>,
    /// `None` when the function returns nothing (no `->` in the source).
    pub returns: Option<String>,
    /// `None` when the signature carries no `uses` clause at all; an
    /// annotated-empty `uses {}` is `Some(vec![])` — the two spell different
    /// promises and render differently.
    pub effects: Option<Vec<String>>,
}

pub struct ApiField {
    pub name: String,
    pub mutable: bool,
    pub ty: String,
}

pub struct ApiStruct {
    pub name: String,
    pub generics: Vec<ApiGeneric>,
    pub fields: Vec<ApiField>,
}

pub struct ApiVariant {
    pub name: String,
    pub payload: Vec<String>,
}

pub struct ApiEnum {
    pub name: String,
    pub generics: Vec<ApiGeneric>,
    pub variants: Vec<ApiVariant>,
}

pub struct ApiConst {
    pub name: String,
    /// The value expression stays out: the surface describes a shape, and a
    /// const's value is implementation.
    pub ty: String,
}

/// Build the model from a loaded project.
///
/// A project with layout problems has no trustworthy module map, and a file
/// that does not parse has no trustworthy surface — both refuse rather than
/// describe a guess.
pub fn surface(project: &Project) -> Result<ApiSurface, String> {
    if let Some((rel, diagnostic)) = project.layout.first() {
        return Err(format!("{rel}: {}", diagnostic.message));
    }
    let mut modules = Vec::new();
    for file in &project.files {
        if file.parsed.has_errors() {
            return Err(format!(
                "src/{}: does not parse; refusing to dump a guessed surface",
                file.rel
            ));
        }
        modules.push(module_of(&file.module, &file.source, &file.parsed.module));
    }
    modules.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(ApiSurface { modules })
}

impl ApiSurface {
    /// The surface restricted to one module and its submodules, dotted.
    /// `None` when nothing matches — an unknown module is an error at the
    /// caller's surface, never an empty answer (design/0013 §2: scoped
    /// queries are first-class because full dumps break token budgets).
    pub fn scoped(&self, module: &str) -> Option<ApiSurface> {
        let prefix = format!("{module}.");
        let modules: Vec<ApiModule> = self
            .modules
            .iter()
            .filter(|m| m.path == module || m.path.starts_with(&prefix))
            .map(clone_module)
            .collect();
        if modules.is_empty() {
            None
        } else {
            Some(ApiSurface { modules })
        }
    }
}

fn clone_module(module: &ApiModule) -> ApiModule {
    ApiModule {
        path: module.path.clone(),
        structs: module
            .structs
            .iter()
            .map(|s| ApiStruct {
                name: s.name.clone(),
                generics: clone_generics(&s.generics),
                fields: s
                    .fields
                    .iter()
                    .map(|f| ApiField {
                        name: f.name.clone(),
                        mutable: f.mutable,
                        ty: f.ty.clone(),
                    })
                    .collect(),
            })
            .collect(),
        enums: module
            .enums
            .iter()
            .map(|e| ApiEnum {
                name: e.name.clone(),
                generics: clone_generics(&e.generics),
                variants: e
                    .variants
                    .iter()
                    .map(|v| ApiVariant {
                        name: v.name.clone(),
                        payload: v.payload.clone(),
                    })
                    .collect(),
            })
            .collect(),
        consts: module
            .consts
            .iter()
            .map(|c| ApiConst {
                name: c.name.clone(),
                ty: c.ty.clone(),
            })
            .collect(),
        fns: module
            .fns
            .iter()
            .map(|f| ApiFn {
                name: f.name.clone(),
                generics: clone_generics(&f.generics),
                params: f
                    .params
                    .iter()
                    .map(|p| ApiParam {
                        name: p.name.clone(),
                        ty: p.ty.clone(),
                    })
                    .collect(),
                returns: f.returns.clone(),
                effects: f.effects.clone(),
            })
            .collect(),
    }
}

fn clone_generics(generics: &[ApiGeneric]) -> Vec<ApiGeneric> {
    generics
        .iter()
        .map(|g| ApiGeneric {
            name: g.name.clone(),
            bounds: g.bounds.clone(),
        })
        .collect()
}

// ----------------------------------------------------------- model building

fn module_of(path: &str, source: &str, ast: &xenith_syntax::ast::Module) -> ApiModule {
    use xenith_syntax::ast::ItemKind;

    let mut module = ApiModule {
        path: path.to_string(),
        structs: Vec::new(),
        enums: Vec::new(),
        consts: Vec::new(),
        fns: Vec::new(),
    };
    for item in &ast.items {
        match &item.kind {
            ItemKind::Struct(s) if s.is_pub => module.structs.push(ApiStruct {
                name: s.name.name.clone(),
                generics: generics_of(&s.generics),
                fields: s
                    .fields
                    .iter()
                    .map(|field| ApiField {
                        name: field.name.name.clone(),
                        mutable: field.mutable,
                        ty: type_text(source, &field.ty),
                    })
                    .collect(),
            }),
            ItemKind::Enum(e) if e.is_pub => module.enums.push(ApiEnum {
                name: e.name.name.clone(),
                generics: generics_of(&e.generics),
                variants: e
                    .variants
                    .iter()
                    .map(|variant| ApiVariant {
                        name: variant.name.name.clone(),
                        payload: variant
                            .payload
                            .iter()
                            .map(|ty| type_text(source, ty))
                            .collect(),
                    })
                    .collect(),
            }),
            ItemKind::Const(c) if c.is_pub => module.consts.push(ApiConst {
                name: c.name.name.clone(),
                ty: type_text(source, &c.ty),
            }),
            ItemKind::Fn(f) if f.is_pub => module.fns.push(ApiFn {
                name: f.name.name.clone(),
                generics: generics_of(&f.generics),
                params: f
                    .params
                    .iter()
                    .map(|param| ApiParam {
                        name: param.name.name.clone(),
                        ty: type_text(source, &param.ty),
                    })
                    .collect(),
                returns: f.return_type.as_ref().map(|ret| type_text(source, ret)),
                effects: f.effects.as_ref().map(|effects| {
                    effects
                        .effects
                        .iter()
                        .map(|path| {
                            path.segments
                                .iter()
                                .map(|s| s.name.as_str())
                                .collect::<Vec<_>>()
                                .join(".")
                        })
                        .collect()
                }),
            }),
            _ => {}
        }
    }
    // Deterministic order: each kind sorted by its rendered form, which is
    // also exactly the frozen dump's order — sorting the structures by the
    // strings they render to keeps one comparison for all three renderers.
    module.structs.sort_by_key(render_struct);
    module.enums.sort_by_key(render_enum);
    module.consts.sort_by_key(render_const);
    module.fns.sort_by_key(render_fn);
    module
}

fn generics_of(generics: &[xenith_syntax::ast::GenericParam]) -> Vec<ApiGeneric> {
    generics
        .iter()
        .map(|generic| ApiGeneric {
            name: generic.name.name.clone(),
            bounds: generic
                .bounds
                .iter()
                .map(|bound| bound.name.clone())
                .collect(),
        })
        .collect()
}

/// A type as its exact source spelling — the sources are the truth, so the
/// slice is as deterministic as a re-print and cannot drift from what a
/// caller must actually write.
fn type_text(source: &str, ty: &xenith_syntax::ast::Type) -> String {
    ty.span.slice(source).unwrap_or("<unprintable>").to_string()
}

// -------------------------------------------------------------- renderers

fn render_generics(generics: &[ApiGeneric]) -> String {
    if generics.is_empty() {
        return String::new();
    }
    let mut out = String::from("<");
    for (i, generic) in generics.iter().enumerate() {
        if i > 0 {
            out.push_str(", ");
        }
        out.push_str(&generic.name);
        if !generic.bounds.is_empty() {
            out.push_str(": ");
            out.push_str(&generic.bounds.join(" + "));
        }
    }
    out.push('>');
    out
}

fn render_fn(f: &ApiFn) -> String {
    let mut out = format!("pub fn {}{}(", f.name, render_generics(&f.generics));
    for (i, param) in f.params.iter().enumerate() {
        if i > 0 {
            out.push_str(", ");
        }
        out.push_str(&param.name);
        out.push_str(": ");
        out.push_str(&param.ty);
    }
    out.push(')');
    if let Some(ret) = &f.returns {
        out.push_str(" -> ");
        out.push_str(ret);
    }
    if let Some(effects) = &f.effects {
        out.push_str(" uses {");
        out.push_str(&effects.join(", "));
        out.push('}');
    }
    out
}

fn render_struct(s: &ApiStruct) -> String {
    let mut out = format!("pub struct {}{} {{\n", s.name, render_generics(&s.generics));
    for field in &s.fields {
        out.push_str("    ");
        if field.mutable {
            out.push_str("var ");
        }
        out.push_str(&field.name);
        out.push_str(": ");
        out.push_str(&field.ty);
        out.push_str(",\n");
    }
    out.push('}');
    out
}

fn render_enum(e: &ApiEnum) -> String {
    let mut out = format!("pub enum {}{} {{\n", e.name, render_generics(&e.generics));
    for variant in &e.variants {
        out.push_str("    ");
        out.push_str(&variant.name);
        if !variant.payload.is_empty() {
            out.push('(');
            out.push_str(&variant.payload.join(", "));
            out.push(')');
        }
        out.push_str(",\n");
    }
    out.push('}');
    out
}

fn render_const(c: &ApiConst) -> String {
    format!("pub const {}: {}", c.name, c.ty)
}

/// One module's rendered items, kinds in the frozen order (structs, enums,
/// consts, functions), each already sorted by its rendered form.
fn rendered_items(module: &ApiModule) -> Vec<String> {
    let mut items = Vec::new();
    items.extend(module.structs.iter().map(render_struct));
    items.extend(module.enums.iter().map(render_enum));
    items.extend(module.consts.iter().map(render_const));
    items.extend(module.fns.iter().map(render_fn));
    items
}

fn module_section(module: &ApiModule) -> String {
    let mut out = format!("module {}\n", module.path);
    let items = rendered_items(module);
    if items.is_empty() {
        out.push_str("\n(no public items)\n");
        return out;
    }
    for item in items {
        out.push('\n');
        out.push_str(&item);
        out.push('\n');
    }
    out
}

/// The text rendering, for humans and agents: every module, no hash header.
pub fn render_text(surface: &ApiSurface) -> String {
    let sections: Vec<String> = surface.modules.iter().map(module_section).collect();
    sections.join("\n")
}

/// The JSON rendering. The payload carries its own `api_schema_version`,
/// deliberately independent of the wire `schema_version` (design/0013 §2).
pub fn render_json(surface: &ApiSurface) -> Value {
    let modules: Vec<Value> = surface
        .modules
        .iter()
        .map(|module| {
            json!({
                "module": module.path,
                "structs": module.structs.iter().map(|s| json!({
                    "name": s.name,
                    "generics": generics_json(&s.generics),
                    "fields": s.fields.iter().map(|f| json!({
                        "name": f.name,
                        "mutable": f.mutable,
                        "type": f.ty,
                    })).collect::<Vec<_>>(),
                })).collect::<Vec<_>>(),
                "enums": module.enums.iter().map(|e| json!({
                    "name": e.name,
                    "generics": generics_json(&e.generics),
                    "variants": e.variants.iter().map(|v| json!({
                        "name": v.name,
                        "payload": v.payload,
                    })).collect::<Vec<_>>(),
                })).collect::<Vec<_>>(),
                "consts": module.consts.iter().map(|c| json!({
                    "name": c.name,
                    "type": c.ty,
                })).collect::<Vec<_>>(),
                "functions": module.fns.iter().map(|f| json!({
                    "name": f.name,
                    "generics": generics_json(&f.generics),
                    "params": f.params.iter().map(|p| json!({
                        "name": p.name,
                        "type": p.ty,
                    })).collect::<Vec<_>>(),
                    "returns": f.returns,
                    "effects": f.effects,
                    "signature": render_fn(f),
                })).collect::<Vec<_>>(),
            })
        })
        .collect();
    json!({
        "api_schema_version": API_SCHEMA_VERSION,
        "modules": modules,
    })
}

fn generics_json(generics: &[ApiGeneric]) -> Vec<Value> {
    generics
        .iter()
        .map(|g| json!({ "name": g.name, "bounds": g.bounds }))
        .collect()
}

/// The frozen bench-dump rendering (design/0011 §7), byte-compatible with
/// every dump under `bench/ai/tasks-t5/`: version and hash header, then one
/// section per module — the entry module `main` excluded, because in the
/// bench it is the calling contract, not a provided library surface.
pub fn render_bench_dump(surface: &ApiSurface) -> String {
    let sections: Vec<String> = surface
        .modules
        .iter()
        .filter(|module| module.path != "main")
        .map(module_section)
        .collect();
    let body = sections.join("\n");
    let hash = fnv1a64(&body);
    format!("# {BENCH_DUMP_VERSION}\n# hash: fnv1a64:{hash:016x}\n\n{body}")
}

/// FNV-1a 64. Not cryptographic and not meant to be: the hash line exists so
/// a stale or hand-edited dump is *detected*; byte comparison against a
/// regeneration is the actual gate.
pub fn fnv1a64(text: &str) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in text.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

#[cfg(test)]
mod tests {
    use super::*;

    fn project_in(dir: &std::path::Path) -> Project {
        crate::project::load(dir).expect("the fixture loads")
    }

    fn fixture(name: &str) -> std::path::PathBuf {
        // One directory per test: the suite runs in parallel, and a shared
        // fixture would be torn down under a sibling's feet.
        let dir =
            std::env::temp_dir().join(format!("xenith-api-model-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src/depot")).unwrap();
        std::fs::write(dir.join("xenith.toml"), "name = \"api-test\"\n").unwrap();
        std::fs::write(
            dir.join("src/depot/locker.xn"),
            "pub struct Locker {\n    label: String,\n    var weight: Int,\n}\n\n\
             fn hidden() -> Int {\n    1\n}\n\n\
             pub enum Rank {\n    Bronze,\n    Gold(Int),\n}\n\n\
             pub const CAP: Int = 40;\n\n\
             pub fn emit(io: Io, total: Int) -> Result<Unit, Error> uses {Io.write} {\n\
                 io.write(text: total.to_text())\n}\n\n\
             pub fn zero() -> Int uses {} {\n    0\n}\n",
        )
        .unwrap();
        std::fs::write(dir.join("src/main.xn"), "fn main() {\n}\n").unwrap();
        dir
    }

    #[test]
    fn the_model_orders_modules_and_items_deterministically() {
        let dir = fixture("orders");
        let surface = surface(&project_in(&dir)).unwrap();
        std::fs::remove_dir_all(&dir).ok();

        let paths: Vec<&str> = surface.modules.iter().map(|m| m.path.as_str()).collect();
        assert_eq!(paths, ["depot.locker", "main"]);
        let locker = &surface.modules[0];
        assert_eq!(locker.structs[0].name, "Locker");
        assert_eq!(locker.structs[0].fields[1].ty, "Int");
        assert!(locker.structs[0].fields[1].mutable);
        assert_eq!(locker.enums[0].variants[1].payload, ["Int"]);
        assert_eq!(locker.consts[0].ty, "Int");
        let names: Vec<&str> = locker.fns.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(names, ["emit", "zero"], "name-sorted");
        // Private items never enter the model.
        assert!(!names.contains(&"hidden"));
        // `uses {Io.write}` vs `uses {}` vs no clause are three shapes.
        assert_eq!(locker.fns[0].effects, Some(vec!["Io.write".to_string()]));
        assert_eq!(locker.fns[1].effects, Some(vec![]));
        assert!(surface.modules[1].fns.is_empty(), "main is not pub");
    }

    #[test]
    fn the_json_rendering_carries_its_own_schema_version() {
        let dir = fixture("json");
        let rendered = render_json(&surface(&project_in(&dir)).unwrap());
        std::fs::remove_dir_all(&dir).ok();

        assert_eq!(rendered["api_schema_version"], API_SCHEMA_VERSION);
        assert!(
            rendered.get("schema_version").is_none(),
            "the api payload versions itself, not the wire"
        );
        let module = &rendered["modules"][0];
        assert_eq!(module["module"], "depot.locker");
        assert_eq!(
            module["functions"][0]["signature"],
            "pub fn emit(io: Io, total: Int) -> Result<Unit, Error> uses {Io.write}"
        );
        assert_eq!(module["functions"][1]["effects"], json!([]));
    }

    #[test]
    fn scoping_keeps_a_subtree_and_refuses_the_unknown() {
        let dir = fixture("scoped");
        let surface = surface(&project_in(&dir)).unwrap();
        std::fs::remove_dir_all(&dir).ok();

        let scoped = surface.scoped("depot.locker").expect("the module exists");
        assert_eq!(scoped.modules.len(), 1);
        let parent = surface.scoped("depot").expect("the subtree matches");
        assert_eq!(parent.modules.len(), 1);
        assert!(surface.scoped("depot.lock").is_none(), "no prefix guessing");
        assert!(surface.scoped("nowhere").is_none());
    }

    #[test]
    fn the_text_rendering_includes_every_module_without_the_dump_header() {
        let dir = fixture("text");
        let text = render_text(&surface(&project_in(&dir)).unwrap());
        std::fs::remove_dir_all(&dir).ok();

        assert!(text.starts_with("module depot.locker\n"), "{text}");
        assert!(
            text.contains("\nmodule main\n\n(no public items)\n"),
            "{text}"
        );
        assert!(!text.contains("api-dump"), "no bench header in text");
    }

    #[test]
    fn the_bench_rendering_excludes_main_and_hashes_its_body() {
        let dir = fixture("bench");
        let dump = render_bench_dump(&surface(&project_in(&dir)).unwrap());
        std::fs::remove_dir_all(&dir).ok();

        assert!(dump.starts_with(&format!("# {BENCH_DUMP_VERSION}\n# hash: fnv1a64:")));
        assert!(!dump.contains("module main"), "{dump}");
        let body_at = dump.find("\n\n").unwrap() + 2;
        let recorded = dump
            .lines()
            .nth(1)
            .and_then(|line| line.strip_prefix("# hash: fnv1a64:"))
            .unwrap();
        assert_eq!(recorded, format!("{:016x}", fnv1a64(&dump[body_at..])));
    }

    #[test]
    fn a_parse_error_refuses_the_whole_surface() {
        let dir = std::env::temp_dir().join(format!("xenith-api-broken-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(dir.join("xenith.toml"), "\n").unwrap();
        std::fs::write(dir.join("src/broken.xn"), "pub fn ) nope\n").unwrap();
        let result = surface(&project_in(&dir));
        std::fs::remove_dir_all(&dir).ok();
        let Err(message) = result else {
            panic!("a guessed surface must be refused");
        };
        assert!(message.contains("src/broken.xn"), "{message}");
    }
}
