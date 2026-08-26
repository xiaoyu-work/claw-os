//! Engine package management — `cos engine ...`.
//!
//! ClawOS treats native inference runtimes (llama.cpp, onnxruntime,
//! onnxruntime-genai) as **independently upgradable system components**,
//! not compile-time dependencies. Each engine has many versions installed
//! side by side under `<data_dir>/engines/<engine>/<version>/`, and the
//! `active` field in `engines.json` decides which one cos loads at
//! runtime.
//!
//! ## Storage layout
//!
//! ```text
//! <data_dir>/engines/
//! ├── llama-cpp/
//! │   ├── b3950/{bin,lib,include}/
//! │   ├── b4001/{bin,lib,include}/
//! │   └── manifest.json   (per-version, optional — Phase 2.4)
//! ├── ort/...
//! ├── ort-genai/...
//! └── engines.json        (registry: active/previous/installed/pinned)
//! ```
//!
//! ## Phases
//!
//! - **P2.1 (this module)**: storage + registry + local install + CLI.
//!   No network. `cos engine install <name>@<ver> --from <archive>`.
//! - **P2.2**: GitHub Releases adapter — `cos engine update [--check]`.
//! - **P2.3 (now)**: dynamic loading (libloading) replaces compile-time
//!   linkage. The model engine layer reads [`active_engine_root`] +
//!   [`active_library_path`] to find the on-disk runtime each process
//!   start.
//! - **P2.4**: per-version manifests + model compat enforcement.

use std::path::PathBuf;

use serde_json::{json, Value};

pub mod download;
pub mod install_local;
pub mod manifest;
pub mod paths;
pub mod registry;
pub mod sources;

/// All engine names ClawOS knows how to manage. Used for input
/// validation and CLI help. Adding a new engine is just appending here
/// + (when remote install lands) adding asset rules.
pub const KNOWN_ENGINES: &[&str] = &["llama-cpp", "ort", "ort-genai"];

/// Known-good ONNX Runtime GenAI version verified against the bundled
/// Qwen3 embedding path. Keep the GitHub release tag separate because
/// upstream tags include a leading `v`, while model compatibility ranges
/// use plain semver.
pub const ORT_GENAI_KNOWN_GOOD_VERSION: &str = "0.14.0";
pub const ORT_GENAI_KNOWN_GOOD_TAG: &str = "v0.14.0";

pub fn is_known_engine(name: &str) -> bool {
    KNOWN_ENGINES.contains(&name)
}

/// Resolve `<engines_dir>/<engine>/<active>/` for a given engine, IF an
/// active version is recorded in the registry AND the version directory
/// exists on disk. Returns `None` otherwise.
///
/// This is the entry point the model layer uses to find a runtime to
/// load. It is intentionally cheap (one small JSON read + one stat) so
/// it can be called from `is_configured()` style hot paths.
pub fn active_engine_root(engine: &str) -> Option<PathBuf> {
    if !is_known_engine(engine) {
        return None;
    }
    let idx = registry::EnginesIndex::load_or_default().ok()?;
    let entry = idx.entry(engine)?;
    if entry.active.is_empty() {
        return None;
    }
    let root = paths::engine_version_dir(engine, &entry.active);
    if root.is_dir() {
        Some(root)
    } else {
        None
    }
}

/// Resolve the on-disk path of a specific shared library shipped by the
/// active version of `engine`. Searches the conventional payload
/// directories of an engine install:
///
///   1. `<root>/lib/<platform-filename>`  — Unix-y layout
///   2. `<root>/bin/<platform-filename>`  — Windows zip layout (where
///      llama.cpp ships flat alongside its sister DLLs)
///
/// `basename` is the platform-agnostic library stem (e.g. `"llama"` →
/// `llama.dll` / `libllama.so` / `libllama.dylib`). Returns `None` if
/// the active engine isn't installed or the file is missing.
///
/// On Linux/macOS the unversioned filename is preferred but missing it
/// is not fatal: the resolver falls back to versioned siblings (e.g.
/// `libonnxruntime.so.1.25.1`) when the upstream tarball doesn't include
/// (or extraction lost) the unversioned symlink. ORT/onnxruntime-genai
/// release tarballs ship versioned soname targets and rely on a
/// symlink which not every extractor preserves.
pub fn active_library_path(engine: &str, basename: &str) -> Option<PathBuf> {
    let root = active_engine_root(engine)?;
    let filename = platform_library_filename(basename);
    for sub in ["lib", "bin"] {
        let dir = root.join(sub);
        let exact = dir.join(&filename);
        if exact.is_file() {
            return Some(exact);
        }
        if let Some(versioned) = find_versioned_library(&dir, basename) {
            return Some(versioned);
        }
    }
    None
}

