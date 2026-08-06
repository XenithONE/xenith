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
pub fn discover(start: &Path) -> Option<PathBuf> {
    let origin = if start.is_dir() {
        start.to_path_buf()
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

/// Check every module: parse diagnostics and semantic diagnostics merged
/// per file, plus the table the interpreter runs against.
pub fn analyze(project: &Project) -> (Vec<Vec<Diagnostic>>, xenith_sema::DefTable) {
    let units: Vec<xenith_sema::ModuleUnit> = project
        .files
        .iter()
        .map(|file| xenith_sema::ModuleUnit {
            path: file.module.clone(),
            module: &file.parsed.module,
        })
        .collect();
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
    (merged, analysis.table)
}
