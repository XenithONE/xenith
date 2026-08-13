//! JSON-RPC 2.0 message handling for the MCP stdio transport.
//!
//! One function matters: [`handle_message`]. It takes one decoded message and
//! the workspace root, and returns the response to write, or `None` for
//! notifications. The main loop is a thin pipe around it, which is also what
//! makes the protocol testable without spawning a process.
//!
//! Every tool that takes a path is confined to the workspace root: the path
//! is canonicalized and refused unless it lands inside the canonicalized
//! root. A server spawned for one project must not read — or, via
//! `fmt write=true`, rewrite — files elsewhere on the machine just because a
//! client asked.

use std::path::{Path, PathBuf};

use serde_json::{Value, json};
use xenith_diag::{DiagCode, LineIndex};
use xenith_driver::project::{ModeRequest, Project, ProjectRequest, ProjectSnapshot};

/// Protocol revisions this server is known to serve correctly. The subset we
/// implement — initialize, tools/list, tools/call, ping — is identical across
/// them, so we echo the client's choice when we recognise it and offer the
/// oldest otherwise.
const KNOWN_PROTOCOL_VERSIONS: &[&str] = &["2024-11-05", "2025-03-26", "2025-06-18"];

/// Startup choices that change the tool surface.
#[derive(Default)]
pub struct ServerOptions {
    /// Expose the `api_surface` tool. Off the default list deliberately
    /// (design/0013 §2): the surface is experimental, and an unstable tool a
    /// model discovers by accident is a contract nobody signed.
    pub experimental_api_surface: bool,
}

/// [`handle_message_with`] under the default options — the surface every
/// existing caller and test already speaks.
pub fn handle_message(message: &Value, workspace_root: &Path) -> Option<Value> {
    handle_message_with(message, workspace_root, &ServerOptions::default())
}

/// Handle one decoded JSON-RPC message. `None` means nothing is written back
/// (notifications, and responses addressed to us, which we do not send).
///
/// `workspace_root` is the directory the file-taking tools are confined to.
/// It is compared canonical-to-canonical on every call, so passing it
/// uncanonicalized (as `main` does) is fine.
pub fn handle_message_with(
    message: &Value,
    workspace_root: &Path,
    options: &ServerOptions,
) -> Option<Value> {
    let id = message.get("id").cloned();
    let Some(method) = message.get("method").and_then(Value::as_str) else {
        // Not a request. If it carries an id it deserves an error; a
        // response-shaped message without a method is silently dropped.
        return id
            .filter(|id| !id.is_null())
            .map(|id| error(id, -32600, "not a valid request"));
    };
    let params = message.get("params").cloned().unwrap_or(Value::Null);

    match (method, id) {
        // Notifications: no id, no reply.
        (_, None) => None,

        ("initialize", Some(id)) => {
            let requested = params
                .get("protocolVersion")
                .and_then(Value::as_str)
                .unwrap_or("");
            let version = if KNOWN_PROTOCOL_VERSIONS.contains(&requested) {
                requested
            } else {
                KNOWN_PROTOCOL_VERSIONS[0]
            };
            Some(result(
                id,
                json!({
                    "protocolVersion": version,
                    "capabilities": { "tools": {} },
                    "serverInfo": {
                        "name": "xenith-mcp",
                        "version": env!("CARGO_PKG_VERSION"),
                    },
                }),
            ))
        }

        ("ping", Some(id)) => Some(result(id, json!({}))),

        ("tools/list", Some(id)) => Some(result(id, json!({ "tools": tool_definitions(options) }))),

        ("tools/call", Some(id)) => {
            let name = params.get("name").and_then(Value::as_str).unwrap_or("");
            let arguments = params.get("arguments").cloned().unwrap_or(json!({}));
            let known = TOOL_NAMES.contains(&name)
                || (name == "api_surface" && options.experimental_api_surface);
            if !known {
                return Some(error(id, -32602, &format!("no tool named `{name}`")));
            }
            let reply = match call_tool(name, &arguments, workspace_root) {
                Ok(text) => json!({
                    "content": [{ "type": "text", "text": text }],
                    "isError": false,
                }),
                Err(text) => json!({
                    "content": [{ "type": "text", "text": text }],
                    "isError": true,
                }),
            };
            Some(result(id, reply))
        }

        (_, Some(id)) => Some(error(id, -32601, &format!("method `{method}` not found"))),
    }
}

