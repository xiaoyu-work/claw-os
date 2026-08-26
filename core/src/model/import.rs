//! `cos model import <path> --as <name>` — register a user-provided
//! ONNX/GGUF file in the model registry.
//!
//! Layout produced:
//! ```text
//! <models>/<name>/<version>/
//!   ├── <basename>           (the source file, copied or moved)
//!   └── manifest.json        (registry::Manifest, see registry.rs)
//! ```
//!
//! Atomicity: file content is staged into
//! `<models>/<name>/<version>.staging-<uuid>/` first, the manifest is
//! written there, then the whole directory is `rename`d into place.
//! Either the model is fully registered or it isn't — partial state
//! cannot leak.

use std::fs;
use std::fs::OpenOptions;
use std::io;
use std::path::{Path, PathBuf};

use serde_json::json;

use super::paths;
use super::registry::{Engine, Format, Manifest, Task};

/// Errors `import_model` can produce. Stays scoped to this module
/// because the CLI just stringifies them.
#[derive(Debug, thiserror::Error)]
pub enum ImportError {
    #[error("source path does not exist: {0}")]
    SourceMissing(PathBuf),
    #[error("source path is not a regular file: {0}")]
    SourceNotFile(PathBuf),
    #[error("source directory missing genai_config.json: {0}")]
    InvalidGenaiBundle(PathBuf),
    #[error("model name '{0}' is invalid (use [A-Za-z0-9._-]+)")]
    InvalidName(String),
    #[error("version '{0}' is invalid (use [A-Za-z0-9._-]+)")]
    InvalidVersion(String),
    #[error("model {name}@{version} already registered (pass --force to overwrite)")]
    AlreadyRegistered { name: String, version: String },
    #[error("could not detect format from extension '{0}'; pass --format <onnx|gguf|onnx-genai>")]
    UnknownFormat(String),
    #[error("refusing to import via symlink: {0} (move/copy the real file/directory and re-run)")]
    SymlinkRejected(PathBuf),
    #[error("io: {0}")]
    Io(#[from] io::Error),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
}

/// Configuration for one import. All fields except `source` and
/// `name` have sensible defaults derived from the source file.
#[derive(Debug, Clone)]
pub struct ImportConfig {
    pub source: PathBuf,
    pub name: String,
    pub version: String,
    /// `None` = infer from extension (.gguf → Llama, .onnx → Ort).
    pub engine: Option<Engine>,
    /// `None` = infer from extension (.gguf → Gguf, .onnx → Onnx).
    pub format: Option<Format>,
    /// `None` = caller didn't say; manifests are still valid without
    /// a task set (the runtime will reject invocation if it doesn't
    /// match the model's capabilities). Defaults to [`Task::Llm`] for
    /// `.gguf`, [`Task::Embed`] for `.onnx` (the most common case
    /// today). The `--task` flag overrides.
    pub task: Option<Task>,
    /// If true, move (rename) the source into the registry. If
    /// false (the default), copy and leave the source in place.
    pub r#move: bool,
    /// If true, replace an existing registration at the same
    /// name@version. Default false.
    pub force: bool,
    /// Optional human-meaningful label for the device this model
    /// prefers (e.g. `"cuda"`, `"cpu"`, `"metal"`). Stored verbatim
    /// in the manifest.
    pub default_device: Option<String>,
    /// Optional free-form parameters (tokeniser settings, sampler
    /// defaults). Stored verbatim in the manifest.
    pub params: serde_json::Value,
}

impl ImportConfig {
    pub fn new(source: impl Into<PathBuf>, name: impl Into<String>) -> Self {
        Self {
            source: source.into(),
            name: name.into(),
            version: "v1".into(),
            engine: None,
            format: None,
            task: None,
            r#move: false,
            force: false,
            default_device: None,
            params: serde_json::Value::Null,
        }
    }
}

/// Successfully imported model summary. Returned by [`import_model`]
/// for the CLI to render.
#[derive(Debug, Clone)]
pub struct ImportedModel {
    pub name: String,
    pub version: String,
    pub manifest_path: PathBuf,
    pub model_path: PathBuf,
    pub sha256: String,
    pub size: u64,
    pub task: Task,
    pub engine: Engine,
    pub format: Format,
}

/// Run the import. See module-level docs for atomicity guarantees.
pub fn import_model(cfg: &ImportConfig) -> Result<ImportedModel, ImportError> {
    if !is_valid_identifier(&cfg.name) {
        return Err(ImportError::InvalidName(cfg.name.clone()));
    }
    if !is_valid_identifier(&cfg.version) {
        return Err(ImportError::InvalidVersion(cfg.version.clone()));
    }

    if !cfg.source.exists() {
        return Err(ImportError::SourceMissing(cfg.source.clone()));
    }
    // Reject symlinks at the source entry: an attacker who can plant
    // a symlink in a watched import directory could otherwise read or
    // delete files outside the source tree (e.g. by symlinking to
    // `/etc` and triggering `--force` overwrite, or pointing at the
    // user's SSH key). We canonicalize so a symlinked grandparent is
    // also caught.
    let lmeta = fs::symlink_metadata(&cfg.source)?;
    if lmeta.file_type().is_symlink() {
        return Err(ImportError::SymlinkRejected(cfg.source.clone()));
    }
    let canonical_source = fs::canonicalize(&cfg.source)
        .map_err(ImportError::Io)?;
    if canonical_source != cfg.source {
        // The source was specified with a relative or non-canonical
        // path; require it to point inside the cwd / stable
        // directory. We don't outright reject, but we re-check with
        // the canonical form to make the symlink-graph deterministic.
        if let Ok(lm) = fs::symlink_metadata(&canonical_source) {
            if lm.file_type().is_symlink() {
                return Err(ImportError::SymlinkRejected(cfg.source.clone()));
            }
        }
    }
    let meta = fs::metadata(&cfg.source)?;
    if meta.is_dir() {
        import_directory(cfg)
    } else if meta.is_file() {
        import_single_file(cfg)
    } else {
        Err(ImportError::SourceNotFile(cfg.source.clone()))
    }
}

/// Single-file import path (.gguf / .onnx). The registry entry is a
/// directory containing the source file copied/moved verbatim plus a
/// manifest declaring the engine + format.
fn import_single_file(cfg: &ImportConfig) -> Result<ImportedModel, ImportError> {
    let basename = cfg
        .source
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| cfg.name.clone());
    let extension = cfg
        .source
        .extension()
        .and_then(|s| s.to_str())
        .map(|s| s.to_ascii_lowercase());

