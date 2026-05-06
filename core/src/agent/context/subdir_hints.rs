//! Detect framework / language / build-system markers in a
//! directory tree.
//!
//! The agent prepends these hints to the system prompt so that the
//! model knows whether it's looking at a Rust crate, a Next.js
//! site, a poetry project, etc., before the first user turn.
//!
//! Two layers:
//!
//!   * [`MARKERS`] — pure-data table mapping a marker file/dir to a
//!     human-readable hint. No IO.
//!   * [`scan_dir`] / [`scan_dir_recursive`] — IO entrypoints that
//!     stat each marker and return the matches.
//!
//! The recursive variant walks the cwd and a configurable depth of
//! immediate subdirectories (default 2). It does **not** descend
//! into well-known noisy directories (`.git`, `node_modules`,
//! `target`, `__pycache__`, `.venv`, `venv`, `dist`, `build`,
//! `.next`, `out`).
//!
//! Hits are returned in stable alphabetical order keyed on the
//! relative path under the cwd so callers can render them
//! deterministically (and so tests don't depend on filesystem
//! ordering).

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

/// What kind of marker we matched. Lets callers group / colour /
/// render hits by semantic category.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum HintKind {
    /// Build / package manifest (Cargo.toml, package.json, ...).
    Manifest,
    /// VCS marker (.git, .hg, ...).
    Vcs,
    /// CI / build automation (Dockerfile, .github/workflows, ...).
    Ci,
    /// Framework-specific config (next.config.js, vite.config.ts, ...).
    Framework,
    /// IDE / editor metadata (.vscode, .idea, .editorconfig, ...).
    Editor,
    /// Runtime config (.env*, etc.).
    Env,
}

/// Static marker definition. Matches against a filename (or
/// directory name) inside the scanned directory.
#[derive(Debug, Clone, Copy)]
pub struct Marker {
    /// Exact basename to match (case-sensitive on case-sensitive
    /// filesystems; we always compare verbatim).
    pub name: &'static str,
    /// True if the marker is expected to be a directory.
    pub is_dir: bool,
    pub kind: HintKind,
    /// Short label embedded in the rendered hint
    /// (e.g. "Rust crate", "Next.js app").
    pub label: &'static str,
}

