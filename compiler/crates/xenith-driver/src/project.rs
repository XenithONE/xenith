//! Project discovery and loading — the file system's half of design/0010 §2.
//!
//! `xenith.toml` marks a root and nothing more; its contents are not read,
//! and `name` stays reserved for a future package identity. Sources live
//! under `src/`, and only there: an `.xn` file anywhere else is not a
//! module candidate. The mapping from paths to module ids is a pure
//! function over relative paths, because the rules it enforces — case
//! collisions above all — concern layouts some file systems cannot even
//! represent, and a rule that cannot be tested is a rule that rots.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use xenith_diag::{DiagCode, Diagnostic, Span};

/// One loaded module file.
pub struct ProjectFile {
    /// Dotted module path ("main", "game.player").
    pub module: String,
    /// Path relative to `src/`, forward slashes.
    pub rel: String,
    pub source: String,
    pub parsed: xenith_syntax::Parsed,
}

/// A loaded project: modules sorted by path, plus layout problems that
/// belong to files rather than spans.
pub struct Project {
    pub root: PathBuf,
    pub files: Vec<ProjectFile>,
    /// (path relative to the root, diagnostic) — spans are empty; these are
    /// about what a file *is*, not what it says.
    pub layout: Vec<(String, Diagnostic)>,
}

/// Walk up from `start` to the nearest directory holding `xenith.toml`.
///
/// `start` is made absolute against the working directory first, and the
/// returned root is therefore absolute too. A relative path runs out of
/// parents at its own first component — `src/main.xn` has no ancestor above
/// `src` — so walking it directly ends on the *empty* path. The empty path
/// resolves `xenith.toml` against the working directory, so a manifest right
/// there was "found" as root `""`, which no later canonicalization accepts:
/// `in_sources` returned false and the run silently degraded to single-file
/// mode. Absolutizing once at this boundary makes relative and absolute
/// invocation discover the same root (design/0010 §2).
pub fn discover(start: &Path) -> Option<PathBuf> {
    let start = std::path::absolute(start).ok()?;
    let origin = if start.is_dir() {
        start.clone()
    } else {
        start.parent()?.to_path_buf()
    };
    let mut dir = Some(origin.as_path());
    while let Some(current) = dir {
        if current.join("xenith.toml").is_file() {
            return Some(current.to_path_buf());
        }
        dir = current.parent();
    }
    None
}

/// Whether `path` is inside the project's source set — the gate for project
/// mode. A stray file next to the manifest stays a single-file run.
pub fn in_sources(root: &Path, path: &Path) -> bool {
    let Ok(canonical_root) = root.canonicalize() else {
        return false;
    };
    if let Ok(canonical) = path.canonicalize() {
        return canonical.starts_with(canonical_root.join("src"))
            || canonical == canonical_root.join("xenith.toml");
    }
    false
}

