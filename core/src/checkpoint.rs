/// OS-level checkpoint system using OverlayFS.
///
/// The /workspace directory is mounted as an OverlayFS:
///   - lower (base): read-only original state
///   - upper: all modifications (copy-on-write)
///   - work: overlayfs internal workdir
///   - merged: /workspace (what the agent sees)
///
/// Checkpoints freeze the current upper layer and start a new one,
/// giving agents instant snapshot/rollback without git or file copies.
///
/// Directory layout under `$COS_DATA_DIR/overlay/`:
/// ```text
/// base/                ← original /workspace content (lower layer)
/// upper/               ← current modifications
/// work/                ← overlayfs workdir
/// checkpoints/         ← frozen upper layers
///   001-description/
///     meta.json        ← {id, description, created_at, files_changed}
///     layer/           ← the frozen upper directory
/// ```
///
/// Commands:
///   create [description]   — freeze current upper, start fresh
///   diff                   — scan upper for created/modified/deleted files
///   rollback [id]          — restore a checkpoint or wipe current upper
///   list                   — list all saved checkpoints
///   status                 — overlay mount state + pending changes + disk usage
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::fs;
use std::path::{Path, PathBuf};
#[cfg(target_os = "linux")]
use std::process::Command;

use crate::policy::{self, OpType};

// ---------------------------------------------------------------------------
// Path helpers
// ---------------------------------------------------------------------------

fn overlay_dir() -> PathBuf {
    PathBuf::from(
        std::env::var("COS_DATA_DIR").unwrap_or_else(|_| "/var/lib/cos".into()),
    )
    .join("overlay")
}

fn workspace_dir() -> PathBuf {
    PathBuf::from(
        std::env::var("WORKSPACE").unwrap_or_else(|_| "/workspace".into()),
    )
}

// ---------------------------------------------------------------------------
// Checkpoint metadata
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CheckpointMeta {
    id: String,
    description: String,
    created_at: String,
    files_changed: usize,
}

// ---------------------------------------------------------------------------
// Mount / unmount
// ---------------------------------------------------------------------------

/// Mount the workspace as an overlayfs.
///
/// Only available on Linux — other platforms return an error.
#[cfg(target_os = "linux")]
fn mount_overlay() -> Result<(), String> {
    let overlay = overlay_dir();
    let lower = overlay.join("base");
    let upper = overlay.join("upper");
    let work = overlay.join("work");
    let merged = workspace_dir();

    for d in [&lower, &upper, &work] {
        fs::create_dir_all(d)
            .map_err(|e| format!("failed to create {}: {e}", d.display()))?;
    }
    fs::create_dir_all(&merged)
        .map_err(|e| format!("failed to create {}: {e}", merged.display()))?;

    let opts = format!(
        "lowerdir={},upperdir={},workdir={}",
        lower.display(),
        upper.display(),
        work.display(),
    );

    let output = Command::new("mount")
        .args(["-t", "overlay", "overlay", "-o", &opts])
        .arg(merged.to_string_lossy().as_ref())
        .output()
        .map_err(|e| format!("mount exec failed: {e}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("mount failed: {stderr}"));
    }
    Ok(())
}

#[cfg(not(target_os = "linux"))]
fn mount_overlay() -> Result<(), String> {
    Err("overlayfs requires Linux".into())
}

/// Unmount the workspace overlay.
#[cfg(target_os = "linux")]
fn umount_overlay() -> Result<(), String> {
    let merged = workspace_dir();
    let output = Command::new("umount")
        .arg(merged.to_string_lossy().as_ref())
        .output()
        .map_err(|e| format!("umount exec failed: {e}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("umount failed: {stderr}"));
    }
    Ok(())
}

#[cfg(not(target_os = "linux"))]
fn umount_overlay() -> Result<(), String> {
    Err("overlayfs requires Linux".into())
}

// ---------------------------------------------------------------------------
// Checkpoint ID generation
// ---------------------------------------------------------------------------

/// Scan checkpoints/ for the highest numeric prefix and return the next one,
/// zero-padded to 3 digits.
fn next_checkpoint_id(checkpoints_dir: &Path) -> String {
    let max = existing_ids(checkpoints_dir)
        .into_iter()
        .max()
        .unwrap_or(0);
    format!("{:03}", max + 1)
}

/// Return all numeric IDs found in checkpoint directory names.
///
/// Directory names follow the pattern `{id}-{description}` where id is a
/// zero-padded number (e.g. `001-before-refactoring`).
fn existing_ids(checkpoints_dir: &Path) -> Vec<u32> {
    let entries = match fs::read_dir(checkpoints_dir) {
        Ok(e) => e,
        Err(_) => return Vec::new(),
    };

    entries
        .filter_map(|e| e.ok())
        .filter(|e| e.path().is_dir())
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().to_string();
            // Take everything before the first '-' as the numeric ID.
            let id_part = name.split('-').next()?;
            id_part.parse::<u32>().ok()
        })
        .collect()
}