/// Scan `dir` for a versioned shared library matching `basename` and
/// return the highest-versioned hit, or `None`. No-op on Windows
/// (`.dll` libraries don't carry version suffixes in this layout).
///
/// Linux: matches `lib<basename>.so.<X>(.<Y>)*`.
/// macOS: matches `lib<basename>.<X>(.<Y>)*.dylib`.
fn find_versioned_library(dir: &std::path::Path, basename: &str) -> Option<PathBuf> {
    if cfg!(target_os = "windows") {
        return None;
    }
    let entries = std::fs::read_dir(dir).ok()?;
    let mut best: Option<(Vec<u32>, PathBuf)> = None;
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let name = path.file_name()?.to_str()?;
        let Some(version_str) = parse_versioned_library_name(name, basename) else {
            continue;
        };
        let parts: Vec<u32> = version_str
            .split('.')
            .map(|p| p.parse::<u32>().unwrap_or(0))
            .collect();
        match &best {
            None => best = Some((parts, path)),
            Some((prev, _)) if parts > *prev => best = Some((parts, path)),
            _ => {}
        }
    }
    best.map(|(_, p)| p)
}

/// Parse a versioned library filename into its version-component string.
/// Returns `None` if `name` doesn't match the expected platform shape.
///
/// Linux:   `lib<basename>.so.1.25.1` → `Some("1.25.1")`
/// macOS:   `lib<basename>.1.25.1.dylib` → `Some("1.25.1")`
/// Anything else (including the unversioned `lib<basename>.so`/`.dylib`)
/// returns `None` so the caller's exact-match path retains priority.
fn parse_versioned_library_name(name: &str, basename: &str) -> Option<String> {
    let prefix = format!("lib{basename}");
    let rest = name.strip_prefix(&prefix)?;
    let version = if cfg!(target_os = "macos") {
        // lib<basename>.1.25.1.dylib → strip `.dylib`, then leading `.`
        rest.strip_suffix(".dylib")?.strip_prefix('.')?
    } else {
        // lib<basename>.so.1.25.1 → must start with `.so.`
        rest.strip_prefix(".so.")?
    };
    let starts_with_digit = version.chars().next().is_some_and(|c| c.is_ascii_digit());
    if !starts_with_digit || !version.chars().all(|c| c.is_ascii_digit() || c == '.') {
        return None;
    }
    Some(version.to_string())
}

/// Compose the platform-specific shared library filename for a given
/// stem. `"llama"` ⇒ `"llama.dll"` on Windows, `"libllama.so"` on Linux,
/// `"libllama.dylib"` on macOS.
pub fn platform_library_filename(basename: &str) -> String {
    if cfg!(target_os = "windows") {
        format!("{basename}.dll")
    } else if cfg!(target_os = "macos") {
        format!("lib{basename}.dylib")
    } else {
        format!("lib{basename}.so")
    }
}

/// Dispatch a `cos engine <command>` invocation.
///
/// **Public surface (5 commands):**
///   - `list [<engine>] [--verbose]` — list all installed, or detailed info on one
///   - `update <engine> [--from <archive>] [--pin] [--check] [--to <tag>] [--force] [--accelerator <a>] [--no-activate]`
///       — fetch (online or offline via `--from`), install, optionally activate + pin
///   - `activate <engine>@<version>` — switch the active version (no download).
///       To roll back, run `activate <engine>@<previous>` (see `list <engine>` for previous).
///   - `remove <engine>[@<version>] [--keep N]`
///       — with `@<ver>`: uninstall that exact version
///       — without:        garbage-collect old versions, keeping N most recent (default 3)
///   - `unpin <engine>` — release the pin so future `update` calls can move forward
pub fn run(command: &str, args: &[String]) -> Result<Value, String> {
    match command {
        "list" => cmd_list(args),
        "update" => cmd_update(args),
        "activate" => cmd_activate(args),
        "remove" => cmd_remove(args),
        "unpin" => cmd_unpin(args),
        other => Err(format!(
            "unknown engine command: {other}. try: list | update | activate | remove | unpin"
        )),
    }
}