/// `lower_snake` identifier: what a module path segment may be.
fn is_module_segment(segment: &str) -> bool {
    let mut chars = segment.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    (first.is_ascii_lowercase() || first == '_')
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

/// (module path, `src/`-relative file) pairs the layout rules accepted.
pub type IndexedModules = Vec<(String, String)>;

/// Layout problems, attributed to the relative path that earned them.
pub type LayoutProblems = Vec<(String, Diagnostic)>;

/// Map `src/`-relative `.xn` paths to module paths, applying the layout
/// rules of design/0010 §2. Pure over path strings so every rule has a
/// test, including the ones a case-insensitive disk cannot reproduce.
pub fn index_sources(rel_paths: &[String]) -> (IndexedModules, LayoutProblems) {
    let mut sorted: Vec<&String> = rel_paths.iter().collect();
    sorted.sort();

    let mut modules: Vec<(String, String)> = Vec::new();
    let mut problems: Vec<(String, Diagnostic)> = Vec::new();
    // Case-folded module path -> first file that claimed it. Checked before
    // segment validation so `Game.xn` vs `game.xn` reports the collision,
    // not just the capital letter.
    let mut folded: HashMap<String, String> = HashMap::new();

    for rel in sorted {
        let Some(stem) = rel.strip_suffix(".xn") else {
            continue;
        };
        let segments: Vec<&str> = stem.split('/').collect();

        let fold = stem.to_lowercase();
        if let Some(first) = folded.get(&fold) {
            problems.push((
                rel.clone(),
                Diagnostic::error(
                    DiagCode::ModuleCaseCollision,
                    Span::EMPTY,
                    format!(
                        "`{rel}` and `{first}` differ only by letter case; \
                         some hosts cannot tell them apart"
                    ),
                ),
            ));
            continue;
        }
        folded.insert(fold, rel.clone());

        if let Some(bad) = segments.iter().find(|s| !is_module_segment(s)) {
            problems.push((
                rel.clone(),
                Diagnostic::error(
                    DiagCode::InvalidModulePath,
                    Span::EMPTY,
                    format!(
                        "`{rel}` cannot name a module: `{bad}` is not a \
                         lower_snake identifier"
                    ),
                ),
            ));
            continue;
        }

        let module = segments.join(".");
        if module == "std" || module.starts_with("std.") {
            problems.push((
                rel.clone(),
                Diagnostic::error(
                    DiagCode::ReservedModuleRoot,
                    Span::EMPTY,
                    format!("`{rel}` claims the reserved module root `std`"),
                ),
            ));
            continue;
        }

        modules.push((module, rel.clone()));
    }

    (modules, problems)
}

/// Load and parse every module under `root/src`.
pub fn load(root: &Path) -> Result<Project, String> {
    let src = root.join("src");
    let mut rels: Vec<String> = Vec::new();
    let mut layout: Vec<(String, Diagnostic)> = Vec::new();
    if src.is_dir() {
        walk(&src, &src, &mut rels, &mut layout)?;
    }

    let (mapped, mut problems) = index_sources(&rels);
    layout.append(&mut problems);
    // Attribute layout problems by their `src/`-relative spelling.
    for (rel, _) in &mut layout {
        *rel = format!("src/{rel}");
    }

    let mut files = Vec::new();
    for (module, rel) in mapped {
        let mut full = src.clone();
        for part in rel.split('/') {
            full.push(part);
        }
        let source =
            std::fs::read_to_string(&full).map_err(|e| format!("{}: {e}", full.display()))?;
        let parsed = xenith_syntax::parse(&source);
        files.push(ProjectFile {
            module,
            rel,
            source,
            parsed,
        });
    }
    files.sort_by(|a, b| a.module.cmp(&b.module));

    Ok(Project {
        root: root.to_path_buf(),
        files,
        layout,
    })
}

fn walk(
    dir: &Path,
    src_root: &Path,
    rels: &mut Vec<String>,
    layout: &mut Vec<(String, Diagnostic)>,
) -> Result<(), String> {
    let mut entries: Vec<PathBuf> = std::fs::read_dir(dir)
        .map_err(|e| format!("{}: {e}", dir.display()))?
        .filter_map(|entry| entry.ok().map(|e| e.path()))
        .collect();
    entries.sort();

    for path in entries {
        let rel = path
            .strip_prefix(src_root)
            .unwrap_or(&path)
            .components()
            .map(|c| c.as_os_str().to_string_lossy())
            .collect::<Vec<_>>()
            .join("/");
        let file_type = std::fs::symlink_metadata(&path)
            .map_err(|e| format!("{}: {e}", path.display()))?
            .file_type();
        if file_type.is_symlink() {
            // A module has exactly one path (design/0010 §2).
            layout.push((
                rel,
                Diagnostic::error(
                    DiagCode::InvalidModulePath,
                    Span::EMPTY,
                    "symbolic links are not module sources".to_string(),
                ),
            ));
            continue;
        }
        if file_type.is_dir() {
            walk(&path, src_root, rels, layout)?;
            continue;
        }
        let name = path.file_name().map(|n| n.to_string_lossy().to_string());
        match name.as_deref() {
            Some("xenith.toml") => layout.push((
                rel,
                Diagnostic::error(
                    DiagCode::NestedManifest,
                    Span::EMPTY,
                    "a manifest inside `src/` would nest one project in another".to_string(),
                ),
            )),
            Some(n) if n.ends_with(".xn") => rels.push(rel),
            _ => {}
        }
    }
    Ok(())
}

/// Everything project analysis yields, per file in `Project::files` order.
pub struct Analyzed {
    /// Parse and semantic diagnostics merged per file, in source order.
    pub diagnostics: Vec<Vec<Diagnostic>>,
    /// Hole goals per file, in source order.
    pub goals: Vec<Vec<xenith_sema::Goal>>,
    /// The table the interpreter runs against.
    pub table: xenith_sema::DefTable,
}

fn units_of(project: &Project) -> Vec<xenith_sema::ModuleUnit<'_>> {
    project
        .files
        .iter()
        .map(|file| xenith_sema::ModuleUnit {
            path: file.module.clone(),
            module: &file.parsed.module,
        })
        .collect()
}