// ---------------------------------------------------------------------------
// File counting / walking helpers
// ---------------------------------------------------------------------------

/// Count non-whiteout files in a directory tree.
fn count_files_in_upper(upper: &Path) -> usize {
    let mut count: usize = 0;
    let _ = walk_count(upper, &mut count);
    count
}

fn walk_count(dir: &Path, count: &mut usize) -> Result<(), String> {
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return Ok(()),
    };
    for entry in entries {
        let entry = entry.map_err(|e| e.to_string())?;
        let meta = entry.metadata().map_err(|e| e.to_string())?;
        if meta.is_dir() {
            walk_count(&entry.path(), count)?;
        } else {
            // On Unix, skip whiteout character devices (0,0).
            #[cfg(unix)]
            {
                use std::os::unix::fs::FileTypeExt;
                if meta.file_type().is_char_device() {
                    continue;
                }
            }
            *count += 1;
        }
    }
    Ok(())
}

/// Recursively walk the upper directory and categorise files.
///
/// In an overlayfs upper layer:
///   - A regular file whose path also exists in `base_layer` → **modified**
///   - A regular file whose path does NOT exist in `base_layer` → **created**
///   - A character device with major/minor 0,0 → **deleted** (whiteout)
///
/// `upper_root` is the top-level upper directory (used to compute relative paths).
/// `current` is the directory currently being iterated (starts equal to `upper_root`).
/// `base_layer` is the lower/base directory to check for pre-existing files.
fn walk_upper(
    upper_root: &Path,
    current: &Path,
    base_layer: &Path,
    created: &mut Vec<String>,
    modified: &mut Vec<String>,
    deleted: &mut Vec<String>,
) -> Result<(), String> {
    let entries = fs::read_dir(current).map_err(|e| e.to_string())?;

    for entry in entries {
        let entry = entry.map_err(|e| e.to_string())?;
        let path = entry.path();
        let relative = path
            .strip_prefix(upper_root)
            .map_err(|e| format!("path {} is not under upper_root {}: {e}", path.display(), upper_root.display()))?
            .to_string_lossy()
            .to_string();

        let meta = entry.metadata().map_err(|e| e.to_string())?;

        if meta.is_dir() {
            walk_upper(upper_root, &path, base_layer, created, modified, deleted)?;
        } else {
            // Check for whiteout (character device with major/minor 0,0).
            #[cfg(unix)]
            {
                use std::os::unix::fs::FileTypeExt;
                if meta.file_type().is_char_device() {
                    deleted.push(relative);
                    continue;
                }
            }

            // File exists in base → modified; otherwise → created.
            let base_path = base_layer.join(&relative);
            if base_path.exists() {
                modified.push(relative);
            } else {
                created.push(relative);
            }
        }
    }
    Ok(())
}

/// Approximate disk usage for a directory tree (sum of file sizes).
fn dir_size(path: &Path) -> u64 {
    let mut total: u64 = 0;
    if let Ok(entries) = fs::read_dir(path) {
        for entry in entries.flatten() {
            let meta = match entry.metadata() {
                Ok(m) => m,
                Err(_) => continue,
            };
            if meta.is_dir() {
                total += dir_size(&entry.path());
            } else {
                total += meta.len();
            }
        }
    }
    total
}

// ---------------------------------------------------------------------------
// Sanitise description for use in directory names
// ---------------------------------------------------------------------------

/// Replace non-alphanumeric characters with hyphens, collapse runs, and trim.
fn sanitize_description(desc: &str) -> String {
    let s: String = desc
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { '-' })
        .collect();
    // Collapse consecutive hyphens.
    let mut out = String::with_capacity(s.len());
    let mut prev_dash = false;
    for c in s.chars() {
        if c == '-' {
            if !prev_dash {
                out.push('-');
            }
            prev_dash = true;
        } else {
            out.push(c);
            prev_dash = false;
        }
    }
    out.trim_matches('-').to_lowercase()
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

pub fn run(command: &str, args: &[String]) -> Result<Value, String> {
    match command {
        "create" => cmd_create(args),
        "diff" => cmd_diff(args),
        "rollback" => cmd_rollback(args),
        "list" => cmd_list(args),
        "status" => cmd_status(args),
        "quota-set" => cmd_quota_set(args),
        "quota-status" => cmd_quota_status(args),
        "namespaces" => cmd_namespaces(args),
        _ => Err(format!("unknown checkpoint command: {command}")),
    }
}

// ---------------------------------------------------------------------------
// cos checkpoint create [description]
// ---------------------------------------------------------------------------