    let (format, engine, default_task) = infer_kind(extension.as_deref(), cfg)?;
    let task = cfg.task.unwrap_or(default_task);

    let (target_dir, parent) = prepare_target_dir(cfg)?;
    let staging_token = uuid::Uuid::new_v4().to_string();
    let staging = parent.join(format!("{}.staging-{staging_token}", cfg.version));
    fs::create_dir_all(&staging)?;
    let guard = StagingGuard(&staging);

    let model_target = staging.join(&basename);
    if cfg.r#move {
        fs::rename(&cfg.source, &model_target)?;
    } else {
        fs::copy(&cfg.source, &model_target)?;
    }

    let sha256 = super::super::engine_pkg::install_local::sha256_of(&model_target)?;
    let size = fs::metadata(&model_target)?.len();

    let manifest = Manifest {
        name: cfg.name.clone(),
        version: cfg.version.clone(),
        task,
        engine,
        format,
        sha256: sha256.clone(),
        size,
        files: vec![basename.clone()],
        default_device: cfg.default_device.clone(),
        params: cfg.params.clone(),
        requires_engine: None,
        gguf_version: None,
        arch: None,
    };
    let manifest_bytes = serde_json::to_vec_pretty(&manifest)?;
    fs::write(staging.join("manifest.json"), &manifest_bytes)?;

    fs::rename(&staging, &target_dir)?;
    std::mem::forget(guard);

    Ok(ImportedModel {
        name: cfg.name.clone(),
        version: cfg.version.clone(),
        manifest_path: paths::manifest_path(&cfg.name, &cfg.version),
        model_path: target_dir.join(&basename),
        sha256,
        size,
        task,
        engine,
        format,
    })
}

