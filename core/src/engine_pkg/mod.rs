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
pub const ORT_GENAI_KNOWN_GOOD_VERSION: &str = "0.12.2";
pub const ORT_GENAI_KNOWN_GOOD_TAG: &str = "v0.12.2";

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
        if let Err(msg) = check_digest_requirement(
            &engine,
            &release.tag_name,
            &asset.name,
            expected_sha.as_deref(),
        ) {
            return Err(msg);
        }
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
    //! Tests for the P2.3 helpers `active_engine_root` and
    //! `active_library_path`. The engine layer (model::engines::*)
    //! is the primary consumer; we duplicate coverage here to catch
    //! regressions in the resolution rules early.

    use super::*;

    fn write_index(engines_dir: &std::path::Path, engine: &str, active: &str) {
        let json = serde_json::json!({
            "version": 1,
            "engines": {
                engine: {
                    "active": active,
                    "previous": "",
                    "installed": [{"version": active, "installed_at": "2026-01-01T00:00:00Z", "bytes": 0, "source": "", "sha256": ""}],
                    "pinned": false,
                    "channel": "release",
                    "accelerator": "",
                    "source": ""
                }
            }
        });
        std::fs::write(
            engines_dir.join("engines.json"),
            serde_json::to_vec_pretty(&json).unwrap(),
        )
        .unwrap();
    }

    #[test]
    fn platform_library_filename_matches_target_os() {
        let f = platform_library_filename("llama");
        if cfg!(target_os = "windows") {
            assert_eq!(f, "llama.dll");
        } else if cfg!(target_os = "macos") {
            assert_eq!(f, "libllama.dylib");
        } else {
            assert_eq!(f, "libllama.so");
        }
    }

    #[test]
    fn active_engine_root_unknown_engine_returns_none() {
        assert!(active_engine_root("not-a-real-engine").is_none());
    }

    #[test]
    fn active_engine_root_no_index_returns_none() {
        let tmp = tempfile::tempdir().unwrap();
        paths::set_engines_dir_override(Some(tmp.path().to_path_buf()));
        assert!(active_engine_root("llama-cpp").is_none());
        paths::set_engines_dir_override(None);
    }

    #[test]
    fn active_engine_root_empty_active_returns_none() {
        let tmp = tempfile::tempdir().unwrap();
        paths::set_engines_dir_override(Some(tmp.path().to_path_buf()));
        // Index exists but `active` is empty string.
        let json = serde_json::json!({
            "version": 1,
            "engines": {
                "llama-cpp": {
                    "active": "",
                    "previous": "",
                    "installed": [],
                    "pinned": false,
                    "channel": "release",
                    "accelerator": "",
                    "source": ""
                }
            }
        });
        std::fs::write(
            tmp.path().join("engines.json"),
            serde_json::to_vec_pretty(&json).unwrap(),
        )
        .unwrap();
        assert!(active_engine_root("llama-cpp").is_none());
        paths::set_engines_dir_override(None);
    }

    #[test]
    fn active_engine_root_directory_must_exist() {
        let tmp = tempfile::tempdir().unwrap();
        paths::set_engines_dir_override(Some(tmp.path().to_path_buf()));
        // Registry says active = "v0" but no directory on disk.
        write_index(tmp.path(), "llama-cpp", "v0");
        assert!(active_engine_root("llama-cpp").is_none());
        paths::set_engines_dir_override(None);
    }

    #[test]
    fn active_engine_root_returns_path_when_present() {
        let tmp = tempfile::tempdir().unwrap();
        paths::set_engines_dir_override(Some(tmp.path().to_path_buf()));
        std::fs::create_dir_all(tmp.path().join("llama-cpp/v0/lib")).unwrap();
        write_index(tmp.path(), "llama-cpp", "v0");
        let p = active_engine_root("llama-cpp").expect("should resolve");
        assert!(p.ends_with("llama-cpp/v0") || p.ends_with("llama-cpp\\v0"));
        paths::set_engines_dir_override(None);
    }

    #[test]
    fn active_library_path_prefers_lib_over_bin() {
        let tmp = tempfile::tempdir().unwrap();
        paths::set_engines_dir_override(Some(tmp.path().to_path_buf()));
        let lib_name = platform_library_filename("llama");
        let lib_dir = tmp.path().join("llama-cpp/v0/lib");
        let bin_dir = tmp.path().join("llama-cpp/v0/bin");
        std::fs::create_dir_all(&lib_dir).unwrap();
        std::fs::create_dir_all(&bin_dir).unwrap();
        std::fs::write(lib_dir.join(&lib_name), b"x").unwrap();
        std::fs::write(bin_dir.join(&lib_name), b"y").unwrap();
        write_index(tmp.path(), "llama-cpp", "v0");
        let p = active_library_path("llama-cpp", "llama").expect("should resolve");
        assert!(p.to_string_lossy().contains("lib"));
        assert!(!p.to_string_lossy().contains("bin"));
        paths::set_engines_dir_override(None);
    }

    #[test]
    fn active_library_path_falls_back_to_bin() {
        let tmp = tempfile::tempdir().unwrap();
        paths::set_engines_dir_override(Some(tmp.path().to_path_buf()));
        let lib_name = platform_library_filename("llama");
        let bin_dir = tmp.path().join("llama-cpp/v0/bin");
        std::fs::create_dir_all(&bin_dir).unwrap();
        std::fs::write(bin_dir.join(&lib_name), b"y").unwrap();
        // No lib/ directory at all.
        write_index(tmp.path(), "llama-cpp", "v0");
        let p = active_library_path("llama-cpp", "llama").expect("should resolve via bin/");
        assert!(p.to_string_lossy().contains("bin"));
        paths::set_engines_dir_override(None);
    }

    #[test]
    fn active_library_path_none_when_file_missing() {
        let tmp = tempfile::tempdir().unwrap();
        paths::set_engines_dir_override(Some(tmp.path().to_path_buf()));
        std::fs::create_dir_all(tmp.path().join("llama-cpp/v0/lib")).unwrap();
        write_index(tmp.path(), "llama-cpp", "v0");
        // Directories exist but no library file.
        assert!(active_library_path("llama-cpp", "llama").is_none());
        paths::set_engines_dir_override(None);
    }

    #[test]
    fn parse_versioned_library_name_linux() {
        // The function is gated on cfg(target_os) at call sites, but the
        // parse helper itself runs on every host — the cfg switch only
        // chooses which suffix to strip. So on Windows we still verify
        // the macOS branch via cfg.
        if cfg!(target_os = "windows") {
            // On Windows the parser uses the macOS branch only when
            // target_os = "macos", so the linux branch is what runs.
            // Just confirm the unversioned + non-matching cases return None.
            assert_eq!(parse_versioned_library_name("llama.dll", "llama"), None);
            return;
        }
        if cfg!(target_os = "macos") {
            assert_eq!(
                parse_versioned_library_name("libonnxruntime.1.25.1.dylib", "onnxruntime"),
                Some("1.25.1".into())
            );
            // Unversioned .dylib is rejected (caller's exact-match path
            // wins).
            assert_eq!(
                parse_versioned_library_name("libonnxruntime.dylib", "onnxruntime"),
                None
            );
        } else {
            assert_eq!(
                parse_versioned_library_name("libonnxruntime.so.1.25.1", "onnxruntime"),
                Some("1.25.1".into())
            );
            assert_eq!(
                parse_versioned_library_name("libonnxruntime.so.1", "onnxruntime"),
                Some("1".into())
            );
            // Unversioned .so is rejected.
            assert_eq!(
                parse_versioned_library_name("libonnxruntime.so", "onnxruntime"),
                None
            );
            // Wrong basename rejected.
            assert_eq!(
                parse_versioned_library_name("libllama.so.1.0", "onnxruntime"),
                None
            );
            // Non-numeric reject.
            assert_eq!(
                parse_versioned_library_name("libonnxruntime.so.dev", "onnxruntime"),
                None
            );
            // Leading-dot reject (".." after strip_prefix would have empty digit lead).
            assert_eq!(
                parse_versioned_library_name("libonnxruntime.so..1", "onnxruntime"),
                None
            );
        }
    }

    #[cfg(not(target_os = "windows"))]
    #[test]
    fn active_library_path_finds_versioned_library_when_unversioned_missing() {
        let tmp = tempfile::tempdir().unwrap();
        paths::set_engines_dir_override(Some(tmp.path().to_path_buf()));
        let lib_dir = tmp.path().join("ort/v0/lib");
        std::fs::create_dir_all(&lib_dir).unwrap();
        let versioned_name = if cfg!(target_os = "macos") {
            "libonnxruntime.1.25.1.dylib"
        } else {
            "libonnxruntime.so.1.25.1"
        };
        std::fs::write(lib_dir.join(versioned_name), b"x").unwrap();
        write_index(tmp.path(), "ort", "v0");
        let p = active_library_path("ort", "onnxruntime").expect("should resolve via versioned");
        assert!(p.to_string_lossy().contains(versioned_name));
        paths::set_engines_dir_override(None);
    }

    #[cfg(not(target_os = "windows"))]
    #[test]
    fn active_library_path_unversioned_preferred_over_versioned() {
        let tmp = tempfile::tempdir().unwrap();
        paths::set_engines_dir_override(Some(tmp.path().to_path_buf()));
        let lib_dir = tmp.path().join("ort/v0/lib");
        std::fs::create_dir_all(&lib_dir).unwrap();
        let unversioned_name = platform_library_filename("onnxruntime");
        let versioned_name = if cfg!(target_os = "macos") {
            "libonnxruntime.1.25.1.dylib"
        } else {
            "libonnxruntime.so.1.25.1"
        };
        std::fs::write(lib_dir.join(&unversioned_name), b"u").unwrap();
        std::fs::write(lib_dir.join(versioned_name), b"v").unwrap();
        write_index(tmp.path(), "ort", "v0");
        let p = active_library_path("ort", "onnxruntime").expect("should resolve");
        assert!(p.ends_with(&unversioned_name));
        paths::set_engines_dir_override(None);
    }

    #[cfg(not(target_os = "windows"))]
    #[test]
    fn active_library_path_picks_highest_versioned() {
        let tmp = tempfile::tempdir().unwrap();
        paths::set_engines_dir_override(Some(tmp.path().to_path_buf()));
        let lib_dir = tmp.path().join("ort/v0/lib");
        std::fs::create_dir_all(&lib_dir).unwrap();
        let (lower, higher) = if cfg!(target_os = "macos") {
            ("libonnxruntime.1.20.0.dylib", "libonnxruntime.1.25.1.dylib")
        } else {
            ("libonnxruntime.so.1.20.0", "libonnxruntime.so.1.25.1")
        };
        std::fs::write(lib_dir.join(lower), b"a").unwrap();
        std::fs::write(lib_dir.join(higher), b"b").unwrap();
        write_index(tmp.path(), "ort", "v0");
        let p = active_library_path("ort", "onnxruntime").expect("should resolve");
        assert!(p.to_string_lossy().contains(higher));
        paths::set_engines_dir_override(None);
    }
}