fn cmd_create(args: &[String]) -> Result<Value, String> {
    policy::require(OpType::System).map_err(|v| v.to_string())?;

    let description = if args.is_empty() {
        "checkpoint".to_string()
    } else {
        args.join(" ")
    };

    let overlay = overlay_dir();
    let upper = overlay.join("upper");
    let checkpoints_dir = overlay.join("checkpoints");

    fs::create_dir_all(&checkpoints_dir)
        .map_err(|e| format!("failed to create checkpoints dir: {e}"))?;

    let id = next_checkpoint_id(&checkpoints_dir);
    let slug = sanitize_description(&description);
    let dir_name = if slug.is_empty() {
        id.clone()
    } else {
        format!("{id}-{slug}")
    };

    let cp_dir = checkpoints_dir.join(&dir_name);
    let cp_layer = cp_dir.join("layer");

    // Count files before we move.
    let files_changed = count_files_in_upper(&upper);

    // 1. Unmount the overlay (best-effort — may not be mounted).
    let _ = umount_overlay();

    // 2. Move current upper → checkpoint layer.
    fs::create_dir_all(&cp_dir)
        .map_err(|e| format!("failed to create checkpoint dir: {e}"))?;
    fs::rename(&upper, &cp_layer)
        .map_err(|e| format!("failed to move upper to checkpoint: {e}"))?;

    // 3. Write meta.json.
    let now = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();
    let meta = CheckpointMeta {
        id: id.clone(),
        description: description.clone(),
        created_at: now.clone(),
        files_changed,
    };
    let meta_path = cp_dir.join("meta.json");
    let meta_json = serde_json::to_string_pretty(&meta)
        .map_err(|e| format!("failed to serialize meta: {e}"))?;
    fs::write(&meta_path, &meta_json)
        .map_err(|e| format!("failed to write meta.json: {e}"))?;

    // 4. Create fresh empty upper + work dir.
    fs::create_dir_all(&upper)
        .map_err(|e| format!("failed to create fresh upper: {e}"))?;
    // Recreate work dir — overlayfs requires a clean workdir after remount.
    let work = overlay.join("work");
    let _ = fs::remove_dir_all(&work);
    fs::create_dir_all(&work)
        .map_err(|e| format!("failed to create work dir: {e}"))?;

    // 5. Remount overlay (best-effort).
    let mount_err = mount_overlay().err();

    let mut result = json!({
        "id": id,
        "description": description,
        "created_at": now,
        "files_changed": files_changed,
        "checkpoint_dir": dir_name,
    });

    if let Some(err) = mount_err {
        result["warning"] = json!(format!("overlay remount failed: {err}"));
    }

    Ok(result)
}

// ---------------------------------------------------------------------------
// cos checkpoint diff
// ---------------------------------------------------------------------------

fn cmd_diff(_args: &[String]) -> Result<Value, String> {
    policy::require(OpType::Read).map_err(|v| v.to_string())?;

    let overlay = overlay_dir();
    let upper = overlay.join("upper");
    let base_layer = overlay.join("base");

    if !upper.exists() {
        return Ok(json!({
            "created": [],
            "modified": [],
            "deleted": [],
            "total_changes": 0,
            "note": "upper directory does not exist — no overlay active",
        }));
    }

    let mut created = Vec::new();
    let mut modified = Vec::new();
    let mut deleted = Vec::new();

    walk_upper(&upper, &upper, &base_layer, &mut created, &mut modified, &mut deleted)?;

    created.sort();
    modified.sort();
    deleted.sort();

    Ok(json!({
        "created": created,
        "modified": modified,
        "deleted": deleted,
        "total_changes": created.len() + modified.len() + deleted.len(),
    }))
}

// ---------------------------------------------------------------------------
// cos checkpoint rollback [checkpoint-id]
// ---------------------------------------------------------------------------

fn cmd_rollback(args: &[String]) -> Result<Value, String> {
    policy::require(OpType::System).map_err(|v| v.to_string())?;

    let overlay = overlay_dir();
    let upper = overlay.join("upper");
    let checkpoints_dir = overlay.join("checkpoints");

    // Count pending changes before rollback.
    let changes_reverted = count_files_in_upper(&upper);

    // 1. Unmount overlay (best-effort).
    let _ = umount_overlay();

    // 2. Wipe current upper.
    if upper.exists() {
        fs::remove_dir_all(&upper)
            .map_err(|e| format!("failed to remove upper: {e}"))?;
    }

    // 3. Determine what to restore.
    let rolled_back_to: String;

    if let Some(target_id) = args.first() {
        // Find the checkpoint whose id matches.
        let cp_dir = find_checkpoint_dir(&checkpoints_dir, target_id)?;
        let layer = cp_dir.join("layer");
        if !layer.exists() {
            return Err(format!("checkpoint layer not found: {}", layer.display()));
        }

        // Copy (not move) the layer as the new upper so the checkpoint is
        // preserved for future rollbacks.
        copy_dir_recursive(&layer, &upper)
            .map_err(|e| format!("failed to restore checkpoint layer: {e}"))?;
        rolled_back_to = target_id.clone();
    } else {
        // No id → reset to base (empty upper).
        fs::create_dir_all(&upper)
            .map_err(|e| format!("failed to create empty upper: {e}"))?;
        rolled_back_to = "base".to_string();
    }

    // 4. Recreate work dir.
    let work = overlay.join("work");
    let _ = fs::remove_dir_all(&work);
    fs::create_dir_all(&work)
        .map_err(|e| format!("failed to create work dir: {e}"))?;

    // 5. Remount overlay (best-effort).
    let mount_err = mount_overlay().err();

    let mut result = json!({
        "rolled_back_to": rolled_back_to,
        "changes_reverted": changes_reverted,
    });

    if let Some(err) = mount_err {
        result["warning"] = json!(format!("overlay remount failed: {err}"));
    }

    Ok(result)
}