/// Marker table. New entries should be inserted in alphabetical
/// order by `name` for searchability — callers do not depend on
/// declaration order (we re-sort by relative path).
pub const MARKERS: &[Marker] = &[
    // --- VCS ---------------------------------------------------------
    Marker { name: ".git",        is_dir: true,  kind: HintKind::Vcs,       label: "Git repository" },
    Marker { name: ".hg",         is_dir: true,  kind: HintKind::Vcs,       label: "Mercurial repository" },
    Marker { name: ".svn",        is_dir: true,  kind: HintKind::Vcs,       label: "Subversion checkout" },
    Marker { name: ".jj",         is_dir: true,  kind: HintKind::Vcs,       label: "Jujutsu repository" },
    // --- Editor ------------------------------------------------------
    Marker { name: ".editorconfig", is_dir: false, kind: HintKind::Editor, label: "EditorConfig" },
    Marker { name: ".idea",       is_dir: true,  kind: HintKind::Editor,   label: "IntelliJ project" },
    Marker { name: ".vscode",     is_dir: true,  kind: HintKind::Editor,   label: "VS Code workspace" },
    // --- CI / build automation --------------------------------------
    Marker { name: ".github",     is_dir: true,  kind: HintKind::Ci,       label: "GitHub config (likely Actions)" },
    Marker { name: ".gitlab-ci.yml", is_dir: false, kind: HintKind::Ci,    label: "GitLab CI" },
    Marker { name: "Dockerfile",  is_dir: false, kind: HintKind::Ci,       label: "Docker image" },
    Marker { name: "docker-compose.yml", is_dir: false, kind: HintKind::Ci, label: "Docker Compose stack" },
    Marker { name: "docker-compose.yaml", is_dir: false, kind: HintKind::Ci, label: "Docker Compose stack" },
    Marker { name: "Makefile",    is_dir: false, kind: HintKind::Ci,       label: "Makefile" },
    Marker { name: "Justfile",    is_dir: false, kind: HintKind::Ci,       label: "Just task runner" },
    Marker { name: "Taskfile.yml", is_dir: false, kind: HintKind::Ci,      label: "Task runner" },
    // --- Manifests (build / package) --------------------------------
    Marker { name: "Cargo.toml",  is_dir: false, kind: HintKind::Manifest, label: "Rust crate" },
    Marker { name: "Gemfile",     is_dir: false, kind: HintKind::Manifest, label: "Ruby project" },
    Marker { name: "Package.swift", is_dir: false, kind: HintKind::Manifest, label: "Swift package" },
    Marker { name: "Pipfile",     is_dir: false, kind: HintKind::Manifest, label: "Pipenv project" },
    Marker { name: "Project.toml", is_dir: false, kind: HintKind::Manifest, label: "Julia project" },
    Marker { name: "build.gradle", is_dir: false, kind: HintKind::Manifest, label: "Gradle project" },
    Marker { name: "build.gradle.kts", is_dir: false, kind: HintKind::Manifest, label: "Gradle (Kotlin DSL) project" },
    Marker { name: "composer.json", is_dir: false, kind: HintKind::Manifest, label: "PHP Composer project" },
    Marker { name: "go.mod",      is_dir: false, kind: HintKind::Manifest, label: "Go module" },
    Marker { name: "mix.exs",     is_dir: false, kind: HintKind::Manifest, label: "Elixir project" },
    Marker { name: "package.json", is_dir: false, kind: HintKind::Manifest, label: "Node.js project" },
    Marker { name: "pom.xml",     is_dir: false, kind: HintKind::Manifest, label: "Maven project" },
    Marker { name: "pyproject.toml", is_dir: false, kind: HintKind::Manifest, label: "Python project" },
    Marker { name: "requirements.txt", is_dir: false, kind: HintKind::Manifest, label: "Python requirements" },
    Marker { name: "shard.yml",   is_dir: false, kind: HintKind::Manifest, label: "Crystal project" },
    Marker { name: "stack.yaml",  is_dir: false, kind: HintKind::Manifest, label: "Haskell stack" },
    // --- Frameworks --------------------------------------------------
    Marker { name: "next.config.js", is_dir: false, kind: HintKind::Framework, label: "Next.js app" },
    Marker { name: "next.config.mjs", is_dir: false, kind: HintKind::Framework, label: "Next.js app" },
    Marker { name: "next.config.ts", is_dir: false, kind: HintKind::Framework, label: "Next.js app" },
    Marker { name: "nuxt.config.ts", is_dir: false, kind: HintKind::Framework, label: "Nuxt app" },
    Marker { name: "vite.config.js", is_dir: false, kind: HintKind::Framework, label: "Vite app" },
    Marker { name: "vite.config.ts", is_dir: false, kind: HintKind::Framework, label: "Vite app" },
    Marker { name: "remix.config.js", is_dir: false, kind: HintKind::Framework, label: "Remix app" },
    Marker { name: "astro.config.mjs", is_dir: false, kind: HintKind::Framework, label: "Astro site" },
    Marker { name: "svelte.config.js", is_dir: false, kind: HintKind::Framework, label: "SvelteKit app" },
    Marker { name: "tailwind.config.js", is_dir: false, kind: HintKind::Framework, label: "Tailwind CSS" },
    Marker { name: "tailwind.config.ts", is_dir: false, kind: HintKind::Framework, label: "Tailwind CSS" },
    Marker { name: "manage.py", is_dir: false, kind: HintKind::Framework, label: "Django project" },
    Marker { name: "Rakefile", is_dir: false, kind: HintKind::Framework, label: "Ruby Rake (likely Rails)" },
    Marker { name: "tsconfig.json", is_dir: false, kind: HintKind::Framework, label: "TypeScript project" },
    Marker { name: "deno.json", is_dir: false, kind: HintKind::Framework, label: "Deno project" },
    Marker { name: "deno.jsonc", is_dir: false, kind: HintKind::Framework, label: "Deno project" },
    Marker { name: "bun.lockb", is_dir: false, kind: HintKind::Framework, label: "Bun project" },
    Marker { name: "pnpm-workspace.yaml", is_dir: false, kind: HintKind::Framework, label: "pnpm workspace" },
    // --- Env / runtime ----------------------------------------------
    Marker { name: ".env",        is_dir: false, kind: HintKind::Env,      label: "dotenv defaults" },
    Marker { name: ".env.local",  is_dir: false, kind: HintKind::Env,      label: "dotenv overrides" },
    Marker { name: ".nvmrc",      is_dir: false, kind: HintKind::Env,      label: "Node version pin" },
    Marker { name: ".python-version", is_dir: false, kind: HintKind::Env,  label: "Python version pin" },
    Marker { name: ".tool-versions", is_dir: false, kind: HintKind::Env,   label: "asdf tool versions" },
    Marker { name: ".rust-toolchain", is_dir: false, kind: HintKind::Env,  label: "Rust toolchain pin" },
    Marker { name: ".rust-toolchain.toml", is_dir: false, kind: HintKind::Env, label: "Rust toolchain pin" },
];