#[cfg(test)]
mod dispatch_tests {
    //! End-to-end coverage for the consolidated CLI surface (5 commands).
    //! Earlier cuts had `info`/`install`/`pin`/`gc`/`uninstall`/`rollback` as
    //! separate dispatch arms; these tests pin the new shape so we don't
    //! accidentally re-grow them.

    use super::*;

    fn fresh_engines_dir() -> tempfile::TempDir {
        let tmp = tempfile::tempdir().unwrap();
        paths::set_engines_dir_override(Some(tmp.path().to_path_buf()));
        tmp
    }

    fn write_three_versions(engines_dir: &std::path::Path) {
        let json = serde_json::json!({
            "version": 1,
            "engines": {
                "llama-cpp": {
                    "active": "v3",
                    "previous": "v2",
                    "installed": [
                        {"version": "v1", "installed_at": "2026-01-01T00:00:00Z", "bytes": 0, "source": "", "sha256": ""},
                        {"version": "v2", "installed_at": "2026-02-01T00:00:00Z", "bytes": 0, "source": "", "sha256": ""},
                        {"version": "v3", "installed_at": "2026-03-01T00:00:00Z", "bytes": 0, "source": "", "sha256": ""}
                    ],
                    "pinned": false,
                    "channel": "release",
                    "accelerator": "",
                    "source": ""
                }
            }
        });
        std::fs::write(
            engines_dir.join("engines.json"),
            serde_json::to_vec_pretty(&json).unwrap(),
        )
        .unwrap();
        for v in ["v1", "v2", "v3"] {
            std::fs::create_dir_all(engines_dir.join("llama-cpp").join(v).join("lib")).unwrap();
        }
    }