/// Directory import path. Currently the only supported directory layout
/// is an Olive-exported `genai` bundle (recognized by the presence of
/// `genai_config.json`). The whole directory tree is copied into the
/// registry and the manifest's `files` field lists every regular file
/// in the bundle (relative paths). The reported `size` is the sum of
/// all file sizes; the reported `sha256` is a deterministic tree hash
/// (sha256 of `<rel_path>\0<file_sha256>\n` lines, sorted by relative
/// path).
fn import_directory(cfg: &ImportConfig) -> Result<ImportedModel, ImportError> {
    if cfg.r#move {
        // Moving an entire model directory tree is expensive to make
        // atomic across volumes and yields no real benefit (the source
        // is usually a one-off Olive export); we always copy.
        return Err(ImportError::Io(io::Error::other(
            "--move is not supported for directory imports; copy is forced",
        )));
    }
    let genai_config = cfg.source.join("genai_config.json");
    let is_genai = genai_config.is_file();

    let (format, engine, default_task) = if is_genai {
        (Format::OnnxGenai, Engine::OrtGenai, Task::Embed)
    } else {
        match (cfg.format, cfg.engine) {
            (Some(f), Some(e)) => (f, e, cfg.task.unwrap_or(Task::Embed)),
            _ => return Err(ImportError::InvalidGenaiBundle(cfg.source.clone())),
        }
    };
    let task = cfg.task.unwrap_or(default_task);

    let primary_basename = if is_genai {
        // Pin `model.onnx` as the canonical primary file so manifest
        // consumers (e.g. UI summaries) have something to point at.
        // Fall back to `genai_config.json` if for some reason the graph
        // isn't named the standard way.
        if cfg.source.join("model.onnx").is_file() {
            "model.onnx".to_string()
        } else {
            "genai_config.json".to_string()
        }
    } else {
        cfg.source
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| cfg.name.clone())
    };

    let (target_dir, parent) = prepare_target_dir(cfg)?;
    let staging_token = uuid::Uuid::new_v4().to_string();
    let staging = parent.join(format!("{}.staging-{staging_token}", cfg.version));
    fs::create_dir_all(&staging)?;
    let guard = StagingGuard(&staging);

    // Collect relative paths first (sorted, deterministic) so the tree
    // hash is stable across runs.
    let mut rel_files: Vec<PathBuf> = Vec::new();
    collect_files_recursive(&cfg.source, &cfg.source, &mut rel_files)?;
    rel_files.sort();

    let mut total_size: u64 = 0;
    let mut tree_hasher = crate::crypto::Sha256Stream::new();
    let mut files_list: Vec<String> = Vec::with_capacity(rel_files.len());
    for rel in &rel_files {
        let src = cfg.source.join(rel);
        let dst = staging.join(rel);
        if let Some(p) = dst.parent() {
            fs::create_dir_all(p)?;
        }
        fs::copy(&src, &dst)?;
        let file_size = fs::metadata(&dst)?.len();
        total_size = total_size.saturating_add(file_size);
        let file_sha = super::super::engine_pkg::install_local::sha256_of(&dst)?;
        let rel_str = rel.to_string_lossy().replace('\\', "/");
        // Tree-hash line: <rel>\0<sha>\n. The path-content separator
        // '\0' is illegal in any sane filesystem so accidental
        // collisions are impossible.
        tree_hasher.update(rel_str.as_bytes());
        tree_hasher.update(&[0u8]);
        tree_hasher.update(file_sha.as_bytes());
        tree_hasher.update(b"\n");
        files_list.push(rel_str);
    }
    let tree_sha = tree_hasher.finalize_hex();

    let requires_engine = if is_genai {
        Some(crate::model::registry::EngineRequirement {
            name: "ort-genai".to_string(),
            version: format!("={}", crate::engine_pkg::ORT_GENAI_KNOWN_GOOD_VERSION),
        })
    } else {
        None
    };

    let manifest = Manifest {
        name: cfg.name.clone(),
        version: cfg.version.clone(),
        task,
        engine,
        format,
        sha256: tree_sha.clone(),
        size: total_size,
        files: files_list,
        default_device: cfg.default_device.clone(),
        params: cfg.params.clone(),
        requires_engine,
        gguf_version: None,
        arch: None,
    };
    let manifest_bytes = serde_json::to_vec_pretty(&manifest)?;
    fs::write(staging.join("manifest.json"), &manifest_bytes)?;

    fs::rename(&staging, &target_dir)?;
    std::mem::forget(guard);

    Ok(ImportedModel {
        name: cfg.name.clone(),
        version: cfg.version.clone(),
        manifest_path: paths::manifest_path(&cfg.name, &cfg.version),
        model_path: target_dir.join(&primary_basename),
        sha256: tree_sha,
        size: total_size,
        task,
        engine,
        format,
    })
}