/// Directory names we never descend into during recursive scans.
pub const NOISE_DIRS: &[&str] = &[
    ".git",
    ".hg",
    ".jj",
    ".svn",
    ".next",
    ".nuxt",
    ".turbo",
    ".cache",
    "__pycache__",
    "node_modules",
    "target",
    "dist",
    "build",
    "out",
    ".venv",
    "venv",
    "vendor",
];

/// One match. `rel` is relative to the originally-scanned root,
/// using forward slashes regardless of platform so JSON output is
/// stable across OSes.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Hint {
    pub rel: String,
    pub kind: HintKind,
    pub label: &'static str,
    pub is_dir: bool,
}

/// Scan a single directory (no recursion) for known markers.
///
/// Returns hits sorted by `rel` for determinism.
pub fn scan_dir(root: &Path) -> Vec<Hint> {
    let mut by_rel: BTreeMap<String, Hint> = BTreeMap::new();
    scan_one(root, root, &mut by_rel);
    by_rel.into_values().collect()
}

/// Scan recursively up to `max_depth` levels of immediate
/// subdirectories. `max_depth = 0` is equivalent to [`scan_dir`].
///
/// Returns hits sorted by `rel`. NOISE_DIRS are skipped.
pub fn scan_dir_recursive(root: &Path, max_depth: usize) -> Vec<Hint> {
    let mut by_rel: BTreeMap<String, Hint> = BTreeMap::new();
    walk(root, root, 0, max_depth, &mut by_rel);
    by_rel.into_values().collect()
}

fn walk(
    base: &Path,
    cur: &Path,
    depth: usize,
    max_depth: usize,
    out: &mut BTreeMap<String, Hint>,
) {
    scan_one(base, cur, out);
    if depth >= max_depth {
        return;
    }
    let entries = match fs::read_dir(cur) {
        Ok(e) => e,
        Err(_) => return,
    };
    let mut subs: Vec<PathBuf> = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(meta) = entry.file_type() else {
            continue;
        };
        if !meta.is_dir() {
            continue;
        }
        let name = match path.file_name().and_then(|n| n.to_str()) {
            Some(n) => n,
            None => continue,
        };
        if NOISE_DIRS.contains(&name) {
            continue;
        }
        subs.push(path);
    }
    // Sort so traversal is deterministic regardless of OS readdir order.
    subs.sort();
    for sub in subs {
        walk(base, &sub, depth + 1, max_depth, out);
    }
}