    #[test]
    fn rejected_legacy_subcommands_have_consistent_error_shape() {
        // Each of these used to be a top-level command. Make sure the
        // unknown-command error names them in the suggested set.
        for cmd in ["info", "install", "pin", "gc", "uninstall", "rollback"] {
            let err = run(cmd, &[]).expect_err("legacy command should be rejected");
            assert!(
                err.contains("unknown engine command")
                    && err.contains("list | update | activate | remove | unpin"),
                "cmd={cmd}: unexpected error: {err}"
            );
        }
    }

    #[test]
    fn list_without_args_returns_index_summary() {
        let _tmp = fresh_engines_dir();
        let v = run("list", &[]).expect("list should succeed");
        let obj = v.as_object().expect("object");
        assert!(obj.contains_key("engines"));
        assert!(obj.contains_key("engines_dir"));
        paths::set_engines_dir_override(None);
    }

    #[test]
    fn list_with_engine_name_returns_detail_view() {
        let tmp = fresh_engines_dir();
        write_three_versions(tmp.path());
        let v = run("list", &["llama-cpp".to_string()]).expect("verbose list should succeed");
        let obj = v.as_object().expect("object");
        assert_eq!(
            obj.get("active").and_then(|v| v.as_str()),
            Some("v3"),
            "active should be the v3 we wrote",
        );
        assert!(
            obj.contains_key("active_manifest"),
            "detail view must attach active_manifest",
        );
        paths::set_engines_dir_override(None);
    }