fn result(id: Value, value: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": value })
}

fn error(id: Value, code: i64, message: &str) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": { "code": code, "message": message },
    })
}

// --------------------------------------------------------------------- tools

const TOOL_NAMES: &[&str] = &[
    "check",
    "goals",
    "type_at",
    "producers",
    "fmt",
    "explain",
    "run",
];

/// The tool list is the context a model reads before calling anything, so the
/// descriptions carry the usage rules — they are product surface, not
/// boilerplate.
fn tool_definitions(options: &ServerOptions) -> Vec<Value> {
    let path_property = json!({
        "type": "string",
        "description": "Path to a .xn file, absolute or relative to the workspace root the \
            server was started with (`--workspace-root`, default: its working directory). \
            Paths outside the workspace root are refused.",
    });
    let mode_property = json!({
        "type": "string",
        "enum": ["auto", "project", "single_file"],
        "description": "How to analyze the path. \"auto\" (default): whole-project analysis \
            when a `xenith.toml` manifest governs the file, single-file otherwise. \
            \"project\" demands project analysis and errors when there is no manifest. \
            \"single_file\" analyzes the one file even inside a project. Discovery \
            failures — a broken project, a containment violation, an invalid layout — \
            are errors, never a silent single-file fallback. Responses carry the \
            `analysis_mode` that actually ran.",
    });
    let mut tools = vec![
        json!({
            "name": "check",
            "description": "Parse and type-check a Xenith file. Returns diagnostics as JSON: \
                stable codes, byte spans, line/column, and machine-applicable fixes where the \
                repair is unambiguous. An empty diagnostics array means the file is clean. \
                Inside a project (a `xenith.toml` above the file), the whole project is \
                checked: the response lists every file's diagnostics, the requested file \
                first, the rest in path order.",
            "inputSchema": {
                "type": "object",
                "properties": { "path": path_property, "mode": mode_property },
                "required": ["path"],
            },
        }),
        json!({
            "name": "goals",
            "description": "Report every typed hole (`??` / `??name`) in a Xenith file: the type \
                required there, the bindings in scope with their types, the effects permitted, \
                ranked candidate scaffolds (nested holes mark what still needs deciding), and \
                symbols blocked by the effect budget with the reason. A partial program is a \
                normal state — write holes, then ask this. Inside a project the whole \
                project's holes are reported, the requested file's first.",
            "inputSchema": {
                "type": "object",
                "properties": { "path": path_property, "mode": mode_property },
                "required": ["path"],
            },
        }),
        json!({
            "name": "type_at",
            "description": "The type of the expression or binding at a position, with the scope \
                and effect budget around it. Positions are one-based; column counts characters. \
                Works on partial programs. Inside a project the file is checked with the whole \
                project's declarations in view, so cross-module types answer qualified.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "path": path_property,
                    "line": { "type": "integer", "description": "One-based line." },
                    "column": { "type": "integer", "description": "One-based column, counting characters." },
                    "mode": mode_property,
                },
                "required": ["path", "line", "column"],
            },
        }),
        json!({
            "name": "producers",
            "description": "Everything in the file's scope that can produce a given type: \
                functions (generics instantiated in the answer, effects shown), enum variants, \
                and the struct literal shape. Ask this instead of guessing a function name. An \
                unknown type is an error, not an empty list. Inside a project the scope is the \
                file's own: its items, the pub items of modules it `use`s, and the prelude.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "path": path_property,
                    "type": {
                        "type": "string",
                        "description": "The type as source spells it, e.g. \"Result<Player, ScoreError>\".",
                    },
                    "mode": mode_property,
                },
                "required": ["path", "type"],
            },
        }),
        json!({
            "name": "fmt",
            "description": "Canonical formatting: the same meaning always produces the same \
                bytes, no options. With write=false (default) returns the formatted text without \
                touching the file. The formatter verifies its own output and refuses rather than \
                risk changing meaning; source that does not parse is not formatted.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "path": path_property,
                    "write": {
                        "type": "boolean",
                        "description": "Rewrite the file in place when it would change. Default false.",
                    },
                },
                "required": ["path"],
            },
        }),
        json!({
            "name": "explain",
            "description": "The full rule behind a diagnostic code, e.g. \"XN4001\". Case-insensitive.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "code": { "type": "string", "description": "A code such as XN3005." },
                },
                "required": ["code"],
            },
        }),
        json!({
            "name": "run",
            "description": "Type-check and execute a file's `fn main`, returning captured stdout \
                and an exit code: 0 = succeeded, 1 = main returned Err, 2 = refused because the \
                file has diagnostics (fix them first), 101 = a runtime trap fired (overflow, \
                division by zero — or a hole was reached, in which case the trap names it and the \
                next step is `goals`). Deterministic: strict left-to-right evaluation, trapping \
                arithmetic. Inside a project the whole project runs, entered at `src/main.xn`, \
                whichever file was named.",
            "inputSchema": {
                "type": "object",
                "properties": { "path": path_property, "mode": mode_property },
                "required": ["path"],
            },
        }),
    ];
    if options.experimental_api_surface {
        tools.push(json!({
            "name": "api_surface",
            "description": "EXPERIMENTAL (behind `--experimental-api-surface`; shape may \
                change). The reachable public API of a project as structured JSON: per module, \
                the pub fn signatures, pub structs, pub enums, pub consts and effect sets, in \
                deterministic order. Scope with `module` to stay inside a token budget. An API \
                map is not a substitute for wiring knowledge — in the 0011 measurements it \
                solved implementation tasks (17/56) but wired nothing (0/28): knowing the \
                surface does not place `use` lines or connect modules for you.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "A path inside the project (default: the workspace \
                            root). The project is the nearest `xenith.toml` at or above it.",
                    },
                    "module": {
                        "type": "string",
                        "description": "Restrict to one module and its submodules, dotted \
                            (\"game.player\"). An unknown module is an error.",
                    },
                },
                "required": [],
            },
        }));
    }
    tools
}