/// Check every module: parse diagnostics and semantic diagnostics merged
/// per file, plus goals and the interpreter's table.
pub fn analyze(project: &Project) -> Analyzed {
    let units = units_of(project);
    let analysis = xenith_sema::analyze_project(&units);
    let merged = project
        .files
        .iter()
        .zip(analysis.diagnostics)
        .map(|(file, mut sema)| {
            let mut all = file.parsed.diagnostics.clone();
            all.append(&mut sema);
            all.sort_by_key(|d| d.span.start);
            all
        })
        .collect();
    Analyzed {
        diagnostics: merged,
        goals: analysis.goals,
        table: analysis.table,
    }
}

/// `type_at` for one project file, answered with the whole project in view.
/// `file` indexes `Project::files`.
pub fn type_at(project: &Project, file: usize, offset: u32) -> Option<xenith_sema::Probe> {
    xenith_sema::project_type_at(&units_of(project), file, offset)
}

/// `producers` for one project file, scoped the way the file itself is:
/// its own items, its `use`d modules' pub items, and the prelude.
pub fn producers(
    project: &Project,
    file: usize,
    type_text: &str,
) -> Result<Vec<xenith_sema::Producer>, String> {
    xenith_sema::project_producers(&units_of(project), file, type_text)
}

// ------------------------------------------------------------------ requests
//
// The one pipeline both frontends walk (design/0013 §1): a request names a
// path, a mode, and — for confined servers — a boundary; the snapshot is
// what actually resolved. Discovery, containment and mode selection happen
// here and nowhere else, so the CLI and the MCP server cannot drift into
// two truths about what a path means.

/// How a caller asks for a path to be treated.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ModeRequest {
    /// Project when a manifest governs the path, single-file otherwise —
    /// and never single-file *because something went wrong*: failures of
    /// discovery are errors, not fallbacks.
    Auto,
    /// Demand project mode; fail loudly when discovery cannot deliver it.
    Project,
    /// Analyze the one file alone, even inside a project.
    SingleFile,
}

/// One analysis request: the path as the caller spelled it, the mode, and
/// the boundary every read must stay inside (`None` for the unconfined CLI).
pub struct ProjectRequest<'a> {
    pub path: &'a Path,
    pub mode: ModeRequest,
    pub containment: Option<&'a Path>,
}

/// What a request resolved to.
pub enum ProjectSnapshot {
    /// No manifest governs the path, or single-file mode was demanded.
    SingleFile {
        /// The path as the caller gave it — the name it knows the file by.
        path: PathBuf,
        source: String,
    },
    /// A manifest was discovered and the path is in its source set.
    Project {
        project: Project,
        /// Index into `project.files` of the requested file, when it maps
        /// to a module source (the manifest itself does not).
        requested: Option<usize>,
    },
}

impl ProjectSnapshot {
    /// The honest label a response carries (design/0013 §1): what actually
    /// ran, as distinct from what `features` says the tool *could* run.
    pub fn analysis_mode(&self) -> &'static str {
        match self {
            ProjectSnapshot::SingleFile { .. } => "single_file",
            ProjectSnapshot::Project { .. } => "project",
        }
    }
}

/// Why a request produced no snapshot. Every variant renders the message a
/// surface shows; none of them may quietly become a single-file run.
#[derive(Debug)]
pub enum SnapshotError {
    /// Project mode was demanded and no manifest sits above the path.
    NoManifest(String),
    /// Project mode was demanded and the path is outside the source set.
    OutsideSources(String),
    /// A read the request implies escapes the containment boundary.
    Containment(String),
    /// Reading or loading failed; the message names the path as given.
    Io(String),
}

impl std::fmt::Display for SnapshotError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SnapshotError::NoManifest(message)
            | SnapshotError::OutsideSources(message)
            | SnapshotError::Containment(message)
            | SnapshotError::Io(message) => f.write_str(message),
        }
    }
}

impl std::error::Error for SnapshotError {}

