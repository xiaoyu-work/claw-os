//! Broker-validated task workspaces. A workspace selects process context; it
//! never grants filesystem capability.

use std::path::{Path, PathBuf};

pub const MAX_WORKSPACE_BYTES: usize = 4_096;

pub fn resolve(owner_uid: u32, requested: Option<&str>) -> Result<PathBuf, String> {
    let home = crate::paths::verified_home_for_uid(owner_uid)?;
    let candidate = match requested {
        None => home.clone(),
        Some(value)
            if !value.is_empty()
                && value.len() <= MAX_WORKSPACE_BYTES
                && !value.chars().any(char::is_control) =>
        {
            let path = Path::new(value);
            if path.is_absolute() {
                path.to_path_buf()
            } else {
                home.join(path)
            }
        }
        Some(_) => {
            return Err(
                "task workspace must be a nonempty, control-free path of at most 4096 bytes".into(),
            )
        }
    };
    let workspace = candidate
        .canonicalize()
        .map_err(|error| format!("canonicalize task workspace: {error}"))?;
    if !workspace.starts_with(&home) {
        return Err(format!(
            "task workspace {} escapes owner home {}",
            workspace.display(),
            home.display()
        ));
    }
    let metadata = workspace
        .metadata()
        .map_err(|error| format!("inspect task workspace: {error}"))?;
    if !metadata.is_dir() {
        return Err("task workspace is not a directory".into());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if metadata.uid() != owner_uid {
            return Err("task workspace is not owned by the task owner".into());
        }
    }
    if workspace.to_str().is_none() {
        return Err("task workspace is not valid UTF-8".into());
    }
    Ok(workspace)
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/agent/workspace.rs"
    ));
}