    #[test]
    fn list_verbose_alone_without_engine_is_explicit_error() {
        let _tmp = fresh_engines_dir();
        let err = run("list", &["--verbose".to_string()])
            .expect_err("--verbose with no name should error");
        assert!(err.contains("--verbose alone needs an engine name"));
        paths::set_engines_dir_override(None);
    }

    /// `remove <engine>@<version>` walks the uninstall path, not the gc path.
    #[test]
    fn remove_with_version_is_uninstall() {
        let tmp = fresh_engines_dir();
        write_three_versions(tmp.path());
        // v1 is neither active nor previous → safe to uninstall.
        let v = run("remove", &["llama-cpp@v1".to_string()]).expect("uninstall ok");
        assert_eq!(
            v.get("status").and_then(|v| v.as_str()),
            Some("uninstalled")
        );
        // Directory should be gone.
        assert!(!tmp.path().join("llama-cpp/v1").exists());
        paths::set_engines_dir_override(None);
    }

    /// `remove <engine>` (no version) walks the gc path.
    #[test]
    fn remove_without_version_is_gc() {
        let tmp = fresh_engines_dir();
        write_three_versions(tmp.path());
        // keep=1 means "keep one most-recent installed plus active+previous".
        let v = run(
            "remove",
            &[
                "llama-cpp".to_string(),
                "--keep".to_string(),
                "1".to_string(),
            ],
        )
        .expect("gc ok");
        assert_eq!(
            v.get("status").and_then(|v| v.as_str()),
            Some("gc-complete")
        );
        assert_eq!(v.get("kept").and_then(|v| v.as_u64()), Some(1));
        paths::set_engines_dir_override(None);
    }

    /// `remove <engine>@<ver> --keep N` is ambiguous on purpose.
    #[test]
    fn remove_versioned_with_keep_flag_is_rejected() {
        let _tmp = fresh_engines_dir();
        let err = run(
            "remove",
            &[
                "llama-cpp@v1".to_string(),
                "--keep".to_string(),
                "3".to_string(),
            ],
        )
        .expect_err("should reject ambiguous combo");
        assert!(err.contains("--keep is for gc-mode"));
        paths::set_engines_dir_override(None);
    }

    /// Update with `--from` rejects online-only flags up front so users
    /// get a deterministic error instead of a partial offline install.
    #[test]
    fn update_from_archive_rejects_online_flags() {
        let _tmp = fresh_engines_dir();
        let err = run(
            "update",
            &[
                "llama-cpp".to_string(),
                "--from".to_string(),
                "/tmp/x.tar.gz".to_string(),
                "--to".to_string(),
                "b9999".to_string(),
            ],
        )
        .expect_err("--from + --to should be rejected");
        assert!(err.contains("offline") && err.contains("online-only"));
        paths::set_engines_dir_override(None);
    }