/// Resolve one request: discovery, containment and mode selection, in this
/// one place (design/0013 §1).
///
/// With a containment boundary, a relative path resolves against the
/// boundary and the entry must canonicalize inside it — exactly the confine
/// rule the MCP server has always applied. Without one, the path is read as
/// given, which keeps the CLI's error text stable.
pub fn snapshot(request: &ProjectRequest) -> Result<ProjectSnapshot, SnapshotError> {
    let given = request.path.to_path_buf();

    // The entry itself, against the boundary.
    let (read_path, canonical_boundary) = match request.containment {
        Some(boundary) => {
            let canonical_boundary = boundary.canonicalize().map_err(|e| {
                SnapshotError::Io(format!("workspace root {}: {e}", boundary.display()))
            })?;
            let joined = if request.path.is_absolute() {
                given.clone()
            } else {
                // Joined to the boundary *as given*: a verbatim `\\?\` base
                // would take `..` literally instead of resolving it.
                boundary.join(&given)
            };
            let canonical = joined
                .canonicalize()
                .map_err(|e| SnapshotError::Io(format!("{}: {e}", given.display())))?;
            if !canonical.starts_with(&canonical_boundary) {
                return Err(SnapshotError::Containment(format!(
                    "`{}` is outside the workspace root",
                    given.display()
                )));
            }
            (canonical, Some(canonical_boundary))
        }
        None => (given.clone(), None),
    };

    let single_file = || -> Result<ProjectSnapshot, SnapshotError> {
        let source = std::fs::read_to_string(&read_path)
            .map_err(|e| SnapshotError::Io(format!("{}: {e}", given.display())))?;
        Ok(ProjectSnapshot::SingleFile {
            path: given.clone(),
            source,
        })
    };

    if request.mode == ModeRequest::SingleFile {
        return single_file();
    }

    let Some(root) = discover(&read_path) else {
        return match request.mode {
            ModeRequest::Project => Err(SnapshotError::NoManifest(format!(
                "no `xenith.toml` found above `{}` — project mode needs a manifest; \
                 pass mode \"single_file\" for a standalone file",
                given.display()
            ))),
            _ => single_file(),
        };
    };
    if !in_sources(&root, &read_path) {
        // A stray file next to a manifest is not part of the project
        // (design/0010 §2) — that is a fact about the layout, not a failure
        // of discovery, so `auto` reads it as the single file it is.
        return match request.mode {
            ModeRequest::Project => Err(SnapshotError::OutsideSources(format!(
                "`{}` is not in the project's source set — sources live under `{}` — \
                 pass mode \"single_file\" to analyze it alone",
                given.display(),
                root.join("src").display()
            ))),
            _ => single_file(),
        };
    }

    let project = load_confined(&root, canonical_boundary.as_deref())?;

    let canonical_entry = read_path.canonicalize().ok();
    let requested = canonical_entry.and_then(|entry| {
        project.files.iter().position(|file| {
            source_path(&project.root, &file.rel)
                .canonicalize()
                .is_ok_and(|canonical| canonical == entry)
        })
    });

    Ok(ProjectSnapshot::Project { project, requested })
}

/// The project governing `path`: discovery plus a confined load. Unlike
/// [`snapshot`], the path may be the project root itself — this is how
/// `xenith api <project>` and the MCP `api_surface` tool name a project.
pub fn project_at(path: &Path, containment: Option<&Path>) -> Result<Project, SnapshotError> {
    let Some(root) = discover(path) else {
        return Err(SnapshotError::NoManifest(format!(
            "no `xenith.toml` found above `{}`",
            path.display()
        )));
    };
    let canonical_boundary = match containment {
        Some(boundary) => Some(boundary.canonicalize().map_err(|e| {
            SnapshotError::Io(format!("workspace root {}: {e}", boundary.display()))
        })?),
        None => None,
    };
    load_confined(&root, canonical_boundary.as_deref())
}

/// Load a project, checking every file the load reads — root, manifest,
/// every module source — against the boundary after canonicalization
/// (design/0013 §1: the whole transitive read set, not just the entry).
fn load_confined(root: &Path, boundary: Option<&Path>) -> Result<Project, SnapshotError> {
    if let Some(boundary) = boundary {
        let canonical_root = root
            .canonicalize()
            .map_err(|e| SnapshotError::Io(format!("{}: {e}", root.display())))?;
        if !canonical_root.starts_with(boundary) {
            return Err(SnapshotError::Containment(format!(
                "the project at `{}` is outside the workspace root; \
                 refusing to read past the boundary",
                root.display()
            )));
        }
    }

    let project = load(root).map_err(SnapshotError::Io)?;

    if let Some(boundary) = boundary {
        let mut reads: Vec<(String, PathBuf)> =
            vec![("xenith.toml".to_string(), project.root.join("xenith.toml"))];
        for file in &project.files {
            reads.push((
                format!("src/{}", file.rel),
                source_path(&project.root, &file.rel),
            ));
        }
        for (shown, path) in reads {
            let canonical = path
                .canonicalize()
                .map_err(|e| SnapshotError::Io(format!("{shown}: {e}")))?;
            if !canonical.starts_with(boundary) {
                return Err(SnapshotError::Containment(format!(
                    "`{shown}` is outside the workspace root; the project at `{}` \
                     reads it",
                    project.root.display()
                )));
            }
        }
    }

    Ok(project)
}

/// `root/src/<rel>`, split on the forward slashes `rel` is stored with.
fn source_path(root: &Path, rel: &str) -> PathBuf {
    let mut full = root.join("src");
    for part in rel.split('/') {
        full.push(part);
    }
    full
}