/// Locate a checkpoint directory by its numeric id prefix (e.g. "001").
fn find_checkpoint_dir(checkpoints_dir: &Path, id: &str) -> Result<PathBuf, String> {
    let entries = fs::read_dir(checkpoints_dir)
        .map_err(|e| format!("cannot read checkpoints dir: {e}"))?;

    let prefix = format!("{id}-");
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if name == id || name.starts_with(&prefix) {
            let p = entry.path();
            if p.is_dir() {
                return Ok(p);
            }
        }
    }
    Err(format!("checkpoint not found: {id}"))
}

/// Recursively copy a directory tree.
fn copy_dir_recursive(src: &Path, dst: &Path) -> Result<(), String> {
    fs::create_dir_all(dst).map_err(|e| format!("mkdir {}: {e}", dst.display()))?;

    for entry in fs::read_dir(src).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        let src_path = entry.path();
        let dst_path = dst.join(entry.file_name());

        if src_path.is_dir() {
            copy_dir_recursive(&src_path, &dst_path)?;
        } else {
            fs::copy(&src_path, &dst_path)
                .map_err(|e| format!("copy {} → {}: {e}", src_path.display(), dst_path.display()))?;
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// cos checkpoint list
// ---------------------------------------------------------------------------

fn cmd_list(_args: &[String]) -> Result<Value, String> {
    policy::require(OpType::Read).map_err(|v| v.to_string())?;

    let checkpoints_dir = overlay_dir().join("checkpoints");
    if !checkpoints_dir.exists() {
        return Ok(json!({
            "checkpoints": [],
            "count": 0,
        }));
    }

    let mut checkpoints: Vec<Value> = Vec::new();

    let mut dirs: Vec<_> = fs::read_dir(&checkpoints_dir)
        .map_err(|e| format!("cannot read checkpoints dir: {e}"))?
        .filter_map(|e| e.ok())
        .filter(|e| e.path().is_dir())
        .collect();
    dirs.sort_by_key(|e| e.file_name());

    for entry in dirs {
        let meta_path = entry.path().join("meta.json");
        if let Ok(data) = fs::read_to_string(&meta_path) {
            if let Ok(meta) = serde_json::from_str::<CheckpointMeta>(&data) {
                checkpoints.push(json!({
                    "id": meta.id,
                    "description": meta.description,
                    "created_at": meta.created_at,
                    "files_changed": meta.files_changed,
                }));
            }
        }
    }

    Ok(json!({
        "checkpoints": checkpoints,
        "count": checkpoints.len(),
    }))
}

// ---------------------------------------------------------------------------
// cos checkpoint status
// ---------------------------------------------------------------------------

fn cmd_status(_args: &[String]) -> Result<Value, String> {
    policy::require(OpType::Read).map_err(|v| v.to_string())?;

    let overlay = overlay_dir();
    let upper = overlay.join("upper");
    let checkpoints_dir = overlay.join("checkpoints");

    let overlay_mounted = is_overlay_mounted();

    let pending_changes = if upper.exists() {
        count_files_in_upper(&upper)
    } else {
        0
    };

    let checkpoint_count = if checkpoints_dir.exists() {
        fs::read_dir(&checkpoints_dir)
            .map(|entries| {
                entries
                    .filter_map(|e| e.ok())
                    .filter(|e| e.path().is_dir())
                    .count()
            })
            .unwrap_or(0)
    } else {
        0
    };

    let upper_bytes = if upper.exists() { dir_size(&upper) } else { 0 };
    let checkpoints_bytes = if checkpoints_dir.exists() {
        dir_size(&checkpoints_dir)
    } else {
        0
    };

    Ok(json!({
        "overlay_mounted": overlay_mounted,
        "pending_changes": pending_changes,
        "checkpoint_count": checkpoint_count,
        "disk_usage": {
            "upper_bytes": upper_bytes,
            "upper_mb": upper_bytes / (1024 * 1024),
            "checkpoints_bytes": checkpoints_bytes,
            "checkpoints_mb": checkpoints_bytes / (1024 * 1024),
            "total_bytes": upper_bytes + checkpoints_bytes,
            "total_mb": (upper_bytes + checkpoints_bytes) / (1024 * 1024),
        },
        "overlay_dir": overlay.to_string_lossy(),
        "workspace": workspace_dir().to_string_lossy(),
    }))
}

/// Check whether the workspace is currently an overlayfs mount.
///
/// Reads /proc/mounts on Linux; returns false on other platforms.
fn is_overlay_mounted() -> bool {
    #[cfg(target_os = "linux")]
    {
        let workspace = workspace_dir();
        let ws_str = workspace.to_string_lossy();
        if let Ok(mounts) = fs::read_to_string("/proc/mounts") {
            for line in mounts.lines() {
                if line.contains("overlay") && line.contains(ws_str.as_ref()) {
                    return true;
                }
            }
        }
        false
    }
    #[cfg(not(target_os = "linux"))]
    {
        false
    }
}

// ---------------------------------------------------------------------------
// Quota management
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
struct QuotaConfig {
    limit_bytes: u64,
}

fn quota_path() -> PathBuf {
    overlay_dir().join("quota.json")
}

fn load_quota() -> Option<QuotaConfig> {
    let path = quota_path();
    let data = fs::read_to_string(&path).ok()?;
    serde_json::from_str(&data).ok()
}

fn save_quota(cfg: &QuotaConfig) {
    let dir = overlay_dir();
    let _ = fs::create_dir_all(&dir);
    if let Ok(data) = serde_json::to_string_pretty(cfg) {
        let _ = fs::write(quota_path(), data);
    }
}

/// Parse a human-readable size string like "2G", "512M", "100K" into bytes.
fn parse_size(s: &str) -> Result<u64, String> {
    let s = s.trim();
    if s.is_empty() {
        return Err("empty size string".into());
    }
    let (num_str, multiplier) = if s.ends_with('G') || s.ends_with('g') {
        (&s[..s.len() - 1], 1024u64 * 1024 * 1024)
    } else if s.ends_with('M') || s.ends_with('m') {
        (&s[..s.len() - 1], 1024u64 * 1024)
    } else if s.ends_with('K') || s.ends_with('k') {
        (&s[..s.len() - 1], 1024u64)
    } else {
        (s, 1u64)
    };
    let num: f64 = num_str.parse().map_err(|_| format!("invalid size: {s}"))?;
    Ok((num * multiplier as f64) as u64)
}

fn format_bytes(bytes: u64) -> String {
    if bytes >= 1024 * 1024 * 1024 {
        format!("{:.1}G", bytes as f64 / (1024.0 * 1024.0 * 1024.0))
    } else if bytes >= 1024 * 1024 {
        format!("{:.1}M", bytes as f64 / (1024.0 * 1024.0))
    } else if bytes >= 1024 {
        format!("{:.1}K", bytes as f64 / 1024.0)
    } else {
        format!("{bytes}B")
    }
}

/// Set the filesystem quota for the upper layer.
///
/// Usage: cos checkpoint quota-set <size>  (e.g. "2G", "512M")
fn cmd_quota_set(args: &[String]) -> Result<Value, String> {
    policy::require(OpType::System).map_err(|v| v.to_string())?;

    let size_str = args
        .first()
        .ok_or("usage: cos checkpoint quota-set <size> (e.g. 2G, 512M)")?;
    let limit_bytes = parse_size(size_str)?;

    let cfg = QuotaConfig { limit_bytes };
    save_quota(&cfg);

    Ok(json!({
        "quota_set": true,
        "limit_bytes": limit_bytes,
        "limit_human": format_bytes(limit_bytes),
    }))
}

/// Show current quota status.
///
/// Usage: cos checkpoint quota-status
fn cmd_quota_status(_args: &[String]) -> Result<Value, String> {
    policy::require(OpType::Read).map_err(|v| v.to_string())?;

    let upper = overlay_dir().join("upper");
    let used = if upper.exists() { dir_size(&upper) } else { 0 };

    if let Some(quota) = load_quota() {
        let available = quota.limit_bytes.saturating_sub(used);
        let pct_used = if quota.limit_bytes > 0 {
            (used as f64 / quota.limit_bytes as f64 * 100.0) as u32
        } else {
            0
        };
        Ok(json!({
            "quota_enabled": true,
            "limit_bytes": quota.limit_bytes,
            "limit_human": format_bytes(quota.limit_bytes),
            "used_bytes": used,
            "used_human": format_bytes(used),
            "available_bytes": available,
            "available_human": format_bytes(available),
            "percent_used": pct_used,
            "exceeded": used > quota.limit_bytes,
        }))
    } else {
        Ok(json!({
            "quota_enabled": false,
            "used_bytes": used,
            "used_human": format_bytes(used),
            "hint": "Set a quota with: cos checkpoint quota-set <size>",
        }))
    }
}

/// Check if writing `additional_bytes` would exceed the quota.
/// Returns Ok(()) if within quota or no quota set, Err if exceeded.
pub fn check_quota(additional_bytes: u64) -> Result<(), String> {
    let quota = match load_quota() {
        Some(q) => q,
        None => return Ok(()), // No quota = unlimited
    };

    let upper = overlay_dir().join("upper");
    let used = if upper.exists() { dir_size(&upper) } else { 0 };

    if used + additional_bytes > quota.limit_bytes {
        Err(format!(
            "quota exceeded: used {} + new {} > limit {}",
            format_bytes(used),
            format_bytes(additional_bytes),
            format_bytes(quota.limit_bytes),
        ))
    } else {
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Multi-namespace overlay management
// ---------------------------------------------------------------------------

fn namespace_base_dir() -> PathBuf {
    PathBuf::from(
        std::env::var("COS_DATA_DIR").unwrap_or_else(|_| "/var/lib/cos".into()),
    )
    .join("overlay-namespaces")
}

/// List all overlay namespaces.
///
/// Usage: cos checkpoint namespaces [--create <name>] [--destroy <name>] [--status <name>]
fn cmd_namespaces(args: &[String]) -> Result<Value, String> {
    policy::require(OpType::Read).map_err(|v| v.to_string())?;

    if args.is_empty() {
        return list_namespaces();
    }

    match args[0].as_str() {
        "--create" if args.len() >= 2 => create_namespace(&args[1]),
        "--destroy" if args.len() >= 2 => destroy_namespace(&args[1]),
        "--status" if args.len() >= 2 => namespace_status(&args[1]),
        _ => list_namespaces(),
    }
}

fn list_namespaces() -> Result<Value, String> {
    let base = namespace_base_dir();
    if !base.exists() {
        return Ok(json!({
            "namespaces": [],
            "count": 0,
            "hint": "Create a namespace: cos checkpoint namespaces --create <name>",
        }));
    }

    let mut namespaces: Vec<Value> = Vec::new();
    let entries = fs::read_dir(&base).map_err(|e| format!("failed to read namespaces: {e}"))?;

    for entry in entries.flatten() {
        if !entry.path().is_dir() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        let upper = entry.path().join("upper");
        let cps = entry.path().join("checkpoints");
        let pending = if upper.exists() {
            count_files_in_upper(&upper)
        } else {
            0
        };
        let cp_count = if cps.exists() {
            fs::read_dir(&cps)
                .map(|e| e.filter_map(|e| e.ok()).filter(|e| e.path().is_dir()).count())
                .unwrap_or(0)
        } else {
            0
        };
        let used = if upper.exists() { dir_size(&upper) } else { 0 };

        namespaces.push(json!({
            "name": name,
            "pending_changes": pending,
            "checkpoints": cp_count,
            "used_bytes": used,
            "used_human": format_bytes(used),
        }));
    }

    namespaces.sort_by(|a, b| {
        let na = a["name"].as_str().unwrap_or("");
        let nb = b["name"].as_str().unwrap_or("");
        na.cmp(nb)
    });

    let count = namespaces.len();
    Ok(json!({
        "namespaces": namespaces,
        "count": count,
    }))
}

fn create_namespace(name: &str) -> Result<Value, String> {
    policy::require(OpType::System).map_err(|v| v.to_string())?;

    if !name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err("namespace name must be alphanumeric (hyphens/underscores allowed)".into());
    }

    let ns_dir = namespace_base_dir().join(name);
    if ns_dir.exists() {
        return Err(format!("namespace already exists: {name}"));
    }

    fs::create_dir_all(ns_dir.join("base"))
        .map_err(|e| format!("failed to create namespace: {e}"))?;
    fs::create_dir_all(ns_dir.join("upper"))
        .map_err(|e| format!("failed to create namespace: {e}"))?;
    fs::create_dir_all(ns_dir.join("work"))
        .map_err(|e| format!("failed to create namespace: {e}"))?;
    fs::create_dir_all(ns_dir.join("checkpoints"))
        .map_err(|e| format!("failed to create namespace: {e}"))?;

    Ok(json!({
        "created": name,
        "path": ns_dir.to_string_lossy(),
    }))
}

fn destroy_namespace(name: &str) -> Result<Value, String> {
    policy::require(OpType::System).map_err(|v| v.to_string())?;

    let ns_dir = namespace_base_dir().join(name);
    if !ns_dir.exists() {
        return Err(format!("namespace not found: {name}"));
    }

    fs::remove_dir_all(&ns_dir).map_err(|e| format!("failed to destroy namespace: {e}"))?;

    Ok(json!({
        "destroyed": name,
    }))
}

fn namespace_status(name: &str) -> Result<Value, String> {
    let ns_dir = namespace_base_dir().join(name);
    if !ns_dir.exists() {
        return Err(format!("namespace not found: {name}"));
    }

    let upper = ns_dir.join("upper");
    let cps = ns_dir.join("checkpoints");

    let pending = if upper.exists() {
        count_files_in_upper(&upper)
    } else {
        0
    };
    let upper_bytes = if upper.exists() { dir_size(&upper) } else { 0 };
    let cp_bytes = if cps.exists() { dir_size(&cps) } else { 0 };
    let cp_count = if cps.exists() {
        fs::read_dir(&cps)
            .map(|e| e.filter_map(|e| e.ok()).filter(|e| e.path().is_dir()).count())
            .unwrap_or(0)
    } else {
        0
    };

    Ok(json!({
        "namespace": name,
        "pending_changes": pending,
        "checkpoint_count": cp_count,
        "disk_usage": {
            "upper_bytes": upper_bytes,
            "upper_human": format_bytes(upper_bytes),
            "checkpoints_bytes": cp_bytes,
            "checkpoints_human": format_bytes(cp_bytes),
            "total_bytes": upper_bytes + cp_bytes,
            "total_human": format_bytes(upper_bytes + cp_bytes),
        },
    }))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    // -- Checkpoint ID generation --

    #[test]
    fn next_id_empty_dir() {
        let dir = std::env::temp_dir().join("cos-cp-test-empty");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();

        assert_eq!(next_checkpoint_id(&dir), "001");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn next_id_sequential() {
        let dir = std::env::temp_dir().join("cos-cp-test-seq");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();

        fs::create_dir_all(dir.join("001-first")).unwrap();
        fs::create_dir_all(dir.join("002-second")).unwrap();

        assert_eq!(next_checkpoint_id(&dir), "003");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn next_id_with_gap() {
        let dir = std::env::temp_dir().join("cos-cp-test-gap");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();

        fs::create_dir_all(dir.join("001-alpha")).unwrap();
        fs::create_dir_all(dir.join("005-beta")).unwrap();

        // Should be max + 1, not fill gaps.
        assert_eq!(next_checkpoint_id(&dir), "006");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn next_id_ignores_non_numeric() {
        let dir = std::env::temp_dir().join("cos-cp-test-nonnum");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();

        fs::create_dir_all(dir.join("not-a-number")).unwrap();
        fs::create_dir_all(dir.join("003-valid")).unwrap();

        assert_eq!(next_checkpoint_id(&dir), "004");

        let _ = fs::remove_dir_all(&dir);
    }

    // -- Meta serialization --

    #[test]
    fn meta_round_trip() {
        let meta = CheckpointMeta {
            id: "007".to_string(),
            description: "before refactoring".to_string(),
            created_at: "2026-03-23T21:45:00Z".to_string(),
            files_changed: 15,
        };

        let json_str = serde_json::to_string_pretty(&meta).unwrap();
        let parsed: CheckpointMeta = serde_json::from_str(&json_str).unwrap();

        assert_eq!(parsed.id, "007");
        assert_eq!(parsed.description, "before refactoring");
        assert_eq!(parsed.created_at, "2026-03-23T21:45:00Z");
        assert_eq!(parsed.files_changed, 15);
    }

    #[test]
    fn meta_json_has_expected_fields() {
        let meta = CheckpointMeta {
            id: "001".to_string(),
            description: "test".to_string(),
            created_at: "2026-01-01T00:00:00Z".to_string(),
            files_changed: 3,
        };

        let v: Value = serde_json::to_value(&meta).unwrap();
        assert!(v["id"].is_string());
        assert!(v["description"].is_string());
        assert!(v["created_at"].is_string());
        assert!(v["files_changed"].is_number());
    }

    // -- walk_upper categorisation --

    #[test]
    fn walk_upper_created_files() {
        let root = std::env::temp_dir().join("cos-cp-walk-created");
        let _ = fs::remove_dir_all(&root);

        let base_layer = root.join("base");
        let upper = root.join("upper");
        fs::create_dir_all(&base_layer).unwrap();
        fs::create_dir_all(&upper).unwrap();

        // File exists in upper but NOT in base → created.
        fs::write(upper.join("new.txt"), "hello").unwrap();

        let mut created = Vec::new();
        let mut modified = Vec::new();
        let mut deleted = Vec::new();

        walk_upper(&upper, &upper, &base_layer, &mut created, &mut modified, &mut deleted).unwrap();

        assert_eq!(created, vec!["new.txt"]);
        assert!(modified.is_empty());
        assert!(deleted.is_empty());

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn walk_upper_modified_files() {
        let root = std::env::temp_dir().join("cos-cp-walk-modified");
        let _ = fs::remove_dir_all(&root);

        let base_layer = root.join("base");
        let upper = root.join("upper");
        fs::create_dir_all(&base_layer).unwrap();
        fs::create_dir_all(&upper).unwrap();

        // File exists in both base AND upper → modified.
        fs::write(base_layer.join("existing.txt"), "original").unwrap();
        fs::write(upper.join("existing.txt"), "changed").unwrap();

        let mut created = Vec::new();
        let mut modified = Vec::new();
        let mut deleted = Vec::new();

        walk_upper(&upper, &upper, &base_layer, &mut created, &mut modified, &mut deleted).unwrap();

        assert!(created.is_empty());
        assert_eq!(modified, vec!["existing.txt"]);
        assert!(deleted.is_empty());

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn walk_upper_subdirectory() {
        let root = std::env::temp_dir().join("cos-cp-walk-subdir");
        let _ = fs::remove_dir_all(&root);

        let base_layer = root.join("base");
        let upper = root.join("upper");
        fs::create_dir_all(base_layer.join("src")).unwrap();
        fs::create_dir_all(upper.join("src")).unwrap();

        // Nested file: exists only in upper → created.
        fs::write(upper.join("src").join("lib.rs"), "fn main(){}").unwrap();

        let mut created = Vec::new();
        let mut modified = Vec::new();
        let mut deleted = Vec::new();

        walk_upper(&upper, &upper, &base_layer, &mut created, &mut modified, &mut deleted).unwrap();

        // Path separator may vary; just check the file name is present.
        assert_eq!(created.len(), 1);
        assert!(created[0].contains("lib.rs"));
        assert!(modified.is_empty());
        assert!(deleted.is_empty());

        let _ = fs::remove_dir_all(&root);
    }

    // -- sanitize_description --

    #[test]
    fn sanitize_basic() {
        assert_eq!(sanitize_description("before refactoring"), "before-refactoring");
    }

    #[test]
    fn sanitize_special_chars() {
        assert_eq!(sanitize_description("fix: tests & lints!"), "fix-tests-lints");
    }

    #[test]
    fn sanitize_empty() {
        assert_eq!(sanitize_description(""), "");
    }

    // -- count_files_in_upper --

    #[test]
    fn count_files_empty() {
        let dir = std::env::temp_dir().join("cos-cp-count-empty");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();

        assert_eq!(count_files_in_upper(&dir), 0);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn count_files_with_content() {
        let dir = std::env::temp_dir().join("cos-cp-count-files");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("sub")).unwrap();

        fs::write(dir.join("a.txt"), "a").unwrap();
        fs::write(dir.join("sub").join("b.txt"), "b").unwrap();

        assert_eq!(count_files_in_upper(&dir), 2);

        let _ = fs::remove_dir_all(&dir);
    }

    // -- dir_size --

    #[test]
    fn dir_size_basic() {
        let dir = std::env::temp_dir().join("cos-cp-dirsize");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();

        fs::write(dir.join("a.txt"), "hello").unwrap(); // 5 bytes

        let size = dir_size(&dir);
        assert!(size >= 5, "expected at least 5 bytes, got {size}");

        let _ = fs::remove_dir_all(&dir);
    }

    // -- copy_dir_recursive --

    #[test]
    fn copy_dir_recursive_works() {
        let root = std::env::temp_dir().join("cos-cp-copydir");
        let _ = fs::remove_dir_all(&root);

        let src = root.join("src");
        let dst = root.join("dst");
        fs::create_dir_all(src.join("sub")).unwrap();
        fs::write(src.join("a.txt"), "aaa").unwrap();
        fs::write(src.join("sub").join("b.txt"), "bbb").unwrap();

        copy_dir_recursive(&src, &dst).unwrap();

        assert_eq!(fs::read_to_string(dst.join("a.txt")).unwrap(), "aaa");
        assert_eq!(fs::read_to_string(dst.join("sub").join("b.txt")).unwrap(), "bbb");

        let _ = fs::remove_dir_all(&root);
    }

    // -- existing_ids --

    #[test]
    fn existing_ids_empty() {
        let dir = std::env::temp_dir().join("cos-cp-ids-empty");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();

        assert!(existing_ids(&dir).is_empty());

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn existing_ids_mixed() {
        let dir = std::env::temp_dir().join("cos-cp-ids-mixed");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();

        fs::create_dir_all(dir.join("002-foo")).unwrap();
        fs::create_dir_all(dir.join("010-bar")).unwrap();
        fs::create_dir_all(dir.join("readme")).unwrap(); // not numeric

        let mut ids = existing_ids(&dir);
        ids.sort();
        assert_eq!(ids, vec![2, 10]);

        let _ = fs::remove_dir_all(&dir);
    }

    // -- run dispatch --

    #[test]
    fn run_unknown_command() {
        let result = run("bogus", &[]);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("unknown checkpoint command"));
    }
}