    /// Online --version is the wrong knob; we steer users to --to.
    #[test]
    fn update_online_with_version_flag_steers_to_tag_flag() {
        let _tmp = fresh_engines_dir();
        // No --from, and a stray --version: should suggest --to instead.
        let err = run(
            "update",
            &[
                "llama-cpp".to_string(),
                "--version".to_string(),
                "b4001".to_string(),
            ],
        )
        .expect_err("online --version should error");
        assert!(err.contains("--to"));
        paths::set_engines_dir_override(None);
    }

    #[test]
    fn ort_genai_update_defaults_to_known_good_tag() {
        assert_eq!(
            online_release_tag("ort-genai", None),
            Some(ORT_GENAI_KNOWN_GOOD_TAG.to_string())
        );
        assert_eq!(
            online_release_tag("ort-genai", Some("v0.13.1".to_string())),
            Some("v0.13.1".to_string())
        );
        assert_eq!(online_release_tag("ort", None), None);
    }

    #[test]
    fn unpin_clears_the_pin_flag() {
        let tmp = fresh_engines_dir();
        write_three_versions(tmp.path());
        // Set the pin via the registry directly (not exposed as a CLI in
        // the new surface — `update --pin` is the entry point).
        let mut idx = registry::EnginesIndex::load_or_default().expect("index loads");
        idx.set_pinned("llama-cpp", true).expect("set pin");
        idx.save().expect("save");

        let v = run("unpin", &["llama-cpp".to_string()]).expect("unpin ok");
        assert_eq!(v.get("status").and_then(|v| v.as_str()), Some("unpinned"));

        let idx = registry::EnginesIndex::load_or_default().expect("reload");
        assert!(!idx.entry("llama-cpp").unwrap().pinned);
        paths::set_engines_dir_override(None);
    }

    // ---------- digest-required guard (audit fix) ----------

    /// Audit fix (engine_pkg HIGH "digest required"): online install
    /// of a release asset that does NOT publish a SHA-256 digest
    /// must be refused, because the kernel would otherwise execute
    /// unverified native code shipped by the publisher's CDN. The
    /// `COS_ENGINE_TRUST_UNVERIFIED=1` env var is the documented
    /// emergency override.
    ///
    /// We test the decision helper directly — the rest of the
    /// install path requires a live network, which we don't want
    /// in unit tests.
    #[test]
    fn digest_required() {
        // Clean env to a known state. Use a unique key with
        // env::remove because cargo test runs in a shared process.
        std::env::remove_var("COS_ENGINE_TRUST_UNVERIFIED");

        // No digest, no override → refuse with a clear message.
        let err = check_digest_requirement("llama-cpp", "b4001", "llama-cpp.tar.gz", None)
            .expect_err("must refuse without digest");
        assert!(
            err.contains("missing a SHA-256 digest"),
            "error must explain the refusal reason, got: {err}",
        );
        assert!(
            err.contains("COS_ENGINE_TRUST_UNVERIFIED"),
            "error must mention the override env var, got: {err}",
        );

        // Override → allowed.
        std::env::set_var("COS_ENGINE_TRUST_UNVERIFIED", "1");
        assert!(check_digest_requirement(
            "llama-cpp",
            "b4001",
            "llama-cpp.tar.gz",
            None
        )
        .is_ok());
        std::env::remove_var("COS_ENGINE_TRUST_UNVERIFIED");

        // Empty / "0" must NOT be treated as on.
        std::env::set_var("COS_ENGINE_TRUST_UNVERIFIED", "0");
        assert!(check_digest_requirement(
            "llama-cpp",
            "b4001",
            "llama-cpp.tar.gz",
            None
        )
        .is_err());
        std::env::set_var("COS_ENGINE_TRUST_UNVERIFIED", "");
        assert!(check_digest_requirement(
            "llama-cpp",
            "b4001",
            "llama-cpp.tar.gz",
            None
        )
        .is_err());
        std::env::remove_var("COS_ENGINE_TRUST_UNVERIFIED");

        // With a digest, the check passes regardless of env state.
        assert!(check_digest_requirement(
            "llama-cpp",
            "b4001",
            "llama-cpp.tar.gz",
            Some("deadbeef"),
        )
        .is_ok());
    }
}