/// Resolve a client-sent path against the workspace root and refuse anything
/// that lands outside it.
///
/// Both sides of the containment check are canonicalized — on Windows,
/// canonical paths carry the `\\?\` prefix, so comparing a canonical path
/// against an uncanonicalized root would refuse everything. Relative inputs
/// are joined to the root *as given* (not its canonical form) because a
/// verbatim `\\?\` base would take `..` components literally instead of
/// resolving them. Canonicalization also resolves symlinks, so a link inside
/// the root pointing outside is refused for where it leads, not where it sits.
fn confine(workspace_root: &Path, raw: &str) -> Result<PathBuf, String> {
    let canonical_root = workspace_root
        .canonicalize()
        .map_err(|e| format!("workspace root {}: {e}", workspace_root.display()))?;
    let candidate = Path::new(raw);
    let joined = if candidate.is_absolute() {
        candidate.to_path_buf()
    } else {
        workspace_root.join(candidate)
    };
    // A canonicalize failure is a file-not-found in client terms: the raw
    // path goes in the message, since that is the name the client used.
    let canonical = joined.canonicalize().map_err(|e| format!("{raw}: {e}"))?;
    if canonical.starts_with(&canonical_root) {
        Ok(canonical)
    } else {
        Err(format!("`{raw}` is outside the workspace root"))
    }
}