// =====================================================================
// Sub-command handlers
// =====================================================================

/// `cos engine list [<engine>] [--verbose]`. With no positional arg it
/// returns the index summary across all engines. With an engine name
/// (or `--verbose`), it returns detailed info plus the active
/// version's manifest. This subsumes the old `info` subcommand.
fn cmd_list(args: &[String]) -> Result<Value, String> {
    let mut name: Option<String> = None;
    let mut verbose = false;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--verbose" | "-v" => {
                verbose = true;
                i += 1;
            }
            other if other.starts_with("--") => {
                return Err(format!("unknown flag: {other}"));
            }
            _ => {
                if name.is_none() {
                    name = Some(args[i].clone());
                } else {
                    return Err(format!("unexpected positional arg: {}", args[i]));
                }
                i += 1;
            }
        }
    }

    let index = registry::EnginesIndex::load_or_default().map_err(|e| e.to_string())?;

    // Plain summary across all engines.
    if name.is_none() && !verbose {
        return Ok(json!({
            "engines_dir": paths::engines_dir().display().to_string(),
            "engines": index.to_list_view(),
        }));
    }

    // Detailed view requires a name. `--verbose` without a name is
    // ambiguous — bail rather than silently dumping the whole index
    // verbosely (the summary view is already complete).
    let name = name.ok_or_else(|| {
        "usage: cos engine list <engine> [--verbose] (--verbose alone needs an engine name)"
            .to_string()
    })?;

    if !is_known_engine(&name) {
        return Err(format!(
            "unknown engine: {name}. known: {}",
            KNOWN_ENGINES.join(", ")
        ));
    }
    let mut info = index.info_view(&name);
    // Attach the active version's manifest (or a synthesized fallback)
    // so operators can see ABI / GGUF compatibility claims at a glance.
    if let Some(obj) = info.as_object_mut() {
        let active = obj
            .get("active")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        if !active.is_empty() {
            let manifest_value = match manifest::EngineManifest::load(&name, &active) {
                Ok(Some(m)) => json!({
                    "found": true,
                    "source": m.source,
                    "manifest": m,
                }),
                Ok(None) => {
                    let synth = manifest::EngineManifest::synthesize(&name, &active);
                    json!({
                        "found": false,
                        "source": synth.source,
                        "manifest": synth,
                    })
                }
                Err(e) => json!({
                    "found": false,
                    "error": e.to_string(),
                }),
            };
            obj.insert("active_manifest".to_string(), manifest_value);
        }
    }
    Ok(info)
}

fn cmd_activate(args: &[String]) -> Result<Value, String> {
    let arg = args
        .first()
        .ok_or_else(|| "usage: cos engine activate <engine>@<version>".to_string())?;
    let (engine, version) = arg
        .split_once('@')
        .map(|(e, v)| (e.to_string(), v.to_string()))
        .ok_or_else(|| "expected <engine>@<version>".to_string())?;
    let mut index = registry::EnginesIndex::load_or_default().map_err(|e| e.to_string())?;
    let prev = index
        .activate(&engine, &version)
        .map_err(|e| e.to_string())?;
    index.save().map_err(|e| e.to_string())?;
    Ok(json!({
        "status": "activated",
        "engine": engine,
        "active": version,
        "previous": prev,
    }))
}

