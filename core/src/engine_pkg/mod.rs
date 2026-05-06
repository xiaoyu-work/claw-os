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
//! - **P2.3**: dynamic loading (libloading) replaces the compile-time
//!   `cfg(feature = "llama_cpp")` link.
//! - **P2.4**: per-version manifests + model compat enforcement.

use serde_json::{json, Value};

pub mod install_local;
pub mod paths;
pub mod registry;

/// All engine names ClawOS knows how to manage. Used for input
/// validation and CLI help. Adding a new engine is just appending here
/// + (when remote install lands) adding asset rules.
pub const KNOWN_ENGINES: &[&str] = &["llama-cpp", "ort", "ort-genai"];

pub fn is_known_engine(name: &str) -> bool {
    KNOWN_ENGINES.contains(&name)
}

/// Dispatch a `cos engine <command>` invocation.
pub fn run(command: &str, args: &[String]) -> Result<Value, String> {
    match command {
        "list" => cmd_list(),
        "info" => cmd_info(args),
        "install" => cmd_install(args),
        "activate" => cmd_activate(args),
        "rollback" => cmd_rollback(args),
        "pin" => cmd_pin(args),
        "unpin" => cmd_unpin(args),
        "gc" => cmd_gc(args),
        "uninstall" => cmd_uninstall(args),
        "update" => Err(
            "cos engine update lands in Phase 2.2 (GitHub Releases adapter). For now use \
             `cos engine install <name>@<version> --from <local-archive>`."
                .into(),
        ),
        other => Err(format!(
            "unknown engine command: {other}. try: list | info | install | activate | rollback | pin | unpin | gc | uninstall | update"
        )),
    }
}

// =====================================================================
// Sub-command handlers
// =====================================================================

fn cmd_list() -> Result<Value, String> {
    let index = registry::EnginesIndex::load_or_default().map_err(|e| e.to_string())?;
    Ok(json!({
        "engines_dir": paths::engines_dir().display().to_string(),
        "engines": index.to_list_view(),
    }))
}

fn cmd_info(args: &[String]) -> Result<Value, String> {
    let name = args
        .first()
        .ok_or_else(|| "usage: cos engine info <engine>".to_string())?;
    if !is_known_engine(name) {
        return Err(format!(
            "unknown engine: {name}. known: {}",
            KNOWN_ENGINES.join(", ")
        ));
    }
    let index = registry::EnginesIndex::load_or_default().map_err(|e| e.to_string())?;
    Ok(index.info_view(name))
}

fn cmd_install(args: &[String]) -> Result<Value, String> {
    let mut positional: Option<String> = None;
    let mut version_flag: Option<String> = None;
    let mut from_flag: Option<String> = None;
    let mut activate_flag = true;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--from" => {
                from_flag = Some(
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
            "--no-activate" => {
                activate_flag = false;
                i += 1;
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
    let positional = positional.ok_or_else(|| {
        "usage: cos engine install <engine>[@<version>] --from <archive> [--no-activate]"
            .to_string()
    })?;

    let (engine, version_in_pos) = match positional.split_once('@') {
        Some((e, v)) => (e.to_string(), Some(v.to_string())),
        None => (positional, None),
    };
    let version = version_in_pos
        .or(version_flag)
        .ok_or_else(|| "missing version. use <engine>@<version> or --version <v>".to_string())?;
    if !is_known_engine(&engine) {
        return Err(format!(
            "unknown engine: {engine}. known: {}",
            KNOWN_ENGINES.join(", ")
        ));
    }
    let from = from_flag.ok_or_else(|| "--from <archive> is required for P2.1".to_string())?;

    let mut index = registry::EnginesIndex::load_or_default().map_err(|e| e.to_string())?;
    let result = install_local::install_from_archive(
        &mut index,
        &engine,
        &version,
        std::path::Path::new(&from),
    )
    .map_err(|e| e.to_string())?;
    if activate_flag {
        index
            .activate(&engine, &version)
            .map_err(|e| e.to_string())?;
        index.save().map_err(|e| e.to_string())?;
    }
    Ok(json!({
        "status": "installed",
        "engine": engine,
        "version": version,
        "path": result.install_dir.display().to_string(),
        "files": result.files_extracted,
        "activated": activate_flag,
    }))
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

fn cmd_rollback(args: &[String]) -> Result<Value, String> {
    let engine = args
        .first()
        .ok_or_else(|| "usage: cos engine rollback <engine>".to_string())?;
    let mut index = registry::EnginesIndex::load_or_default().map_err(|e| e.to_string())?;
    let (now_active, now_previous) = index.rollback(engine).map_err(|e| e.to_string())?;
    index.save().map_err(|e| e.to_string())?;
    Ok(json!({
        "status": "rolled-back",
        "engine": engine,
        "active": now_active,
        "previous": now_previous,
    }))
}

fn cmd_pin(args: &[String]) -> Result<Value, String> {
    let arg = args
        .first()
        .ok_or_else(|| "usage: cos engine pin <engine>[@<version>]".to_string())?;
    let (engine, version) = match arg.split_once('@') {
        Some((e, v)) => (e.to_string(), Some(v.to_string())),
        None => (arg.clone(), None),
    };
    let mut index = registry::EnginesIndex::load_or_default().map_err(|e| e.to_string())?;
    if let Some(v) = &version {
        index.activate(&engine, v).map_err(|e| e.to_string())?;
    }
    index.set_pinned(&engine, true).map_err(|e| e.to_string())?;
    index.save().map_err(|e| e.to_string())?;
    Ok(json!({
        "status": "pinned",
        "engine": engine,
        "active": index.entry(&engine).map(|e| e.active.clone()),
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

fn cmd_gc(args: &[String]) -> Result<Value, String> {
    let mut keep: usize = 3;
    let mut engine: Option<String> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--keep" => {
                keep = args
                    .get(i + 1)
                    .ok_or_else(|| "--keep requires a value".to_string())?
                    .parse()
                    .map_err(|e: std::num::ParseIntError| e.to_string())?;
                i += 2;
            }
            other if other.starts_with("--") => {
                return Err(format!("unknown flag: {other}"));
            }
            _ => {
                engine = Some(args[i].clone());
                i += 1;
            }
        }
    }
    let engine =
        engine.ok_or_else(|| "usage: cos engine gc <engine> [--keep N]".to_string())?;
    let mut index = registry::EnginesIndex::load_or_default().map_err(|e| e.to_string())?;
    let removed = index.gc(&engine, keep).map_err(|e| e.to_string())?;
    index.save().map_err(|e| e.to_string())?;
    Ok(json!({
        "status": "gc-complete",
        "engine": engine,
        "removed": removed,
        "kept": keep,
    }))
}

fn cmd_uninstall(args: &[String]) -> Result<Value, String> {
    let arg = args
        .first()
        .ok_or_else(|| "usage: cos engine uninstall <engine>@<version>".to_string())?;
    let (engine, version) = arg
        .split_once('@')
        .map(|(e, v)| (e.to_string(), v.to_string()))
        .ok_or_else(|| "expected <engine>@<version>".to_string())?;
    let mut index = registry::EnginesIndex::load_or_default().map_err(|e| e.to_string())?;
    index
        .uninstall(&engine, &version)
        .map_err(|e| e.to_string())?;
    index.save().map_err(|e| e.to_string())?;
    Ok(json!({"status": "uninstalled", "engine": engine, "version": version}))
}