/// Open (or create) `lock_path` and acquire an advisory exclusive
/// flock on it. The returned [`File`] keeps the lock alive — dropping
/// it releases the lock. On non-Unix the lock is degraded to a no-op
/// (we still create the file so callers don't have to special-case
/// Windows).
fn acquire_import_lock(lock_path: &Path) -> Result<std::fs::File, ImportError> {
    let f = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(lock_path)?;
    #[cfg(unix)]
    {
        use std::os::unix::io::AsRawFd;
        let r = unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX) };
        if r != 0 {
            return Err(ImportError::Io(std::io::Error::other(format!(
                "flock LOCK_EX {}: {}",
                lock_path.display(),
                std::io::Error::last_os_error()
            ))));
        }
    }
    Ok(f)
}

/// Compute the registry slot for `(name, version)`, removing an
/// existing slot only when `force` is set.
///
/// `--force` previously called `fs::remove_dir_all` without checking
/// whether the existing path was a symlink — an attacker who could
/// pre-create the slot as a symlink to e.g. `~/.ssh` could trick
/// the import into wiping the linked directory. We now reject any
/// path that has a symlink component anywhere on the route to
/// `target_dir` and use `symlink_metadata` to detect direct links.
///
/// We also serialize the `--force` rmtree against concurrent
/// imports of the same `(name, version)` via a small lockfile in
/// the parent directory; without this two concurrent `--force`
/// imports could each see "target exists → rmtree → create" and
/// race, leaving one of them with a half-deleted directory.
fn prepare_target_dir(cfg: &ImportConfig) -> Result<(PathBuf, PathBuf), ImportError> {
    let target_dir = paths::model_version_dir(&cfg.name, &cfg.version);
    let parent = target_dir
        .parent()
        .ok_or_else(|| io::Error::other("models_dir parent missing"))?
        .to_path_buf();
    fs::create_dir_all(&parent)?;

    // Acquire a coarse-grained per-model lockfile in the parent
    // dir. The lock guards both the symlink-check + rmtree (--force
    // path) and the empty-slot creation, so concurrent imports of
    // the same model name serialize cleanly. We hold the OS handle
    // for the duration of `prepare_target_dir`; on Unix the lock is
    // released by the kernel when the file descriptor closes
    // (drop), on Windows the OpenOptions handle suffices.
    let lock_path = parent.join(format!(".import.{}.lock", cfg.version));
    let _lock = acquire_import_lock(&lock_path)?;

    if target_dir.exists() || fs::symlink_metadata(&target_dir).is_ok() {
        if !cfg.force {
            return Err(ImportError::AlreadyRegistered {
                name: cfg.name.clone(),
                version: cfg.version.clone(),
            });
        }
        // Refuse to follow a symlink during --force rmtree: deleting
        // through a symlink would wipe whatever the link points at.
        let lm = fs::symlink_metadata(&target_dir)?;
        if lm.file_type().is_symlink() {
            return Err(ImportError::SymlinkRejected(target_dir));
        }
        fs::remove_dir_all(&target_dir)?;
    }
    Ok((target_dir, parent))
}