/// `cos engine remove <engine>[@<version>] [--keep N]`.
///
/// Two modes, distinguished by whether a version is given:
///   - `<engine>@<version>` → uninstall that exact version (will error
///     if it's the active or previous version — switch first with
///     `cos engine activate`).
///   - `<engine>` (no version) → garbage-collect, keeping the N most
///     recent versions plus active+previous (default `--keep 3`).
///
/// Replaces the older `gc` and `uninstall` subcommands.
fn cmd_remove(args: &[String]) -> Result<Value, String> {
    let mut positional: Option<String> = None;
    let mut keep: Option<usize> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--keep" => {
                let v = args
                    .get(i + 1)
                    .ok_or_else(|| "--keep requires a value".to_string())?;
                keep = Some(
                    v.parse()
                        .map_err(|e: std::num::ParseIntError| format!("--keep: {e}"))?,
                );
                i += 2;
            }
            other if other.starts_with("--") => {
                return Err(format!("unknown flag: {other}"));
            }
            _ => {
                if positional.is_none() {
                    positional = Some(args[i].clone());
                } else {
                    return Err(format!("unexpected positional arg: {}", args[i]));
                }
                i += 1;
            }
        }
    }
    let positional = positional
        .ok_or_else(|| "usage: cos engine remove <engine>[@<version>] [--keep N]".to_string())?;

    let mut index = registry::EnginesIndex::load_or_default().map_err(|e| e.to_string())?;
    if let Some((engine, version)) = positional.split_once('@') {
        if keep.is_some() {
            return Err(
                "--keep is for gc-mode (no version). With <engine>@<version>, --keep is ambiguous — drop one or the other."
                    .into(),
            );
        }
        let engine = engine.to_string();
        let version = version.to_string();
        index
            .uninstall(&engine, &version)
            .map_err(|e| e.to_string())?;
        index.save().map_err(|e| e.to_string())?;
        // Save committed — now reclaim the bytes. If this fails, the
        // registry is already consistent; the directory will be
        // garbage-collected on the next `gc` pass.
        if let Err(e) = registry::EnginesIndex::cleanup_uninstalled_dir(&engine, &version) {
            tracing::warn!(
                target: "cos::engine_pkg",
                engine = %engine, version = %version,
                error = %e,
                "registry uninstall committed, but install dir cleanup failed"
            );
        }
        return Ok(json!({
            "status": "uninstalled",
            "engine": engine,
            "version": version,
        }));
    }

    let engine = positional;
    let keep = keep.unwrap_or(3);
    let removed = index.gc(&engine, keep).map_err(|e| e.to_string())?;
    index.save().map_err(|e| e.to_string())?;
    for v in &removed {
        if let Err(e) = registry::EnginesIndex::cleanup_uninstalled_dir(&engine, v) {
            tracing::warn!(
                target: "cos::engine_pkg",
                engine = %engine, version = %v,
                error = %e,
                "registry gc committed, but install dir cleanup failed"
            );
        }
    }
    Ok(json!({
        "status": "gc-complete",
        "engine": engine,
        "removed": removed,
        "kept": keep,
    }))
}

fn cmd_unpin(args: &[String]) -> Result<Value, String> {
    let engine = args
        .first()
        .ok_or_else(|| "usage: cos engine unpin <engine>".to_string())?;
    let mut index = registry::EnginesIndex::load_or_default().map_err(|e| e.to_string())?;
    index.set_pinned(engine, false).map_err(|e| e.to_string())?;
    index.save().map_err(|e| e.to_string())?;
    Ok(json!({"status": "unpinned", "engine": engine}))
}

// =====================================================================
// `cos engine update <engine>` — combined offline + online installer.
//
// Flags:
//   --from <archive>            offline install from a local archive (no GitHub)
//   --pin                       pin after install so future `update` is a no-op
//   --check                     online: report what *would* be downloaded
//   --to <tag>                  online: install a specific tag instead of latest
//   --force                     online: override an active pin
//   --accelerator <a>           online: pick cpu / cuda / vulkan / ... asset
//   --no-activate               install but don't make it active
//
// `--from` and the online-only flags (`--check`, `--to`, `--force`,
// `--accelerator`) are mutually exclusive — pass one or the other.
// =====================================================================

