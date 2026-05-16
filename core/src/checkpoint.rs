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

use crate::caps::{require_or_json, Scope, Verb};

// ---------------------------------------------------------------------------
// Path helpers
// ---------------------------------------------------------------------------

fn overlay_dir() -> PathBuf {
    PathBuf::from(std::env::var("COS_DATA_DIR").unwrap_or_else(|_| "/var/lib/cos".into()))
        .join("overlay")
}

fn workspace_dir() -> PathBuf {
    PathBuf::from(std::env::var("WORKSPACE").unwrap_or_else(|_| "/workspace".into()))
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
        fs::create_dir_all(d).map_err(|e| format!("failed to create {}: {e}", d.display()))?;
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
    let max = existing_ids(checkpoints_dir).into_iter().max().unwrap_or(0);
    format!("{:03}", max + 1)
}

/// Acquire an exclusive create-lock for `checkpoints_dir`. Pure RAII:
/// the returned guard removes the sentinel on drop.
///
/// Without this, two parallel `cos checkpoint create` invocations
/// will both call `next_checkpoint_id` (TOCTOU), pick the same id,
/// and one of them will silently overwrite the other's freshly
/// created `meta.json` and layer.
struct CreateLockGuard {
    path: PathBuf,
}

impl Drop for CreateLockGuard {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

fn acquire_create_lock(checkpoints_dir: &Path) -> Result<CreateLockGuard, String> {
    use std::io::Write;
    fs::create_dir_all(checkpoints_dir)
        .map_err(|e| format!("failed to create checkpoints dir: {e}"))?;
    let lock_path = checkpoints_dir.join(".create.lock");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    loop {
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&lock_path)
        {
            Ok(mut f) => {
                let _ = write!(f, "{}", std::process::id());
                return Ok(CreateLockGuard {
                    path: lock_path.clone(),
                });
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                // Stale-lock reclaim: if the recorded pid is gone,
                // remove and retry.
                if let Ok(meta) = fs::metadata(&lock_path) {
                    let age = meta
                        .modified()
                        .ok()
                        .and_then(|m| m.elapsed().ok())
                        .unwrap_or_default();
                    let pid_str = fs::read_to_string(&lock_path).unwrap_or_default();
                    let pid: u32 = pid_str.trim().parse().unwrap_or(0);
                    let stale_by_pid = pid != 0
                        && !{
                            #[cfg(unix)]
                            {
                                let rc = unsafe { libc::kill(pid as i32, 0) };
                                rc == 0
                                    || std::io::Error::last_os_error().raw_os_error()
                                        == Some(libc::EPERM)
                            }
                            #[cfg(not(unix))]
                            {
                                false
                            }
                        };
                    let stale_by_age = age >= std::time::Duration::from_secs(60);
                    if stale_by_pid || stale_by_age {
                        let _ = fs::remove_file(&lock_path);
                        continue;
                    }
                }
                if std::time::Instant::now() >= deadline {
                    return Err(format!(
                        "timed out waiting for checkpoint create lock at {}",
                        lock_path.display()
                    ));
                }
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
            Err(e) => {
                return Err(format!(
                    "failed to acquire create lock {}: {e}",
                    lock_path.display()
                ))
            }
        }
    }
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
            // Skip hidden / sentinel entries — `.create.lock` and
            // any future staging directories must not influence id
            // allocation.
            if name.starts_with('.') {
                return None;
            }
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
            .map_err(|e| {
                format!(
                    "path {} is not under upper_root {}: {e}",
                    path.display(),
                    upper_root.display()
                )
            })?
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
    require_or_json(Verb::SYS_KERNEL, Scope::wild()).map_err(|v| v.to_string())?;

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

    // Take the exclusive create-lock for the lifetime of this
    // operation. Without it, two parallel callers race on
    // next_checkpoint_id and silently produce duplicates.
    let _lock = acquire_create_lock(&checkpoints_dir)?;

    // Allocate the id under the lock, then retry past any
    // pre-existing dir (e.g., a partial recover artifact). Use the
    // first id that does not currently exist on disk.
    let mut id_num = existing_ids(&checkpoints_dir).into_iter().max().unwrap_or(0) + 1;
    let slug = sanitize_description(&description);
    let (id, dir_name, cp_dir) = loop {
        let id_s = format!("{:03}", id_num);
        let dn = if slug.is_empty() {
            id_s.clone()
        } else {
            format!("{id_s}-{slug}")
        };
        let cp = checkpoints_dir.join(&dn);
        if !cp.exists() {
            break (id_s, dn, cp);
        }
        id_num += 1;
        if id_num > 999_999 {
            return Err("checkpoint id space exhausted".into());
        }
    };
    let cp_layer = cp_dir.join("layer");

    // Count files before we move.
    let files_changed = count_files_in_upper(&upper);

    // 1. Create the checkpoint directory.
    fs::create_dir_all(&cp_dir).map_err(|e| format!("failed to create checkpoint dir: {e}"))?;

    // 2. Write meta.json FIRST.
    //
    // This inverts the prior write order so a crash *before*
    // moving the upper leaves a meta-only directory (no layer),
    // which `cmd_list` now skips as incomplete. The prior order
    // (rename upper → cp_layer, THEN write meta) had two distinct
    // failure modes: a crash between rename and meta-write left a
    // layer with no meta (silently un-listable) AND a crash between
    // rename and the new-upper mkdir left the workspace with no
    // upper at all.
    let now = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();
    let meta = CheckpointMeta {
        id: id.clone(),
        description: description.clone(),
        created_at: now.clone(),
        files_changed,
    };
    let meta_json = serde_json::to_string_pretty(&meta)
        .map_err(|e| format!("failed to serialize meta: {e}"))?;
    crate::filelock::write_locked(&cp_dir.join("meta.json"), &meta_json)
        .map_err(|e| format!("failed to write meta.json: {e}"))?;

    // 3. Unmount the overlay (best-effort — may not be mounted).
    let _ = umount_overlay();

    // 4. Move current upper → checkpoint layer. If the upper
    //    didn't exist (fresh install), create an empty layer so
    //    cmd_list's both-exist invariant still holds.
    if upper.exists() {
        fs::rename(&upper, &cp_layer)
            .map_err(|e| format!("failed to move upper to checkpoint: {e}"))?;
    } else {
        fs::create_dir_all(&cp_layer)
            .map_err(|e| format!("failed to create empty checkpoint layer: {e}"))?;
    }

    // 5. Create fresh empty upper + work dir.
    fs::create_dir_all(&upper).map_err(|e| format!("failed to create fresh upper: {e}"))?;
    let work = overlay.join("work");
    let _ = fs::remove_dir_all(&work);
    fs::create_dir_all(&work).map_err(|e| format!("failed to create work dir: {e}"))?;

    // 6. Remount overlay (best-effort).
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
    require_or_json(Verb::DATA_LOG_READ, Scope::wild()).map_err(|v| v.to_string())?;

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

    walk_upper(
        &upper,
        &upper,
        &base_layer,
        &mut created,
        &mut modified,
        &mut deleted,
    )?;

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
    require_or_json(Verb::SYS_KERNEL, Scope::wild()).map_err(|v| v.to_string())?;

    let overlay = overlay_dir();
    let upper = overlay.join("upper");
    let checkpoints_dir = overlay.join("checkpoints");

    // VALIDATE FIRST. The previous order — wipe upper, *then* look up
    // the checkpoint — meant a user who typoed the checkpoint id
    // (`cos checkpoint rollback abc` when there is no `abc-…`)
    // destroyed all their uncommitted changes in `upper/` and then
    // got an error. Hoist the lookup so a typo is rejected before
    // anything mutates the filesystem. With no argument the caller
    // is explicitly resetting to base — that is still allowed and
    // does wipe `upper/`.
    let resolved_layer: Option<(String, PathBuf)> = match args.first() {
        Some(target_id) => {
            let cp_dir = find_checkpoint_dir(&checkpoints_dir, target_id)?;
            let layer = cp_dir.join("layer");
            if !layer.exists() {
                return Err(format!("checkpoint layer not found: {}", layer.display()));
            }
            Some((target_id.clone(), layer))
        }
        None => None,
    };

    // Count pending changes before rollback (purely informational).
    let changes_reverted = count_files_in_upper(&upper);

    // 1. Unmount overlay (best-effort).
    let _ = umount_overlay();

    // 2. Restore via stage-then-swap.
    //
    // Old order: rm -r upper, then copy layer → upper. A crash in
    // the gap between the rm and the copy left the workspace with
    // NO upper at all — i.e., all uncommitted changes destroyed
    // and the checkpoint contents not yet visible.
    //
    // New order: stage the new upper alongside the old one
    // (`upper.new`), copy the layer into the stage, then atomically
    // swap (`rename upper -> upper.old`, then `rename upper.new ->
    // upper`, then `rm upper.old`). The window where `upper` is
    // missing collapses to one fs::rename. For the no-id "reset to
    // base" path the stage is just an empty directory.
    let rolled_back_to = match resolved_layer {
        Some((target_id, layer)) => {
            let new_upper = overlay.join("upper.new");
            let old_upper = overlay.join("upper.old");
            // Remove leftover from any earlier botched run.
            let _ = fs::remove_dir_all(&new_upper);
            let _ = fs::remove_dir_all(&old_upper);
            copy_dir_recursive(&layer, &new_upper)
                .map_err(|e| format!("failed to stage checkpoint layer: {e}"))?;
            if upper.exists() {
                fs::rename(&upper, &old_upper)
                    .map_err(|e| format!("failed to move current upper aside: {e}"))?;
            }
            if let Err(e) = fs::rename(&new_upper, &upper) {
                // Try to put the original upper back so we don't
                // leave the workspace with no upper at all.
                if old_upper.exists() {
                    let _ = fs::rename(&old_upper, &upper);
                }
                return Err(format!("failed to swap staged upper into place: {e}"));
            }
            let _ = fs::remove_dir_all(&old_upper);
            target_id
        }
        None => {
            // No id → reset to base (empty upper). Same stage-and-swap
            // pattern: stage an empty dir, then swap.
            let new_upper = overlay.join("upper.new");
            let old_upper = overlay.join("upper.old");
            let _ = fs::remove_dir_all(&new_upper);
            let _ = fs::remove_dir_all(&old_upper);
            fs::create_dir_all(&new_upper)
                .map_err(|e| format!("failed to stage empty upper: {e}"))?;
            if upper.exists() {
                fs::rename(&upper, &old_upper)
                    .map_err(|e| format!("failed to move current upper aside: {e}"))?;
            }
            if let Err(e) = fs::rename(&new_upper, &upper) {
                if old_upper.exists() {
                    let _ = fs::rename(&old_upper, &upper);
                }
                return Err(format!("failed to swap staged upper into place: {e}"));
            }
            let _ = fs::remove_dir_all(&old_upper);
            "base".to_string()
        }
    };

    // 3. Recreate work dir.
    let work = overlay.join("work");
    let _ = fs::remove_dir_all(&work);
    fs::create_dir_all(&work).map_err(|e| format!("failed to create work dir: {e}"))?;

    // 4. Remount overlay (best-effort).
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
    let entries =
        fs::read_dir(checkpoints_dir).map_err(|e| format!("cannot read checkpoints dir: {e}"))?;

    let prefix = format!("{id}-");
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        // Hidden / sentinel dirs (`.create.lock`, staging dirs) are
        // never user-addressable checkpoints — refuse to resolve to
        // them even on a literal id match.
        if name.starts_with('.') {
            continue;
        }
        if name == id || name.starts_with(&prefix) {
            let p = entry.path();
            if p.is_dir() {
                return Ok(p);
            }
        }
    }
    Err(format!("checkpoint not found: {id}"))
}