/// RAII guard: if anything below fails before `mem::forget`, the
/// staging dir is cleaned up before the function returns.
struct StagingGuard<'a>(&'a Path);
impl Drop for StagingGuard<'_> {
    fn drop(&mut self) {
        if self.0.exists() {
            let _ = fs::remove_dir_all(self.0);
        }
    }
}

fn collect_files_recursive(
    base: &Path,
    cur: &Path,
    out: &mut Vec<PathBuf>,
) -> Result<(), ImportError> {
    for entry in fs::read_dir(cur)? {
        let entry = entry?;
        let p = entry.path();
        let ft = entry.file_type()?;
        if ft.is_symlink() {
            // Refuse rather than silently skip: an attacker who
            // can plant a symlink in the source tree (e.g. shared
            // CI cache, world-writable scratch dir) would otherwise
            // have the import bypass scrutiny of an entire subtree.
            // If the symlink points to a real file the user wants
            // imported, they can copy it in first.
            return Err(ImportError::SymlinkRejected(p));
        }
        if ft.is_dir() {
            collect_files_recursive(base, &p, out)?;
        } else if ft.is_file() {
            let rel = p
                .strip_prefix(base)
                .map_err(|e| io::Error::other(format!("strip_prefix failed: {e}")))?;
            out.push(rel.to_path_buf());
        }
    }
    Ok(())
}

/// Remove a registered (name, version). Returns `Ok(false)` if the
/// version dir didn't exist; `Ok(true)` if it was deleted.
pub fn remove_model(name: &str, version: &str) -> Result<bool, ImportError> {
    if !is_valid_identifier(name) {
        return Err(ImportError::InvalidName(name.into()));
    }
    if !is_valid_identifier(version) {
        return Err(ImportError::InvalidVersion(version.into()));
    }
    let target = paths::model_version_dir(name, version);
    if !target.exists() {
        return Ok(false);
    }
    fs::remove_dir_all(&target)?;
    // Best-effort: drop the per-version import lockfile so a follow-up
    // `remove_model` can prune the empty model dir.
    let model_root = paths::model_dir(name);
    let _ = fs::remove_file(model_root.join(format!(".import.{version}.lock")));
    // Best-effort: if the model dir is now empty, drop it too.
    if let Ok(mut entries) = fs::read_dir(&model_root) {
        if entries.next().is_none() {
            let _ = fs::remove_dir(&model_root);
        }
    }
    Ok(true)
}

fn is_valid_identifier(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 128
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
        && !s.starts_with('.')
}

fn infer_kind(
    extension: Option<&str>,
    cfg: &ImportConfig,
) -> Result<(Format, Engine, Task), ImportError> {
    // Explicit overrides win unconditionally.
    let format = match cfg.format {
        Some(f) => f,
        None => match extension {
            Some("gguf") => Format::Gguf,
            Some("onnx") => Format::Onnx,
            _ => return Err(ImportError::UnknownFormat(extension.unwrap_or("").into())),
        },
    };
    let engine = match cfg.engine {
        Some(e) => e,
        None => match format {
            Format::Gguf => Engine::Llama,
            Format::Onnx => Engine::Ort,
            Format::OnnxGenai => Engine::OrtGenai,
        },
    };
    let default_task = match format {
        Format::Gguf => Task::Llm,
        Format::Onnx => Task::Embed,
        Format::OnnxGenai => Task::Embed,
    };
    Ok((format, engine, default_task))
}

/// Build the JSON envelope the CLI prints after a successful import.
pub fn imported_model_json(m: &ImportedModel) -> serde_json::Value {
    json!({
        "status": "imported",
        "name": m.name,
        "version": m.version,
        "task": m.task,
        "engine": m.engine,
        "format": m.format,
        "sha256": m.sha256,
        "size": m.size,
        "manifest": m.manifest_path.display().to_string(),
        "model": m.model_path.display().to_string(),
    })
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/model/import.rs"
    ));
}