fn cmd_update(args: &[String]) -> Result<Value, String> {
    let mut engine: Option<String> = None;
    let mut check_only = false;
    let mut to_tag: Option<String> = None;
    let mut force = false;
    let mut accelerator: Option<String> = None;
    let mut activate_flag = true;
    let mut from_archive: Option<String> = None;
    let mut pin_after = false;
    let mut version_flag: Option<String> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--check" => {
                check_only = true;
                i += 1;
            }
            "--to" => {
                to_tag = Some(
                    args.get(i + 1)
                        .cloned()
                        .ok_or_else(|| "--to requires a tag".to_string())?,
                );
                i += 2;
            }
            "--from" => {
                from_archive = Some(
                    args.get(i + 1)
                        .cloned()
                        .ok_or_else(|| "--from requires a path".to_string())?,
                );
                i += 2;
            }
            "--version" => {
                version_flag = Some(
                    args.get(i + 1)
                        .cloned()
                        .ok_or_else(|| "--version requires a value".to_string())?,
                );
                i += 2;
            }
            "--pin" => {
                pin_after = true;
                i += 1;
            }
            "--force" => {
                force = true;
                i += 1;
            }
            "--accelerator" => {
                accelerator = Some(
                    args.get(i + 1)
                        .cloned()
                        .ok_or_else(|| "--accelerator requires a value".to_string())?,
                );
                i += 2;
            }
            "--no-activate" => {
                activate_flag = false;
                i += 1;
            }
            other if other.starts_with("--") => {
                return Err(format!("unknown flag: {other}"));
            }
            _ => {
                if engine.is_none() {
                    engine = Some(args[i].clone());
                } else {
                    return Err(format!("unexpected positional arg: {}", args[i]));
                }
                i += 1;
            }
        }
    }
    let positional = engine.ok_or_else(|| {
        "usage: cos engine update <engine>[@<version>] [--from <archive>] [--pin] [--check] [--to <tag>] [--force] [--accelerator cpu|cuda|vulkan|...] [--no-activate]"
            .to_string()
    })?;
    let (engine, version_in_pos) = match positional.split_once('@') {
        Some((e, v)) => (e.to_string(), Some(v.to_string())),
        None => (positional, None),
    };
    if !is_known_engine(&engine) {
        return Err(format!(
            "unknown engine: {engine}. known: {}",
            KNOWN_ENGINES.join(", ")
        ));
    }

    // ---------- Offline path: --from <archive> ----------
    // Skips all GitHub interaction; mutually exclusive with the
    // network-only flags. This subsumes the old `cos engine install`.
    if let Some(archive) = from_archive {
        if check_only || to_tag.is_some() || accelerator.is_some() || force {
            return Err(
                "--from <archive> is offline; --check/--to/--accelerator/--force are online-only — drop one or the other"
                    .into(),
            );
        }
        let version = version_in_pos.or(version_flag).ok_or_else(|| {
            "offline install needs a version: pass <engine>@<version> or --version <v>".to_string()
        })?;
        let mut index = registry::EnginesIndex::load_or_default().map_err(|e| e.to_string())?;
        let result = install_local::install_from_archive(
            &mut index,
            &engine,
            &version,
            std::path::Path::new(&archive),
        )
        .map_err(|e| e.to_string())?;
        if activate_flag {
            index
                .activate(&engine, &version)
                .map_err(|e| e.to_string())?;
        }
        if pin_after {
            index.set_pinned(&engine, true).map_err(|e| e.to_string())?;
        }
        index.save().map_err(|e| e.to_string())?;
        return Ok(json!({
            "status": "installed",
            "source": "offline",
            "engine": engine,
            "version": version,
            "path": result.install_dir.display().to_string(),
            "files": result.files_extracted,
            "activated": activate_flag,
            "pinned": pin_after,
        }));
    }

    // ---------- Online path: GitHub Releases ----------
    if version_flag.is_some() {
        return Err(
            "--version is for offline install (--from). For online, use --to <tag> instead.".into(),
        );
    }
    let to_tag = online_release_tag(&engine, to_tag.or(version_in_pos));
    let spec = sources::github::spec_for(&engine)
        .ok_or_else(|| format!("no GitHub release source registered for engine \"{engine}\""))?;

    let mut ctx = sources::asset_select::SelectionContext::current();
    if let Some(a) = accelerator {
        ctx.accelerator = a.to_lowercase();
    }

    let token = std::env::var("GITHUB_TOKEN")
        .ok()
        .or_else(|| std::env::var("GH_TOKEN").ok());

    // Run the async install pipeline. We have to support two callers:
    //   * The synchronous CLI dispatcher (no ambient runtime) — build a
    //     small current-thread runtime and `block_on` it.
    //   * A test or embedding that *already* holds a tokio runtime —
    //     `Builder::new()...build().block_on()` from inside a runtime
    //     panics, so we use `Handle::block_on` via `block_in_place`.
    let work = run_online_install(
        engine.clone(),
        spec,
        ctx,
        to_tag.clone(),
        token,
        activate_flag,
        pin_after,
        force,
        check_only,
    );
    match tokio::runtime::Handle::try_current() {
        Ok(handle) => tokio::task::block_in_place(|| handle.block_on(work)),
        Err(_) => tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| e.to_string())?
            .block_on(work),
    }
}

