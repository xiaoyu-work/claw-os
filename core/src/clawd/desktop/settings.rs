//! Fixed human Settings activation through the authenticated user's service
//! manager. The daemon and its request helper retain NoNewPrivileges; only the
//! independent desktop process can ask polkit for a genuine human decision.
use super::*;

pub(super) const PROGRAM: &str = "/usr/bin/cosmic-settings";

pub(super) async fn launch(
    args: Vec<String>,
    environment: DesktopEnvironment,
) -> Result<(), String> {
    validate_session_bus(&environment)?;
    let service_args = service_args(&args, &environment)?;
    let output = run_user_command(
        PathBuf::from("/usr/bin/systemd-run"),
        service_args,
        environment,
        LAUNCH_TIMEOUT,
    )
    .await?;
    if !output.status.success() {
        return Err(format!(
            "Settings user-session service unavailable or activation failed: {} ({})",
            tail(&output.stderr),
            output.status,
        ));
    }
    Ok(())
}

fn validate_session_bus(environment: &DesktopEnvironment) -> Result<(), String> {
    for relative in ["bus", "systemd/private"] {
        let path = environment.runtime_dir.join(relative);
        let metadata = fs::symlink_metadata(&path).map_err(|error| {
            format!(
                "Settings user-session service unavailable at {}: {error}",
                path.display()
            )
        })?;
        if !metadata.file_type().is_socket() || metadata.uid() != environment.uid {
            return Err(
                "Settings requires the authenticated owner's user-session bus and service manager"
                    .into(),
            );
        }
    }
    Ok(())
}

fn service_args(args: &[String], environment: &DesktopEnvironment) -> Result<Vec<String>, String> {
    let launch = configured_user_command(Path::new(PROGRAM), args, environment);
    let mut service = vec![
        "--user".into(),
        "--quiet".into(),
        "--collect".into(),
        "--service-type=exec".into(),
        "--expand-environment=no".into(),
        format!(
            "--unit=claw-settings-{}.service",
            uuid::Uuid::new_v4().simple()
        ),
        "--property=Description=Claw OS Settings".into(),
        "--property=TimeoutStartSec=25s".into(),
        // The start job observes immediate main-process failures without waiting
        // for the GUI's lifetime. systemd owns reaping and garbage collection.
        "--property=ExecStartPost=/usr/bin/sleep 0.2".into(),
        "--property=WorkingDirectory=/".into(),
        "--property=StandardInput=null".into(),
        "--property=StandardOutput=null".into(),
        "--property=StandardError=journal".into(),
        "--property=UMask=0077".into(),
        "--property=LimitCORE=0".into(),
        "--".into(),
        "/usr/bin/env".into(),
        "-i".into(),
    ];
    // Environment= alone would still inherit the user manager's environment.
    // Reuse the desktop allowlist, then clear everything else at the GUI exec.
    for (name, value) in launch.get_envs() {
        if let Some(value) = value {
            let name = name
                .to_str()
                .ok_or("desktop environment name must be UTF-8")?;
            let value = value
                .to_str()
                .ok_or("desktop environment value must be UTF-8")?;
            service.push(format!("{name}={value}"));
        }
    }
    service.push(PROGRAM.into());
    service.extend(args.iter().cloned());
    Ok(service)
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/clawd/desktop/settings.rs"
    ));
}