/// Recursively copy a directory tree, preserving symbolic links
/// as symlinks (not following them).
///
/// Pre-fix this function used `src_path.is_dir()` (which follows
/// symlinks) and `fs::copy` (which reads + writes the *target*'s
/// bytes), so a symlink in the source tree would either:
///
/// * recurse into the link target if it was a directory (copying
///   data that lives outside `src`), or
/// * silently materialize the target's bytes as a regular file at
///   the link's path in `dst`.
///
/// Either way the checkpoint loses the link identity, and on
/// restore the directory layout no longer matches what was
/// captured. Switch to `fs::symlink_metadata` to inspect entries
/// without traversing links, and rebuild symlinks at the
/// destination with `std::os::unix::fs::symlink`.
fn copy_dir_recursive(src: &Path, dst: &Path) -> Result<(), String> {
    fs::create_dir_all(dst).map_err(|e| format!("mkdir {}: {e}", dst.display()))?;

    for entry in fs::read_dir(src).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        let src_path = entry.path();
        let dst_path = dst.join(entry.file_name());

        let metadata = fs::symlink_metadata(&src_path)
            .map_err(|e| format!("symlink_metadata {}: {e}", src_path.display()))?;
        let ft = metadata.file_type();

        if ft.is_symlink() {
            let target = fs::read_link(&src_path)
                .map_err(|e| format!("read_link {}: {e}", src_path.display()))?;
            #[cfg(unix)]
            {
                if let Err(e) = std::os::unix::fs::symlink(&target, &dst_path) {
                    return Err(format!(
                        "symlink {} → {}: {e}",
                        dst_path.display(),
                        target.display()
                    ));
                }
            }
            #[cfg(not(unix))]
            {
                return Err(format!(
                    "symlink {} → {}: symlink reconstruction unsupported on this platform",
                    dst_path.display(),
                    target.display()
                ));
            }
        } else if ft.is_dir() {
            copy_dir_recursive(&src_path, &dst_path)?;
        } else {
            fs::copy(&src_path, &dst_path).map_err(|e| {
                format!("copy {} → {}: {e}", src_path.display(), dst_path.display())
            })?;
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// cos checkpoint list
// ---------------------------------------------------------------------------

fn cmd_list(_args: &[String]) -> Result<Value, String> {
    require_or_json(Verb::DATA_LOG_READ, Scope::wild()).map_err(|v| v.to_string())?;

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
        let name = entry.file_name().to_string_lossy().to_string();
        // Skip hidden / sentinel entries (`.create.lock`, future
        // staging dirs, …).
        if name.starts_with('.') {
            continue;
        }
        let cp_path = entry.path();
        let meta_path = cp_path.join("meta.json");
        let layer_path = cp_path.join("layer");
        // An incomplete checkpoint (created mid-crash) is one
        // where meta.json or the layer/ subdir is missing. List
        // must hide those so rollback can't restore half a
        // checkpoint and so the count is meaningful.
        if !layer_path.is_dir() {
            continue;
        }
        if let Ok(Some(data)) = crate::filelock::read_locked(&meta_path) {
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
    require_or_json(Verb::DATA_LOG_READ, Scope::wild()).map_err(|v| v.to_string())?;

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
    let data = crate::filelock::read_locked(&path).ok()??;
    serde_json::from_str(&data).ok()
}

fn save_quota(cfg: &QuotaConfig) {
    if let Ok(data) = serde_json::to_string_pretty(cfg) {
        let _ = crate::filelock::write_locked(&quota_path(), &data);
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
    require_or_json(Verb::SYS_KERNEL, Scope::wild()).map_err(|v| v.to_string())?;

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
    require_or_json(Verb::DATA_LOG_READ, Scope::wild()).map_err(|v| v.to_string())?;

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
    PathBuf::from(std::env::var("COS_DATA_DIR").unwrap_or_else(|_| "/var/lib/cos".into()))
        .join("overlay-namespaces")
}

/// List all overlay namespaces.
///
/// Usage: cos checkpoint namespaces [--create <name>] [--destroy <name>] [--status <name>]
fn cmd_namespaces(args: &[String]) -> Result<Value, String> {
    require_or_json(Verb::DATA_LOG_READ, Scope::wild()).map_err(|v| v.to_string())?;

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
                .map(|e| {
                    e.filter_map(|e| e.ok())
                        .filter(|e| e.path().is_dir())
                        .count()
                })
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
    require_or_json(Verb::SYS_KERNEL, Scope::name(name)).map_err(|v| v.to_string())?;

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
    require_or_json(Verb::SYS_KERNEL, Scope::name(name)).map_err(|v| v.to_string())?;

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
            .map(|e| {
                e.filter_map(|e| e.ok())
                    .filter(|e| e.path().is_dir())
                    .count()
            })
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

    use std::sync::Once;
    static PERMS_INIT: Once = Once::new();
    fn perms_init() {
        PERMS_INIT.call_once(|| std::env::set_var("COS_PERMS_MODE", "permissive"));
    }
    use std::fs;

    // -- Checkpoint ID generation --

    #[test]
    fn next_id_empty_dir() {
        perms_init();
        let dir = std::env::temp_dir().join("cos-cp-test-empty");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();

        assert_eq!(next_checkpoint_id(&dir), "001");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn next_id_sequential() {
        perms_init();
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
        perms_init();
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
        perms_init();
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
        perms_init();
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
        perms_init();
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
        perms_init();
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

        walk_upper(
            &upper,
            &upper,
            &base_layer,
            &mut created,
            &mut modified,
            &mut deleted,
        )
        .unwrap();

        assert_eq!(created, vec!["new.txt"]);
        assert!(modified.is_empty());
        assert!(deleted.is_empty());

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn walk_upper_modified_files() {
        perms_init();
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

        walk_upper(
            &upper,
            &upper,
            &base_layer,
            &mut created,
            &mut modified,
            &mut deleted,
        )
        .unwrap();

        assert!(created.is_empty());
        assert_eq!(modified, vec!["existing.txt"]);
        assert!(deleted.is_empty());

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn walk_upper_subdirectory() {
        perms_init();
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

        walk_upper(
            &upper,
            &upper,
            &base_layer,
            &mut created,
            &mut modified,
            &mut deleted,
        )
        .unwrap();

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
        perms_init();
        assert_eq!(
            sanitize_description("before refactoring"),
            "before-refactoring"
        );
    }

    #[test]
    fn sanitize_special_chars() {
        perms_init();
        assert_eq!(
            sanitize_description("fix: tests & lints!"),
            "fix-tests-lints"
        );
    }

    #[test]
    fn sanitize_empty() {
        perms_init();
        assert_eq!(sanitize_description(""), "");
    }

    // -- count_files_in_upper --

    #[test]
    fn count_files_empty() {
        perms_init();
        let dir = std::env::temp_dir().join("cos-cp-count-empty");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();

        assert_eq!(count_files_in_upper(&dir), 0);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn count_files_with_content() {
        perms_init();
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
        perms_init();
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
        perms_init();
        let root = std::env::temp_dir().join("cos-cp-copydir");
        let _ = fs::remove_dir_all(&root);

        let src = root.join("src");
        let dst = root.join("dst");
        fs::create_dir_all(src.join("sub")).unwrap();
        fs::write(src.join("a.txt"), "aaa").unwrap();
        fs::write(src.join("sub").join("b.txt"), "bbb").unwrap();

        copy_dir_recursive(&src, &dst).unwrap();

        assert_eq!(fs::read_to_string(dst.join("a.txt")).unwrap(), "aaa");
        assert_eq!(
            fs::read_to_string(dst.join("sub").join("b.txt")).unwrap(),
            "bbb"
        );

        let _ = fs::remove_dir_all(&root);
    }

    /// Pre-fix: a symlink in the source tree was dereferenced —
    /// `is_dir()` followed links and `fs::copy` materialized the
    /// target's bytes. After: symlinks are preserved as symlinks
    /// at the destination, with their original target path.
    #[cfg(unix)]
    #[test]
    fn copy_dir_recursive_preserves_symlinks() {
        perms_init();
        let root = std::env::temp_dir().join("cos-cp-copydir-symlinks");
        let _ = fs::remove_dir_all(&root);

        let src = root.join("src");
        let dst = root.join("dst");
        fs::create_dir_all(&src).unwrap();

        // file -> file symlink with a relative target
        fs::write(src.join("real.txt"), "real-bytes").unwrap();
        std::os::unix::fs::symlink("real.txt", src.join("link-to-file")).unwrap();

        // file -> file symlink with an absolute target that points
        // *outside* src; the pre-fix code would happily inline its
        // bytes, leaking external content into the checkpoint.
        let outside = root.join("outside.txt");
        fs::write(&outside, "outside-bytes").unwrap();
        std::os::unix::fs::symlink(&outside, src.join("link-absolute")).unwrap();

        // dir -> dir symlink — pre-fix this would have triggered
        // recursive copy of the linked directory contents.
        fs::create_dir_all(src.join("real_dir")).unwrap();
        fs::write(src.join("real_dir").join("inside.txt"), "inside-bytes").unwrap();
        std::os::unix::fs::symlink("real_dir", src.join("link-to-dir")).unwrap();

        // Dangling symlink — must still round-trip as a dangling
        // symlink rather than failing the copy.
        std::os::unix::fs::symlink("nonexistent-target", src.join("link-dangling")).unwrap();

        copy_dir_recursive(&src, &dst).unwrap();

        // The real file should be copied as a regular file.
        let real_meta = fs::symlink_metadata(dst.join("real.txt")).unwrap();
        assert!(real_meta.file_type().is_file(), "real.txt must stay a file");

        // All four links must be symlinks at dst (NOT regular files).
        for name in ["link-to-file", "link-absolute", "link-to-dir", "link-dangling"] {
            let meta = fs::symlink_metadata(dst.join(name))
                .unwrap_or_else(|e| panic!("symlink_metadata {name}: {e}"));
            assert!(
                meta.file_type().is_symlink(),
                "{name} must be a symlink at dst, not {:?}",
                meta.file_type()
            );
        }

        // Targets must round-trip unchanged.
        assert_eq!(fs::read_link(dst.join("link-to-file")).unwrap(), Path::new("real.txt"));
        assert_eq!(fs::read_link(dst.join("link-absolute")).unwrap(), outside);
        assert_eq!(fs::read_link(dst.join("link-to-dir")).unwrap(), Path::new("real_dir"));
        assert_eq!(
            fs::read_link(dst.join("link-dangling")).unwrap(),
            Path::new("nonexistent-target")
        );

        // real_dir was a real directory, so it should still be a
        // real directory at dst with its contents intact.
        let dir_meta = fs::symlink_metadata(dst.join("real_dir")).unwrap();
        assert!(
            dir_meta.file_type().is_dir() && !dir_meta.file_type().is_symlink(),
            "real_dir must stay a real directory"
        );
        assert_eq!(
            fs::read_to_string(dst.join("real_dir").join("inside.txt")).unwrap(),
            "inside-bytes"
        );

        let _ = fs::remove_dir_all(&root);
    }

    // -- existing_ids --

    #[test]
    fn existing_ids_empty() {
        perms_init();
        let dir = std::env::temp_dir().join("cos-cp-ids-empty");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();

        assert!(existing_ids(&dir).is_empty());

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn existing_ids_mixed() {
        perms_init();
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
        perms_init();
        let result = run("bogus", &[]);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("unknown checkpoint command"));
    }

    // -- parse_size --

    #[test]
    fn parse_size_gigabytes() {
        perms_init();
        assert_eq!(parse_size("2G").unwrap(), 2 * 1024 * 1024 * 1024);
    }

    #[test]
    fn parse_size_megabytes() {
        perms_init();
        assert_eq!(parse_size("512M").unwrap(), 512 * 1024 * 1024);
    }

    #[test]
    fn parse_size_kilobytes() {
        perms_init();
        assert_eq!(parse_size("100K").unwrap(), 100 * 1024);
    }

    #[test]
    fn parse_size_bytes() {
        perms_init();
        assert_eq!(parse_size("1024").unwrap(), 1024);
    }

    #[test]
    fn parse_size_invalid() {
        perms_init();
        assert!(parse_size("abc").is_err());
    }

    // -- format_bytes --

    #[test]
    fn format_bytes_gb() {
        perms_init();
        let s = format_bytes(2 * 1024 * 1024 * 1024);
        assert!(s.contains("G"));
    }

    #[test]
    fn format_bytes_mb() {
        perms_init();
        let s = format_bytes(100 * 1024 * 1024);
        assert!(s.contains("M"));
    }

    // -- quota --

    // Quota and namespace tests share a single COS_DATA_DIR (set via Once)
    // and use a Mutex to serialize because they share global state (quota.json,
    // namespace dirs).
    use std::sync::Mutex;
    static CP_INIT: Once = Once::new();
    static CP_LOCK: Mutex<()> = Mutex::new(());

    fn cp_setup() -> std::sync::MutexGuard<'static, ()> {
        let guard = CP_LOCK.lock().unwrap();
        CP_INIT.call_once(|| {
            let dir = std::env::temp_dir().join(format!("cos-test-shared-{}", std::process::id()));
            let _ = fs::create_dir_all(&dir);
            std::env::set_var("COS_DATA_DIR", &dir);
        });
        std::env::remove_var("COS_SESSION");
        guard
    }

    #[test]
    fn quota_set_and_status() {
        perms_init();
        let _g = cp_setup();

        let r = cmd_quota_set(&vec!["1G".into()]).unwrap();
        assert_eq!(r["quota_set"], true);

        let r = cmd_quota_status(&vec![]).unwrap();
        assert_eq!(r["quota_enabled"], true);
        assert_eq!(r["exceeded"], false);
    }

    // -- namespaces --

    #[test]
    fn namespace_create_list_destroy() {
        perms_init();
        let _g = cp_setup();
        let ns_name = format!("test-ns-{}", std::process::id());

        let r = create_namespace(&ns_name).unwrap();
        assert_eq!(r["created"], ns_name);

        let r = list_namespaces().unwrap();
        assert!(r["count"].as_u64().unwrap() >= 1);

        let r = namespace_status(&ns_name).unwrap();
        assert_eq!(r["namespace"], ns_name);
        assert_eq!(r["pending_changes"], 0);

        let r = destroy_namespace(&ns_name).unwrap();
        assert_eq!(r["destroyed"], ns_name);
    }

    #[test]
    fn namespace_invalid_name() {
        perms_init();
        std::env::remove_var("COS_SESSION");
        let r = create_namespace("bad/name");
        assert!(r.is_err());
    }

    // -- rollback id validation --

    /// Regression: rollback with an unknown id must reject the
    /// command BEFORE wiping `upper/`. Pre-fix the function counted
    /// pending changes, unmounted overlay, and removed upper/ at
    /// step 2 — long before it tried to resolve the checkpoint id at
    /// step 3. A user who typoed `cos checkpoint rollback abc`
    /// destroyed all their uncommitted work and got an error.
    #[test]
    fn rollback_invalid_id_does_not_wipe_upper() {
        perms_init();
        let _g = cp_setup();

        let overlay = overlay_dir();
        let upper = overlay.join("upper");
        let checkpoints = overlay.join("checkpoints");
        let _ = fs::remove_dir_all(&upper);
        let _ = fs::remove_dir_all(&checkpoints);
        fs::create_dir_all(&upper).unwrap();
        fs::create_dir_all(&checkpoints).unwrap();

        // Seed upper/ with a sentinel file that MUST survive the
        // failed rollback. This file represents the user's
        // uncommitted work.
        let sentinel = upper.join("uncommitted_work.txt");
        fs::write(&sentinel, b"do not destroy me").unwrap();

        // Seed at least one valid checkpoint so the checkpoints dir
        // isn't empty (catches a different code path).
        fs::create_dir_all(checkpoints.join("001-real/layer")).unwrap();

        // Attempt rollback with a bogus id. Must return Err.
        let res = cmd_rollback(&vec!["this-id-does-not-exist".to_string()]);
        assert!(res.is_err(), "expected Err for unknown checkpoint id, got {res:?}");

        // The sentinel MUST still exist — proof we validated before
        // touching upper/. Pre-fix this assertion would fail.
        assert!(
            sentinel.exists(),
            "upper/ was wiped despite invalid id; uncommitted work lost"
        );
        let body = fs::read_to_string(&sentinel).unwrap();
        assert_eq!(body, "do not destroy me");
    }

    /// Companion case: rollback with NO id is the explicit
    /// "reset to base" command and IS allowed to wipe `upper/`.
    /// This must still work after the validation hoist.
    #[test]
    fn rollback_no_id_resets_upper_to_base() {
        perms_init();
        let _g = cp_setup();

        let overlay = overlay_dir();
        let upper = overlay.join("upper");
        let checkpoints = overlay.join("checkpoints");
        let _ = fs::remove_dir_all(&upper);
        let _ = fs::remove_dir_all(&checkpoints);
        fs::create_dir_all(&upper).unwrap();
        fs::create_dir_all(&checkpoints).unwrap();

        fs::write(upper.join("scratch.txt"), b"x").unwrap();
        let res = cmd_rollback(&vec![]).unwrap();
        assert_eq!(res["rolled_back_to"], "base");
        assert!(upper.exists(), "upper should be re-created empty");
        assert!(!upper.join("scratch.txt").exists(), "scratch should be gone");
    }

    /// Crashes in the middle of cmd_create must NOT pollute the
    /// checkpoint list with half-built entries. Specifically, a
    /// directory whose meta.json was written but whose `layer/`
    /// did not survive the rename, OR a directory whose `layer/`
    /// exists but whose meta.json was never written, must be
    /// invisible to `cmd_list` and `find_checkpoint_dir`.
    #[test]
    fn create_is_atomic_on_crash() {
        perms_init();
        let _g = cp_setup();

        let overlay = overlay_dir();
        let checkpoints = overlay.join("checkpoints");
        let _ = fs::remove_dir_all(&checkpoints);
        fs::create_dir_all(&checkpoints).unwrap();

        // 1) A complete checkpoint — should be visible.
        let good = checkpoints.join("001-good");
        fs::create_dir_all(good.join("layer")).unwrap();
        let meta = CheckpointMeta {
            id: "001".to_string(),
            description: "good".to_string(),
            created_at: "2026-01-01T00:00:00Z".to_string(),
            files_changed: 0,
        };
        fs::write(
            good.join("meta.json"),
            serde_json::to_string_pretty(&meta).unwrap(),
        )
        .unwrap();

        // 2) Crash AFTER meta.json was written but BEFORE the
        //    upper-rename completed: meta.json present, layer
        //    missing.
        let no_layer = checkpoints.join("002-no-layer");
        fs::create_dir_all(&no_layer).unwrap();
        let meta2 = CheckpointMeta {
            id: "002".to_string(),
            description: "no-layer".to_string(),
            created_at: "2026-01-01T00:00:00Z".to_string(),
            files_changed: 0,
        };
        fs::write(
            no_layer.join("meta.json"),
            serde_json::to_string_pretty(&meta2).unwrap(),
        )
        .unwrap();

        // 3) Legacy crash from the old write-meta-last code path:
        //    layer present, meta.json missing.
        let no_meta = checkpoints.join("003-no-meta");
        fs::create_dir_all(no_meta.join("layer")).unwrap();

        // 4) A hidden sentinel — the create-lock file. Must NEVER
        //    be reported.
        fs::write(checkpoints.join(".create.lock"), b"99999").unwrap();

        // cmd_list must only surface the complete checkpoint.
        let list = cmd_list(&vec![]).unwrap();
        let arr = list["checkpoints"].as_array().unwrap();
        let ids: Vec<&str> = arr.iter().map(|c| c["id"].as_str().unwrap()).collect();
        assert_eq!(ids, vec!["001"], "list must hide partial checkpoints");
        assert_eq!(list["count"], 1);

        // find_checkpoint_dir must refuse the sentinel even if
        // someone passes its literal name.
        let err = find_checkpoint_dir(&checkpoints, ".create.lock").unwrap_err();
        assert!(
            err.contains("not found"),
            "must not resolve to the create-lock sentinel: {err}"
        );

        // existing_ids must ignore the sentinel — next_checkpoint_id
        // proceeds as 003 + 1 = 004 (NOT some giant value derived
        // from the sentinel's filename).
        assert_eq!(next_checkpoint_id(&checkpoints), "004");
    }
}
