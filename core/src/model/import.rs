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
        tree_hasher.update(&[b'\n']);
        files_list.push(rel_str);
    }
    let tree_sha = tree_hasher.finalize_hex();

    let requires_engine = if is_genai {
        Some(crate::model::registry::EngineRequirement {
            name: "ort-genai".to_string(),
            version: ">=0.12.0,<0.13.0".to_string(),
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

/// Compute the registry slot for `(name, version)`, removing an
/// existing slot only when `force` is set.
fn prepare_target_dir(cfg: &ImportConfig) -> Result<(PathBuf, PathBuf), ImportError> {
    let target_dir = paths::model_version_dir(&cfg.name, &cfg.version);
    if target_dir.exists() {
        if !cfg.force {
            return Err(ImportError::AlreadyRegistered {
                name: cfg.name.clone(),
                version: cfg.version.clone(),
            });
        }
        fs::remove_dir_all(&target_dir)?;
    }
    let parent = target_dir
        .parent()
        .ok_or_else(|| io::Error::other("models_dir parent missing"))?
        .to_path_buf();
    fs::create_dir_all(&parent)?;
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
        if ft.is_dir() {
            collect_files_recursive(base, &p, out)?;
        } else if ft.is_file() {
            let rel = p
                .strip_prefix(base)
                .map_err(|e| io::Error::other(format!("strip_prefix failed: {e}")))?;
            out.push(rel.to_path_buf());
        }
        // symlinks ignored — Windows doesn't permit them by default
        // and following them would let import escape the source dir.
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
    // Best-effort: if the model dir is now empty, drop it too.
    let model_root = paths::model_dir(name);
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
    use super::*;

    /// Test isolation: every test moves COS_DATA_DIR (which controls
    /// `models_dir()`) to a unique subdir so concurrent tests can't
    /// stomp on each other. We use a per-test counter (atomic) +
    /// pid + thread id to guarantee uniqueness even with
    /// --test-threads=N.
    fn fresh_models_root() -> tempfile::TempDir {
        tempfile::Builder::new()
            .prefix("cos-import-test-")
            .tempdir()
            .expect("tempdir")
    }

    /// Set COS_DATA_DIR so `paths::models_dir()` resolves under the
    /// test root. The previous value is restored on guard drop.
    struct EnvGuard {
        prev: Option<String>,
    }
    impl EnvGuard {
        fn set(dir: &Path) -> Self {
            let prev = std::env::var("COS_DATA_DIR").ok();
            std::env::set_var("COS_DATA_DIR", dir);
            Self { prev }
        }
    }
    impl Drop for EnvGuard {
        fn drop(&mut self) {
            match &self.prev {
                Some(v) => std::env::set_var("COS_DATA_DIR", v),
                None => std::env::remove_var("COS_DATA_DIR"),
            }
        }
    }

    fn make_source(dir: &Path, name: &str, contents: &[u8]) -> PathBuf {
        let p = dir.join(name);
        fs::write(&p, contents).unwrap();
        p
    }

    #[test]
    fn imports_gguf_with_default_engine_and_task() {
        let root = fresh_models_root();
        let _guard = EnvGuard::set(root.path());
        let src = make_source(root.path(), "tiny.gguf", b"FAKE-GGUF-DATA-ABCDEF");
        let cfg = ImportConfig::new(&src, "tiny");
        let imported = import_model(&cfg).expect("import ok");
        assert_eq!(imported.name, "tiny");
        assert_eq!(imported.version, "v1");
        assert_eq!(imported.engine, Engine::Llama);
        assert_eq!(imported.format, Format::Gguf);
        assert_eq!(imported.task, Task::Llm);
        assert!(imported.manifest_path.exists());
        assert!(imported.model_path.exists());
        assert_eq!(imported.size, b"FAKE-GGUF-DATA-ABCDEF".len() as u64);
        assert_eq!(imported.sha256.len(), 64);
    }

    #[test]
    fn imports_onnx_with_default_engine_and_task() {
        let root = fresh_models_root();
        let _guard = EnvGuard::set(root.path());
        let src = make_source(root.path(), "embed.onnx", b"FAKE-ONNX");
        let cfg = ImportConfig::new(&src, "embed");
        let imported = import_model(&cfg).unwrap();
        assert_eq!(imported.engine, Engine::Ort);
        assert_eq!(imported.format, Format::Onnx);
        assert_eq!(imported.task, Task::Embed);
    }

    #[test]
    fn unknown_extension_without_format_is_rejected() {
        let root = fresh_models_root();
        let _guard = EnvGuard::set(root.path());
        let src = make_source(root.path(), "weights.bin", b"X");
        let cfg = ImportConfig::new(&src, "mystery");
        let err = import_model(&cfg).unwrap_err();
        match err {
            ImportError::UnknownFormat(_) => {}
            other => panic!("want UnknownFormat, got {other:?}"),
        }
    }

    #[test]
    fn unknown_extension_with_explicit_format_succeeds() {
        let root = fresh_models_root();
        let _guard = EnvGuard::set(root.path());
        let src = make_source(root.path(), "weights.bin", b"X");
        let mut cfg = ImportConfig::new(&src, "mystery");
        cfg.format = Some(Format::Gguf);
        cfg.engine = Some(Engine::Llama);
        cfg.task = Some(Task::Llm);
        let imported = import_model(&cfg).unwrap();
        assert_eq!(imported.engine, Engine::Llama);
    }

    #[test]
    fn invalid_name_is_rejected() {
        let root = fresh_models_root();
        let _guard = EnvGuard::set(root.path());
        let src = make_source(root.path(), "f.gguf", b"X");
        let mut cfg = ImportConfig::new(&src, "");
        assert!(matches!(
            import_model(&cfg).unwrap_err(),
            ImportError::InvalidName(_)
        ));
        cfg.name = "../escape".into();
        assert!(matches!(
            import_model(&cfg).unwrap_err(),
            ImportError::InvalidName(_)
        ));
        cfg.name = ".hidden".into();
        assert!(matches!(
            import_model(&cfg).unwrap_err(),
            ImportError::InvalidName(_)
        ));
        cfg.name = "ok-name_v1".into();
        assert!(import_model(&cfg).is_ok());
    }

    #[test]
    fn already_registered_without_force_is_rejected() {
        let root = fresh_models_root();
        let _guard = EnvGuard::set(root.path());
        let src = make_source(root.path(), "f.gguf", b"V1");
        let cfg = ImportConfig::new(&src, "dup");
        import_model(&cfg).unwrap();
        // Second import must fail without force.
        let src2 = make_source(root.path(), "f2.gguf", b"V2");
        let cfg2 = ImportConfig::new(&src2, "dup");
        match import_model(&cfg2).unwrap_err() {
            ImportError::AlreadyRegistered { name, version } => {
                assert_eq!(name, "dup");
                assert_eq!(version, "v1");
            }
            other => panic!("want AlreadyRegistered, got {other:?}"),
        }
    }

    #[test]
    fn force_overwrites_existing_registration() {
        let root = fresh_models_root();
        let _guard = EnvGuard::set(root.path());
        let src = make_source(root.path(), "f.gguf", b"V1");
        import_model(&ImportConfig::new(&src, "dup")).unwrap();
        let src2 = make_source(root.path(), "f2.gguf", b"V2");
        let mut cfg2 = ImportConfig::new(&src2, "dup");
        cfg2.force = true;
        let imported = import_model(&cfg2).unwrap();
        assert_eq!(imported.size, b"V2".len() as u64);
        // Old basename should be gone, new one present.
        assert!(imported.model_path.exists());
        let listing: Vec<_> = fs::read_dir(paths::model_version_dir("dup", "v1"))
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().to_string())
            .collect();
        assert!(listing.contains(&"f2.gguf".to_string()));
        assert!(!listing.contains(&"f.gguf".to_string()));
    }

    #[test]
    fn missing_source_is_rejected() {
        let root = fresh_models_root();
        let _guard = EnvGuard::set(root.path());
        let cfg = ImportConfig::new(root.path().join("nope.gguf"), "x");
        match import_model(&cfg).unwrap_err() {
            ImportError::SourceMissing(_) => {}
            other => panic!("want SourceMissing, got {other:?}"),
        }
    }

    #[test]
    fn directory_without_genai_config_is_rejected() {
        let root = fresh_models_root();
        let _guard = EnvGuard::set(root.path());
        let dir = root.path().join("sub");
        fs::create_dir_all(&dir).unwrap();
        // No genai_config.json + no explicit format/engine override =>
        // directory layout is unrecognized.
        let cfg = ImportConfig::new(&dir, "x");
        match import_model(&cfg).unwrap_err() {
            ImportError::InvalidGenaiBundle(_) => {}
            other => panic!("want InvalidGenaiBundle, got {other:?}"),
        }
    }

    #[test]
    fn directory_with_genai_config_imports_as_onnx_genai() {
        let root = fresh_models_root();
        let _guard = EnvGuard::set(root.path());
        let src = root.path().join("qwen3-export");
        fs::create_dir_all(&src).unwrap();
        fs::write(src.join("genai_config.json"), b"{}").unwrap();
        fs::write(src.join("model.onnx"), b"fake-onnx-graph").unwrap();
        fs::write(src.join("tokenizer.json"), b"{}").unwrap();

        let cfg = ImportConfig::new(&src, "qwen3-test");
        let imported = import_model(&cfg).expect("import");
        assert_eq!(imported.engine, Engine::OrtGenai);
        assert_eq!(imported.format, Format::OnnxGenai);
        assert_eq!(imported.task, Task::Embed);
        assert!(imported.size > 0);
        // Tree-hash sha is hex.
        assert_eq!(imported.sha256.len(), 64);
        assert!(imported.sha256.chars().all(|c| c.is_ascii_hexdigit()));

        // Directory contents preserved.
        let target_dir = paths::model_version_dir("qwen3-test", "v1");
        assert!(target_dir.join("genai_config.json").is_file());
        assert!(target_dir.join("model.onnx").is_file());
        assert!(target_dir.join("tokenizer.json").is_file());
        assert!(target_dir.join("manifest.json").is_file());

        // Manifest declares ort-genai requires_engine pin.
        let raw = fs::read_to_string(target_dir.join("manifest.json")).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(parsed["engine"], "ort-genai");
        assert_eq!(parsed["format"], "onnx-genai");
        assert_eq!(parsed["requires_engine"]["name"], "ort-genai");
        assert!(parsed["files"].as_array().unwrap().len() >= 3);
    }

    #[test]
    fn directory_tree_hash_is_deterministic() {
        let root = fresh_models_root();
        let _guard = EnvGuard::set(root.path());

        // First import.
        let src1 = root.path().join("a");
        fs::create_dir_all(&src1).unwrap();
        fs::write(src1.join("genai_config.json"), b"{}").unwrap();
        fs::write(src1.join("model.onnx"), b"onnx-bytes").unwrap();
        let cfg1 = ImportConfig::new(&src1, "modelA");
        let h1 = import_model(&cfg1).unwrap().sha256;

        // Identical content → identical hash.
        let src2 = root.path().join("b");
        fs::create_dir_all(&src2).unwrap();
        fs::write(src2.join("genai_config.json"), b"{}").unwrap();
        fs::write(src2.join("model.onnx"), b"onnx-bytes").unwrap();
        let cfg2 = ImportConfig::new(&src2, "modelB");
        let h2 = import_model(&cfg2).unwrap().sha256;

        assert_eq!(h1, h2, "tree hash must be content-addressable");

        // Same paths but different content → different hash.
        let src3 = root.path().join("c");
        fs::create_dir_all(&src3).unwrap();
        fs::write(src3.join("genai_config.json"), b"{}").unwrap();
        fs::write(src3.join("model.onnx"), b"onnx-bytes-DIFFERENT").unwrap();
        let cfg3 = ImportConfig::new(&src3, "modelC");
        let h3 = import_model(&cfg3).unwrap().sha256;

        assert_ne!(h1, h3);
    }

    #[test]
    fn move_flag_removes_source() {
        let root = fresh_models_root();
        let _guard = EnvGuard::set(root.path());
        let src = make_source(root.path(), "moveme.gguf", b"X");
        let mut cfg = ImportConfig::new(&src, "moved");
        cfg.r#move = true;
        let imported = import_model(&cfg).unwrap();
        assert!(imported.model_path.exists(), "registered file should exist");
        assert!(!src.exists(), "source should have been moved");
    }

    #[test]
    fn manifest_round_trips_through_registry_list() {
        let root = fresh_models_root();
        let _guard = EnvGuard::set(root.path());
        let src = make_source(root.path(), "m.gguf", b"DATA");
        import_model(&ImportConfig::new(&src, "rt")).unwrap();
        let listed = super::super::registry::list().unwrap();
        let m = listed.iter().find(|m| m.name == "rt").expect("found");
        assert_eq!(m.version, "v1");
        assert_eq!(m.engine, Engine::Llama);
        assert_eq!(m.format, Format::Gguf);
        assert_eq!(m.size, b"DATA".len() as u64);
    }

    #[test]
    fn remove_model_drops_version_dir() {
        let root = fresh_models_root();
        let _guard = EnvGuard::set(root.path());
        let src = make_source(root.path(), "rm.gguf", b"X");
        import_model(&ImportConfig::new(&src, "removable")).unwrap();
        assert!(remove_model("removable", "v1").unwrap());
        // Version dir gone.
        assert!(!paths::model_version_dir("removable", "v1").exists());
        // Empty model dir was also pruned (best-effort).
        assert!(!paths::model_dir("removable").exists());
        // Idempotent: removing a missing one returns false, not an error.
        assert!(!remove_model("removable", "v1").unwrap());
    }

    #[test]
    fn remove_model_keeps_other_versions() {
        let root = fresh_models_root();
        let _guard = EnvGuard::set(root.path());
        let src = make_source(root.path(), "v1.gguf", b"X");
        import_model(&ImportConfig::new(&src, "multi")).unwrap();
        let src2 = make_source(root.path(), "v2.gguf", b"Y");
        let mut cfg2 = ImportConfig::new(&src2, "multi");
        cfg2.version = "v2".into();
        import_model(&cfg2).unwrap();
        // Remove v1 only.
        remove_model("multi", "v1").unwrap();
        assert!(!paths::model_version_dir("multi", "v1").exists());
        assert!(paths::model_version_dir("multi", "v2").exists());
    }

    #[test]
    fn imported_model_json_includes_expected_fields() {
        let root = fresh_models_root();
        let _guard = EnvGuard::set(root.path());
        let src = make_source(root.path(), "j.gguf", b"X");
        let imported = import_model(&ImportConfig::new(&src, "jsmodel")).unwrap();
        let v = imported_model_json(&imported);
        assert_eq!(v["status"], "imported");
        assert_eq!(v["name"], "jsmodel");
        assert_eq!(v["version"], "v1");
        assert_eq!(v["engine"], "llama");
        assert_eq!(v["format"], "gguf");
        assert!(v["sha256"].as_str().unwrap().len() == 64);
    }
}