/// Replace `target` with `contents` atomically: write a sibling temp file,
/// then rename over the target. `std::fs::rename` replaces existing files on
/// the same volume, and the temp file lives in the target's own directory so
/// the rename never crosses one. The name carries pid and a counter so
/// concurrent servers — or concurrent calls, should the transport ever grow
/// them — cannot collide.
fn replace_file(target: &Path, contents: &str) -> std::io::Result<()> {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);

    let parent = target.parent().unwrap_or_else(|| Path::new("."));
    let stem = target
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "file".to_string());
    let temp = parent.join(format!(
        ".{stem}.{}.{}.fmt-tmp",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed),
    ));

    std::fs::write(&temp, contents)?;
    std::fs::rename(&temp, target).inspect_err(|_| {
        // Leave nothing behind on the failure path; the error to surface is
        // the rename's, not the cleanup's.
        let _ = std::fs::remove_file(&temp);
    })
}

/// The `mode` argument, defaulted and validated (design/0013 §1).
fn mode_of(arguments: &Value) -> Result<ModeRequest, String> {
    match arguments.get("mode") {
        None | Some(Value::Null) => Ok(ModeRequest::Auto),
        Some(Value::String(mode)) => match mode.as_str() {
            "auto" => Ok(ModeRequest::Auto),
            "project" => Ok(ModeRequest::Project),
            "single_file" => Ok(ModeRequest::SingleFile),
            other => Err(format!(
                "`mode` must be \"auto\", \"project\" or \"single_file\", not \"{other}\""
            )),
        },
        Some(_) => Err("`mode` must be a string".to_string()),
    }
}

/// Resolve a tool call's path and mode through the one shared pipeline
/// (design/0013 §1): discovery, containment and mode selection happen in
/// xenith-driver, and every failure — no manifest under `project` mode, a
/// containment escape, an unreadable file — surfaces as the tool error the
/// returned `Err` becomes. There is no silent single-file fallback here.
fn snapshot_of(
    arguments: &Value,
    workspace_root: &Path,
) -> Result<(String, ProjectSnapshot), String> {
    let raw = arguments
        .get("path")
        .and_then(Value::as_str)
        .ok_or_else(|| "`path` is required".to_string())?;
    let mode = mode_of(arguments)?;
    let request = ProjectRequest {
        path: Path::new(raw),
        mode,
        containment: Some(workspace_root),
    };
    let snapshot = xenith_driver::project::snapshot(&request).map_err(|e| e.to_string())?;
    Ok((raw.to_string(), snapshot))
}

/// A project whose layout is invalid is a discovery failure, not a working
/// project with extra diagnostics: refusing here is what keeps "the project
/// mode ran" an honest claim (design/0013 §1).
fn refuse_invalid_layout(project: &Project) -> Result<(), String> {
    if project.layout.is_empty() {
        return Ok(());
    }
    let mut text = format!(
        "the project at `{}` has an invalid layout:\n",
        project.root.display()
    );
    for (rel, diagnostic) in &project.layout {
        text.push_str(&format!(
            "  {rel}: {}: {}\n",
            diagnostic.code.id(),
            diagnostic.message
        ));
    }
    text.push_str("fix the layout, or pass mode \"single_file\" to analyze one file alone");
    Err(text)
}

/// The project root relative to the workspace root, forward slashes; "." for
/// the workspace root itself. Falls back to the absolute spelling when the
/// root cannot be expressed relative to the workspace (it then failed
/// containment anyway).
fn root_relative(workspace_root: &Path, root: &Path) -> String {
    let relative = workspace_root
        .canonicalize()
        .and_then(|workspace| root.canonicalize().map(|root| (workspace, root)))
        .ok()
        .and_then(|(workspace, root)| root.strip_prefix(&workspace).map(PathBuf::from).ok());
    match relative {
        Some(path) if path.as_os_str().is_empty() => ".".to_string(),
        Some(path) => path
            .components()
            .map(|c| c.as_os_str().to_string_lossy())
            .collect::<Vec<_>>()
            .join("/"),
        None => root.display().to_string(),
    }
}