fn online_release_tag(engine: &str, requested: Option<String>) -> Option<String> {
    requested.or_else(|| default_online_release_tag(engine).map(str::to_string))
}

fn default_online_release_tag(engine: &str) -> Option<&'static str> {
    match engine {
        "ort-genai" => Some(ORT_GENAI_KNOWN_GOOD_TAG),
        _ => None,
    }
}

/// Decide whether to allow installation of an asset that has no
/// publisher-supplied SHA-256 digest. Returns `Ok(())` when the
/// install may proceed (either because we have a digest or the
/// operator set `COS_ENGINE_TRUST_UNVERIFIED`). Returns `Err(msg)`
/// otherwise so the caller can surface a user-facing refusal.
///
/// Extracted so it can be unit-tested without spinning up an HTTP
/// client or hitting GitHub.
pub(crate) fn check_digest_requirement(
    engine: &str,
    tag: &str,
    asset_name: &str,
    expected_sha: Option<&str>,
) -> Result<(), String> {
    if expected_sha.is_some() {
        return Ok(());
    }
    let allow_unverified = std::env::var_os("COS_ENGINE_TRUST_UNVERIFIED")
        .map(|v| v != "0" && !v.is_empty())
        .unwrap_or(false);
    if allow_unverified {
        Ok(())
    } else {
        Err(format!(
            "refusing to install {engine}@{tag}: release asset \"{asset_name}\" is \
             missing a SHA-256 digest. Re-run with COS_ENGINE_TRUST_UNVERIFIED=1 to \
             override (insecure)."
        ))
    }
}