fn scan_one(base: &Path, cur: &Path, out: &mut BTreeMap<String, Hint>) {
    for marker in MARKERS {
        let candidate = cur.join(marker.name);
        let exists_kind = match fs::metadata(&candidate) {
            Ok(m) => Some(m.is_dir()),
            Err(_) => None,
        };
        let Some(is_dir_actual) = exists_kind else {
            continue;
        };
        if is_dir_actual != marker.is_dir {
            // Marker's expected file/dir kind doesn't match what's
            // on disk — skip (e.g. someone created a `.git` file
            // instead of a directory).
            continue;
        }
        let rel = relative_to(base, &candidate).unwrap_or_else(|| marker.name.to_string());
        out.insert(
            rel.clone(),
            Hint {
                rel,
                kind: marker.kind,
                label: marker.label,
                is_dir: marker.is_dir,
            },
        );
    }
}

fn relative_to(base: &Path, full: &Path) -> Option<String> {
    let rel = full.strip_prefix(base).ok()?;
    let s = rel
        .components()
        .filter_map(|c| c.as_os_str().to_str())
        .collect::<Vec<_>>()
        .join("/");
    if s.is_empty() { None } else { Some(s) }
}

/// Render a list of hints as a single short paragraph suitable for
/// embedding in a system prompt. Empty input → empty string.
pub fn render_summary(hints: &[Hint]) -> String {
    if hints.is_empty() {
        return String::new();
    }
    // Group by label so "Rust crate" hits in workspace + member get
    // collapsed to a single line.
    let mut by_label: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for h in hints {
        by_label.entry(h.label).or_default().push(h.rel.as_str());
    }
    let mut lines = Vec::new();
    for (label, rels) in by_label {
        if rels.len() == 1 {
            lines.push(format!("- {label} ({})", rels[0]));
        } else {
            lines.push(format!("- {label} ({} locations)", rels.len()));
        }
    }
    format!("Project hints:\n{}", lines.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::io::Write;

    fn tmp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "cos-subdir-hints-{}-{}",
            tag,
            uuid::Uuid::new_v4().simple()
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn touch(p: &Path) {
        if let Some(parent) = p.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        let mut f = fs::File::create(p).unwrap();
        writeln!(f, "{{}}").unwrap();
    }

    #[test]
    fn marker_table_is_non_empty_and_alphabetised_within_kinds() {
        // Sanity: enough markers to be useful.
        assert!(MARKERS.len() >= 30);
        // Manifest entries should be sorted alphabetically (the table
        // claims this in its doc-comment for searchability).
        let manifests: Vec<&str> = MARKERS
            .iter()
            .filter(|m| m.kind == HintKind::Manifest)
            .map(|m| m.name)
            .collect();
        let mut sorted = manifests.clone();
        sorted.sort();
        assert_eq!(manifests, sorted, "Manifest markers should be alphabetical");
    }

    #[test]
    fn scan_dir_finds_cargo_toml_at_root() {
        let dir = tmp_dir("cargo");
        touch(&dir.join("Cargo.toml"));
        let hits = scan_dir(&dir);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].rel, "Cargo.toml");
        assert_eq!(hits[0].kind, HintKind::Manifest);
        assert_eq!(hits[0].label, "Rust crate");
        assert!(!hits[0].is_dir);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn scan_dir_distinguishes_dir_marker_from_file_collision() {
        let dir = tmp_dir("git-collision");
        // Create `.git` as a *file* — should not register as the
        // VCS marker (which expects a directory).
        touch(&dir.join(".git"));
        let hits = scan_dir(&dir);
        assert!(
            hits.iter().all(|h| h.label != "Git repository"),
            "expected no Git hint when .git is a file, got: {hits:?}"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn scan_dir_finds_dir_markers() {
        let dir = tmp_dir("git-real");
        fs::create_dir_all(dir.join(".git")).unwrap();
        let hits = scan_dir(&dir);
        assert!(hits.iter().any(|h| h.label == "Git repository"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn scan_dir_does_not_descend_into_subdirs() {
        let dir = tmp_dir("flat");
        fs::create_dir_all(dir.join("subproj")).unwrap();
        touch(&dir.join("subproj").join("Cargo.toml"));
        let hits = scan_dir(&dir);
        assert!(hits.is_empty(), "single-level scan should not find subproj/Cargo.toml");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn scan_dir_recursive_finds_subdir_markers() {
        let dir = tmp_dir("deep");
        fs::create_dir_all(dir.join("apps").join("web")).unwrap();
        touch(&dir.join("apps").join("web").join("package.json"));
        let hits = scan_dir_recursive(&dir, 3);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].rel, "apps/web/package.json");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn scan_dir_recursive_skips_noise_dirs() {
        let dir = tmp_dir("noise");
        fs::create_dir_all(dir.join("node_modules").join("foo")).unwrap();
        touch(&dir.join("node_modules").join("foo").join("package.json"));
        fs::create_dir_all(dir.join("real-pkg")).unwrap();
        touch(&dir.join("real-pkg").join("package.json"));
        let hits = scan_dir_recursive(&dir, 3);
        assert_eq!(hits.len(), 1);
        assert!(hits[0].rel.starts_with("real-pkg/"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn scan_dir_recursive_respects_max_depth() {
        let dir = tmp_dir("depth-cap");
        fs::create_dir_all(dir.join("a").join("b").join("c")).unwrap();
        touch(&dir.join("a").join("b").join("c").join("Cargo.toml"));
        // depth 2 from root: visits root, a/, a/b/ — does NOT enter a/b/c/.
        let hits = scan_dir_recursive(&dir, 2);
        assert!(hits.is_empty(), "depth-2 scan should not find a/b/c/Cargo.toml");
        // depth 3 should pick it up.
        let hits = scan_dir_recursive(&dir, 3);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].rel, "a/b/c/Cargo.toml");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn scan_dir_returns_alphabetical_order_by_rel() {
        let dir = tmp_dir("order");
        touch(&dir.join("package.json"));
        touch(&dir.join("Cargo.toml"));
        touch(&dir.join("go.mod"));
        let hits = scan_dir(&dir);
        let rels: Vec<&str> = hits.iter().map(|h| h.rel.as_str()).collect();
        let mut sorted = rels.clone();
        sorted.sort();
        assert_eq!(rels, sorted);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn render_summary_groups_repeated_labels() {
        let hits = vec![
            Hint {
                rel: "package.json".into(),
                kind: HintKind::Manifest,
                label: "Node.js project",
                is_dir: false,
            },
            Hint {
                rel: "apps/web/package.json".into(),
                kind: HintKind::Manifest,
                label: "Node.js project",
                is_dir: false,
            },
        ];
        let s = render_summary(&hits);
        assert!(s.contains("2 locations"));
        assert!(s.contains("Node.js project"));
    }

    #[test]
    fn render_summary_single_hit_shows_path() {
        let hits = vec![Hint {
            rel: "Cargo.toml".into(),
            kind: HintKind::Manifest,
            label: "Rust crate",
            is_dir: false,
        }];
        let s = render_summary(&hits);
        assert!(s.contains("Rust crate"));
        assert!(s.contains("Cargo.toml"));
    }

    #[test]
    fn render_summary_empty_returns_empty_string() {
        assert_eq!(render_summary(&[]), "");
    }

    #[test]
    fn relative_to_handles_root_itself() {
        let dir = tmp_dir("rel-self");
        // The file's relative path under itself is the basename.
        let p = dir.join("Cargo.toml");
        touch(&p);
        let r = relative_to(&dir, &p).unwrap();
        assert_eq!(r, "Cargo.toml");
        let _ = fs::remove_dir_all(&dir);
    }
}