/// The requested file's root-relative spelling, when it maps to a module.
fn requested_rel(project: &Project, requested: Option<usize>) -> Option<String> {
    requested.map(|index| format!("src/{}", project.files[index].rel))
}

/// Run one tool. `Ok` is the payload text (JSON for everything but `explain`);
/// `Err` is a human-readable failure the model can act on.
fn call_tool(name: &str, arguments: &Value, workspace_root: &Path) -> Result<String, String> {
    // The raw string is kept for payloads and messages — it is the name the
    // client knows the file by — while the canonical path does the I/O.
    let path_of = |arguments: &Value| -> Result<(String, PathBuf), String> {
        let raw = arguments
            .get("path")
            .and_then(Value::as_str)
            .ok_or_else(|| "`path` is required".to_string())?;
        let resolved = confine(workspace_root, raw)?;
        Ok((raw.to_string(), resolved))
    };
    let read = |raw: &str, resolved: &Path| -> Result<String, String> {
        std::fs::read_to_string(resolved).map_err(|e| format!("{raw}: {e}"))
    };
    let u32_of = |arguments: &Value, name: &str| -> Result<u32, String> {
        let wide = arguments
            .get(name)
            .and_then(Value::as_u64)
            .ok_or_else(|| format!("`{name}` is required and one-based"))?;
        u32::try_from(wide).map_err(|_| format!("`{name}` out of range"))
    };
    let pretty = |value: &Value| -> Result<String, String> {
        serde_json::to_string_pretty(value).map_err(|e| e.to_string())
    };

    match name {
        "check" => {
            let (path, snapshot) = snapshot_of(arguments, workspace_root)?;
            match snapshot {
                ProjectSnapshot::SingleFile { source, .. } => {
                    let analysis = xenith_driver::analyze_source(&source);
                    // The MCP surface has no teaching flag: a tool consumer
                    // always gets the taught shape and may ignore what it
                    // does not read.
                    let value = xenith_driver::wire::single_file_check(
                        &path,
                        &source,
                        &analysis.diagnostics,
                    );
                    pretty(&value)
                }
                ProjectSnapshot::Project { project, requested } => {
                    refuse_invalid_layout(&project)?;
                    let analyzed = xenith_driver::project::analyze(&project);
                    let reports: Vec<xenith_driver::wire::ProjectFileReport> = project
                        .files
                        .iter()
                        .zip(&analyzed.diagnostics)
                        .map(
                            |(file, diagnostics)| xenith_driver::wire::ProjectFileReport {
                                file: format!("src/{}", file.rel),
                                source: &file.source,
                                diagnostics,
                            },
                        )
                        .collect();
                    let value = xenith_driver::wire::project_check(
                        &root_relative(workspace_root, &project.root),
                        requested_rel(&project, requested).as_deref(),
                        &reports,
                    );
                    pretty(&value)
                }
            }
        }

        "goals" => {
            let (path, snapshot) = snapshot_of(arguments, workspace_root)?;
            match snapshot {
                ProjectSnapshot::SingleFile { source, .. } => {
                    let analysis = xenith_driver::analyze_source(&source);
                    let mut value = xenith_driver::wire::goals(&path, &source, &analysis.goals);
                    xenith_driver::wire::stamp_mode(&mut value, "single_file", None);
                    pretty(&value)
                }
                ProjectSnapshot::Project { project, requested } => {
                    refuse_invalid_layout(&project)?;
                    let analyzed = xenith_driver::project::analyze(&project);
                    let files: Vec<(String, &str, &[xenith_sema::Goal])> = project
                        .files
                        .iter()
                        .zip(&analyzed.goals)
                        .map(|(file, goals)| {
                            (
                                format!("src/{}", file.rel),
                                file.source.as_str(),
                                goals.as_slice(),
                            )
                        })
                        .collect();
                    let value = xenith_driver::wire::project_goals(
                        &root_relative(workspace_root, &project.root),
                        requested_rel(&project, requested).as_deref(),
                        &files,
                    );
                    pretty(&value)
                }
            }
        }

        "type_at" => {
            let (path, snapshot) = snapshot_of(arguments, workspace_root)?;
            let line = u32_of(arguments, "line")?;
            let column = u32_of(arguments, "column")?;
            match snapshot {
                ProjectSnapshot::SingleFile { source, .. } => {
                    let index = LineIndex::new(&source);
                    let offset =
                        xenith_driver::wire::position_to_offset(&source, &index, line, column)
                            .ok_or_else(|| format!("{path}:{line}:{column} is outside the file"))?;
                    let parsed = xenith_syntax::parse(&source);
                    let probe = xenith_sema::type_at(&parsed.module, offset).ok_or_else(|| {
                        format!(
                            "{path}:{line}:{column} is not inside an expression — try a position on a value"
                        )
                    })?;
                    let mut value = xenith_driver::wire::probe(&path, line, column, &probe);
                    xenith_driver::wire::stamp_mode(&mut value, "single_file", None);
                    pretty(&value)
                }
                ProjectSnapshot::Project { project, requested } => {
                    refuse_invalid_layout(&project)?;
                    let file = requested
                        .ok_or_else(|| format!("`{path}` is not a module source of the project"))?;
                    let source = &project.files[file].source;
                    let index = LineIndex::new(source);
                    let offset =
                        xenith_driver::wire::position_to_offset(source, &index, line, column)
                            .ok_or_else(|| format!("{path}:{line}:{column} is outside the file"))?;
                    let probe = xenith_driver::project::type_at(&project, file, offset)
                        .ok_or_else(|| {
                            format!(
                                "{path}:{line}:{column} is not inside an expression — try a position on a value"
                            )
                        })?;
                    let mut value = xenith_driver::wire::probe(&path, line, column, &probe);
                    xenith_driver::wire::stamp_mode(
                        &mut value,
                        "project",
                        Some(&root_relative(workspace_root, &project.root)),
                    );
                    pretty(&value)
                }
            }
        }

        "producers" => {
            let (path, snapshot) = snapshot_of(arguments, workspace_root)?;
            let type_text = arguments
                .get("type")
                .and_then(Value::as_str)
                .ok_or("`type` is required, spelled as in source")?;
            match snapshot {
                ProjectSnapshot::SingleFile { source, .. } => {
                    let parsed = xenith_syntax::parse(&source);
                    let found = xenith_sema::producers(&parsed.module, type_text)?;
                    let mut value = xenith_driver::wire::producers(&found);
                    xenith_driver::wire::stamp_mode(&mut value, "single_file", None);
                    pretty(&value)
                }
                ProjectSnapshot::Project { project, requested } => {
                    refuse_invalid_layout(&project)?;
                    let file = requested
                        .ok_or_else(|| format!("`{path}` is not a module source of the project"))?;
                    let found = xenith_driver::project::producers(&project, file, type_text)?;
                    let mut value = xenith_driver::wire::producers(&found);
                    xenith_driver::wire::stamp_mode(
                        &mut value,
                        "project",
                        Some(&root_relative(workspace_root, &project.root)),
                    );
                    pretty(&value)
                }
            }
        }

        "fmt" => {
            let (path, resolved) = path_of(arguments)?;
            let write = arguments
                .get("write")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let source = read(&path, &resolved)?;
            let formatted = xenith_syntax::format(&source).map_err(|e| e.to_string())?;
            let changed = formatted != source;
            if write && changed {
                replace_file(&resolved, &formatted).map_err(|e| format!("{path}: {e}"))?;
            }
            let value = if write {
                json!({ "changed": changed })
            } else {
                json!({ "changed": changed, "formatted": formatted })
            };
            serde_json::to_string_pretty(&value).map_err(|e| e.to_string())
        }

        "explain" => {
            let code = arguments
                .get("code")
                .and_then(Value::as_str)
                .ok_or("`code` is required, e.g. XN3005")?;
            let normalised = code.to_ascii_uppercase();
            match DiagCode::from_id(&normalised) {
                Some(found) => Ok(format!("{}\n\n{}", found.id(), found.explain())),
                None => Err(format!(
                    "unknown diagnostic code `{code}`; codes run XN0001–XN7008"
                )),
            }
        }

        "run" => {
            let (path, snapshot) = snapshot_of(arguments, workspace_root)?;
            match snapshot {
                ProjectSnapshot::SingleFile { source, .. } => {
                    let analysis = xenith_driver::analyze_source(&source);
                    if !analysis.diagnostics.is_empty() {
                        let value = json!({
                            "analysis_mode": "single_file",
                            "exit_code": 2,
                            "stdout": "",
                            "error": "the file has diagnostics; call `check` and fix them first",
                        });
                        return pretty(&value);
                    }
                    let parsed = xenith_syntax::parse(&source);
                    let (table, _) = xenith_sema::def::collect(&parsed.module);
                    let outcome = xenith_vm::run(&parsed.module, &table);
                    let index = LineIndex::new(&source);
                    let error = outcome.error.map(|(message, span)| {
                        let at = index.line_col(&source, span.start);
                        format!("{path}:{}:{}: {message}", at.line, at.column)
                    });
                    let value = json!({
                        "analysis_mode": "single_file",
                        "exit_code": outcome.exit,
                        "stdout": String::from_utf8_lossy(&outcome.stdout),
                        "error": error,
                    });
                    pretty(&value)
                }
                ProjectSnapshot::Project { project, .. } => {
                    refuse_invalid_layout(&project)?;
                    let root = root_relative(workspace_root, &project.root);
                    let analyzed = xenith_driver::project::analyze(&project);
                    if analyzed.diagnostics.iter().any(|file| !file.is_empty()) {
                        let value = json!({
                            "analysis_mode": "project",
                            "project_root": root,
                            "exit_code": 2,
                            "stdout": "",
                            "error": "the project has diagnostics; call `check` and fix them first",
                        });
                        return pretty(&value);
                    }
                    let modules: Vec<(String, &xenith_syntax::ast::Module)> = project
                        .files
                        .iter()
                        .map(|file| (file.module.clone(), &file.parsed.module))
                        .collect();
                    let outcome = xenith_vm::run_project(&modules, &analyzed.table);
                    // The trap span cannot name its file across modules, so
                    // project errors carry the message alone — same as the
                    // CLI's project rendering.
                    let error = outcome.error.map(|(message, _)| message);
                    let value = json!({
                        "analysis_mode": "project",
                        "project_root": root,
                        "exit_code": outcome.exit,
                        "stdout": String::from_utf8_lossy(&outcome.stdout),
                        "error": error,
                    });
                    pretty(&value)
                }
            }
        }

        "api_surface" => {
            let raw = arguments.get("path").and_then(Value::as_str).unwrap_or(".");
            let resolved = confine(workspace_root, raw)?;
            let project = xenith_driver::project::project_at(&resolved, Some(workspace_root))
                .map_err(|e| e.to_string())?;
            let surface = xenith_driver::api::surface(&project)?;
            let scoped = match arguments.get("module").and_then(Value::as_str) {
                Some(module) => surface.scoped(module).ok_or_else(|| {
                    format!(
                        "no module `{module}` in the project at `{}` — modules: {}",
                        project.root.display(),
                        surface
                            .modules
                            .iter()
                            .map(|m| m.path.as_str())
                            .collect::<Vec<_>>()
                            .join(", ")
                    )
                })?,
                None => surface,
            };
            let mut value = xenith_driver::api::render_json(&scoped);
            if let Some(map) = value.as_object_mut() {
                map.insert(
                    "project_root".into(),
                    json!(root_relative(workspace_root, &project.root)),
                );
            }
            pretty(&value)
        }

        _ => unreachable!("tool names are validated by the caller"),
    }
}