#[allow(clippy::too_many_arguments)]
async fn run_online_install(
    engine: String,
    spec: sources::github::GhSpec,
    ctx: sources::asset_select::SelectionContext,
    to_tag: Option<String>,
    token: Option<String>,
    activate_flag: bool,
    pin_after: bool,
    force: bool,
    check_only: bool,
) -> Result<Value, String> {
    let client = sources::github::GhClient::new().with_token(token);
    let release = match &to_tag {
        Some(tag) => client
            .tag(&spec, tag)
            .await
            .map_err(|e| format!("github: {e}"))?,
        None => client
            .latest(&spec)
            .await
            .map_err(|e| format!("github: {e}"))?,
    };

    let asset = sources::asset_select::select(&engine, &ctx, &release.assets).ok_or_else(
        || {
                format!(
                    "no compatible asset found in release {} for ({}, {}, {})",
                    release.tag_name, ctx.os, ctx.arch, ctx.accelerator
                )
            },
        )?;

        let mut index = registry::EnginesIndex::load_or_default().map_err(|e| e.to_string())?;
        let entry = index.entry(&engine).cloned().unwrap_or_default();

        if check_only {
            return Ok(json!({
                "status": "available",
                "engine": engine,
                "tag": release.tag_name,
                "active": entry.active,
                "previous": entry.previous,
                "pinned": entry.pinned,
                "asset": asset.name,
                "asset_size": asset.size,
                "asset_url": asset.browser_download_url,
                "asset_sha256": asset.sha256_hex(),
                "context": {"os": ctx.os, "arch": ctx.arch, "accelerator": ctx.accelerator},
            }));
        }

        if entry.pinned && !force {
            return Err(format!(
                "engine \"{engine}\" is pinned (active={}). pass --force to override.",
                entry.active
            ));
        }
        if entry
            .installed
            .iter()
            .any(|v| v.version == release.tag_name)
        {
            return Err(format!(
                "version \"{}\" already installed for \"{engine}\". use `cos engine activate {engine}@{}` if you want to switch.",
                release.tag_name, release.tag_name
            ));
        }

        let mut headers: Vec<(&str, &str)> =
            vec![("Accept", "application/octet-stream")];
        let auth_value;
        let token_for_dl = std::env::var("GITHUB_TOKEN")
            .ok()
            .or_else(|| std::env::var("GH_TOKEN").ok());
        if let Some(t) = &token_for_dl {
            auth_value = format!("Bearer {t}");
            headers.push(("Authorization", auth_value.as_str()));
        }
        let expected_sha = asset.sha256_hex();
        // **Refuse to install an unverified engine.** The release asset
        // must publish a SHA-256 digest (in the release notes, the
        // sibling `.sha256` file, or the `digest` field of GitHub's
        // asset metadata). Anything else means we'd be running native
        // code we can't independently authenticate. The
        // `COS_ENGINE_TRUST_UNVERIFIED=1` env var is an emergency
        // override for operators rescuing themselves from a publisher
        // outage; setting it is logged.
        check_digest_requirement(
            &engine,
            &release.tag_name,
            &asset.name,
            expected_sha.as_deref(),
        )?;
        if expected_sha.is_none() {
            tracing::warn!(
                target: "cos::engine_pkg",
                engine = %engine,
                version = %release.tag_name,
                asset = %asset.name,
                "installing engine without publisher-supplied digest (COS_ENGINE_TRUST_UNVERIFIED set)"
            );
        }
        let dl_label = format!("{engine}@{}", release.tag_name);
        let dl = download::stream_to_temp(&download::DownloadOpts {
            url: &asset.browser_download_url,
            headers: &headers,
            expected_sha256: expected_sha.as_deref(),
            label: &dl_label,
        })
        .await
        .map_err(|e| e.to_string())?;

        let install_result = install_local::install_from_archive(
            &mut index,
            &engine,
            &release.tag_name,
            dl.temp_file.path(),
        )
        .map_err(|e| e.to_string())?;

        // Stamp source metadata + sha256 on the just-recorded version.
        if let Some(entry) = index.engines.get_mut(&engine) {
            if let Some(v) = entry
                .installed
                .iter_mut()
                .find(|v| v.version == release.tag_name)
            {
                v.sha256 = dl.sha256_hex.clone();
                v.source = format!("github:{}/{}", spec.owner, spec.repo);
            }
            entry.source = format!("github:{}/{}", spec.owner, spec.repo);
            entry.last_checked = Some(chrono::Utc::now());
            if entry.accelerator.is_empty() {
                entry.accelerator = ctx.accelerator.clone();
            }
        }
        if activate_flag {
            index
                .activate(&engine, &release.tag_name)
                .map_err(|e| e.to_string())?;
        }
        if pin_after {
            index.set_pinned(&engine, true).map_err(|e| e.to_string())?;
        }
        index.save().map_err(|e| e.to_string())?;

        Ok(json!({
            "status": "updated",
            "source": "online",
            "engine": engine,
            "tag": release.tag_name,
            "asset": asset.name,
            "bytes": dl.bytes,
            "sha256": dl.sha256_hex,
            "path": install_result.install_dir.display().to_string(),
            "files": install_result.files_extracted,
            "activated": activate_flag,
            "pinned": pin_after,
        }))
}

#[cfg(test)]
mod active_helper_tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/engine_pkg/active_helper_tests.rs"
    ));
}

#[cfg(test)]
mod dispatch_tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/engine_pkg/dispatch_tests.rs"
    ));
}
