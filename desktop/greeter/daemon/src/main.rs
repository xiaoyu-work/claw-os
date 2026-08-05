use color_eyre::eyre::Context;
use cosmic_greeter_daemon::{UserData, UserFilter};
use std::error::Error;
use std::ffi::CString;
use std::future::pending;
use std::fs;
use std::io::Write;
use std::path::Path;
use std::process::Command;
use std::{env, io};
use tracing::metadata::LevelFilter;
use tracing::warn;
use tracing_subscriber::prelude::*;
use tracing_subscriber::{EnvFilter, fmt};
use zbus::DBusError;
use zbus::connection::Builder;

//IMPORTANT: this function is critical to the security of this proxy. It must ensure that the
// callback is executed with the permissions of the specified user id. A good test is to see if
// the /etc/shadow file can be read with a non-root user, it should fail with EPERM.
fn run_as_user<F: FnOnce() -> T, T>(user: &pwd::Passwd, f: F) -> Result<T, io::Error> {
    use nix::unistd::{Gid, Uid, getgroups, initgroups, setegid, seteuid, setgroups};

    // Save root HOME
    let root_home_opt = env::var_os("HOME");

    // Save root groups
    let root_groups = getgroups().expect("failed to get root groups");

    // Switch to user HOME
    unsafe {
        env::set_var("HOME", &user.dir);
    }

    // Switch to user identity
    {
        let name_c = CString::new(&*user.name).expect("invalid username");
        initgroups(&name_c, Gid::from_raw(user.gid))
            .expect("failed to set user supplementary groups");
    }
    setegid(Gid::from_raw(user.gid)).expect("failed to set user gid");
    seteuid(Uid::from_raw(user.uid)).expect("failed to set user uid");

    let t = f();

    // Restore root identity
    seteuid(Uid::from_raw(0)).expect("failed to restore root uid");
    setegid(Gid::from_raw(0)).expect("failed to restore root gid");
    setgroups(&root_groups).expect("failed to restore root supplementary groups");

    // Restore root HOME
    match root_home_opt {
        Some(root_home) => unsafe {
            env::set_var("HOME", root_home);
        },
        None => unsafe {
            env::remove_var("HOME");
        },
    }

    Ok(t)
}

#[derive(DBusError, Debug)]
#[zbus(prefix = "com.clawos.Greeter")]
enum GreeterError {
    #[zbus(error)]
    ZBus(zbus::Error),
    Ron(String),
    RunAsUser(String),
    HomeSetup(String),
}

struct GreeterProxy;

fn sync_parent(path: &Path) -> Result<(), String> {
    fs::File::open(path.parent().unwrap_or(Path::new("/")))
        .and_then(|directory| directory.sync_all())
        .map_err(|error| format!("sync {}: {error}", path.display()))
}

fn write_config_atomic(path: &Path, contents: &[u8]) -> Result<(), String> {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    let tmp = path.with_extension(format!("tmp.{}.{nonce}", std::process::id()));
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o644);
    }
    let result = (|| {
        let mut file = options
            .open(&tmp)
            .map_err(|error| format!("create {}: {error}", tmp.display()))?;
        file.write_all(contents)
            .map_err(|error| format!("write {}: {error}", tmp.display()))?;
        file.sync_all()
            .map_err(|error| format!("sync {}: {error}", tmp.display()))?;
        fs::rename(&tmp, path)
            .map_err(|error| format!("commit {}: {error}", path.display()))?;
        sync_parent(path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

fn systemctl(action: &str) -> Result<(), String> {
    let status = Command::new("/usr/bin/systemctl")
        .args([action, "cos-home-setup.service"])
        .status()
        .map_err(|error| format!("{action} cos-home-setup.service: {error}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!(
            "systemctl {action} cos-home-setup.service exited with {}",
            status.code().unwrap_or(-1)
        ))
    }
}

fn is_overlay_mount(home: &Path) -> Result<bool, String> {
    let output = Command::new("/usr/bin/findmnt")
        .args(["-n", "-o", "TARGET,FSTYPE", "--mountpoint"])
        .arg(home)
        .output()
        .map_err(|error| format!("inspect mount {}: {error}", home.display()))?;
    if !output.status.success() {
        return Ok(false);
    }
    let output = String::from_utf8_lossy(&output.stdout);
    let mut fields = output.split_whitespace();
    let target = fields.next().unwrap_or_default();
    let fs_type = fields.next().unwrap_or_default();
    Ok(target == home.to_string_lossy().as_ref()
        && matches!(fs_type, "overlay" | "overlayfs"))
}

fn unmount_overlay(home: &Path) -> Result<(), String> {
    if !is_overlay_mount(home)? {
        return Ok(());
    }
    let status = Command::new("/usr/bin/umount")
        .arg(home)
        .status()
        .map_err(|error| format!("unmount {}: {error}", home.display()))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!(
            "umount {} exited with {}",
            home.display(),
            status.code().unwrap_or(-1)
        ))
    }
}

fn cleanup_overlay_state() -> Result<(), String> {
    for path in ["/var/lib/cos/overlay", "/run/cos-overlay"] {
        match fs::remove_dir_all(path) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(format!("remove {path}: {error}")),
        }
    }
    Ok(())
}

fn overlay_state_owned_by(uid: u32) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        ["/var/lib/cos/overlay/upper", "/var/lib/cos/overlay/base"]
            .into_iter()
            .filter_map(|path| fs::metadata(path).ok())
            .any(|metadata| metadata.uid() == uid)
    }
    #[cfg(not(unix))]
    {
        let _ = uid;
        false
    }
}

fn configured_home(config: &Path) -> Result<Option<String>, String> {
    let contents = match fs::read_to_string(config) {
        Ok(contents) => contents,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("read {}: {error}", config.display())),
    };
    Ok(contents.lines().find_map(|line| {
        line.trim()
            .strip_prefix("COS_HOME=")
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
    }))
}

fn transaction_owns_home_state(config: &Path, home: &Path) -> bool {
    configured_home(config)
        .ok()
        .flatten()
        .is_some_and(|value| value == home.to_string_lossy().as_ref())
        || is_overlay_mount(home).unwrap_or(false)
}

fn rollback_home_setup(
    config: &Path,
    previous: Option<&[u8]>,
    home: &Path,
    remove_overlay: bool,
) -> Result<(), String> {
    let stop_error = systemctl("stop").err();
    let unmount_error = remove_overlay.then(|| unmount_overlay(home).err()).flatten();
    let cleanup_error = remove_overlay
        .then(|| cleanup_overlay_state().err())
        .flatten();
    let restore_result = match previous {
        Some(contents) => write_config_atomic(config, contents),
        None => match fs::remove_file(config) {
            Ok(()) => sync_parent(config),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(format!("remove {}: {error}", config.display())),
        },
    };
    let mut errors = Vec::new();
    if let Some(error) = stop_error {
        errors.push(error);
    }
    if let Some(error) = unmount_error {
        errors.push(error);
    }
    if let Some(error) = cleanup_error {
        errors.push(error);
    }
    if let Err(error) = restore_result {
        errors.push(error);
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors.join("; "))
    }
}

#[zbus::interface(name = "com.clawos.Greeter")]
impl GreeterProxy {
    fn get_user_data(&mut self) -> Result<String, GreeterError> {
        let user_filter = UserFilter::new();

        // The pwd::Passwd method is unsafe (but not labelled as such) due to using global state (libc pwent functions).
        // To prevent issues, this should only be called once in the entire process space at a time
        let users: Vec<_> = /* unsafe */ {
             pwd::Passwd::iter()
                .filter(|user| user_filter.filter(user))
                .collect()
            };

        let mut user_datas = Vec::new();
        for user in users {
            let mut user_data = UserData::from(user.clone());

            //IMPORTANT: Assume the identity of the user to ensure we don't read user file data as root
            run_as_user(&user, || user_data.load_config_as_user())
                .map_err(|err| GreeterError::RunAsUser(err.to_string()))?;

            user_datas.push(user_data);
        }

        //TODO: is ron the best choice for passing around background data?
        ron::to_string(&user_datas).map_err(|err| GreeterError::Ron(err.to_string()))
    }

    fn initial_setup_end(&mut self, new_user: String) -> Result<(), GreeterError> {
        let user = pwd::Passwd::iter()
            .find(|user| user.name == new_user)
            .ok_or_else(|| GreeterError::HomeSetup(format!("unknown user `{new_user}`")))?;
        if user.uid < 1000 || user.uid >= 65534 {
            return Err(GreeterError::HomeSetup(format!(
                "refusing non-human uid {}",
                user.uid
            )));
        }
        let home = Path::new(&*user.dir);
        if !home.is_absolute() || !home.starts_with("/home") {
            return Err(GreeterError::HomeSetup(format!(
                "refusing unexpected home {}",
                home.display()
            )));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            let metadata = fs::metadata(home).map_err(|error| {
                GreeterError::HomeSetup(format!("inspect {}: {error}", home.display()))
            })?;
            if !metadata.is_dir() || metadata.uid() != user.uid {
                return Err(GreeterError::HomeSetup(format!(
                    "home {} is not owned by uid {}",
                    home.display(),
                    user.uid
                )));
            }
        }
        let overlay_was_mounted =
            is_overlay_mount(home).map_err(GreeterError::HomeSetup)?;

        let config = Path::new("/etc/default/cos-home");
        let previous = match fs::read(config) {
            Ok(contents) => Some(contents),
            Err(error) if error.kind() == io::ErrorKind::NotFound => None,
            Err(error) => {
                return Err(GreeterError::HomeSetup(format!(
                    "read {}: {error}",
                    config.display()
                )));
            }
        };
        let mut config_targets_user = false;
        if let Some(existing) = previous.as_deref() {
            let existing = String::from_utf8_lossy(existing);
            if let Some(current) = existing.lines().find_map(|line| {
                line.trim()
                    .strip_prefix("COS_HOME=")
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
            }) {
                if current != home.to_string_lossy() {
                    return Err(GreeterError::HomeSetup(format!(
                        "cos-home is already configured for {current}"
                    )));
                }
                config_targets_user = true;
            }
        }
        if config_targets_user && overlay_was_mounted {
            return Ok(());
        }
        let desired = format!(
            "# Written by cosmic-initial-setup after creating the first user.\nCOS_HOME={}\n",
            home.display()
        );
        if let Err(error) = write_config_atomic(config, desired.as_bytes()) {
            let rollback = rollback_home_setup(
                config,
                previous.as_deref(),
                home,
                !overlay_was_mounted && transaction_owns_home_state(config, home),
            );
            return Err(GreeterError::HomeSetup(match rollback {
                Ok(()) => error,
                Err(rollback) => format!("{error}; rollback failed: {rollback}"),
            }));
        }
        if let Err(error) = systemctl("restart") {
            let rollback = rollback_home_setup(
                config,
                previous.as_deref(),
                home,
                !overlay_was_mounted,
            );
            return Err(GreeterError::HomeSetup(match rollback {
                Ok(()) => error,
                Err(rollback) => format!("{error}; rollback failed: {rollback}"),
            }));
        }
        let overlay_error = match is_overlay_mount(home) {
            Ok(true) => None,
            Ok(false) => Some(format!(
                "cos-home-setup.service did not mount OverlayFS at {}",
                home.display()
            )),
            Err(error) => Some(error),
        };
        if let Some(error) = overlay_error {
            let rollback = rollback_home_setup(
                config,
                previous.as_deref(),
                home,
                !overlay_was_mounted,
            );
            return Err(GreeterError::HomeSetup(match rollback {
                Ok(()) => error,
                Err(rollback) => format!("{error}; rollback failed: {rollback}"),
            }));
        }
        Ok(())
    }

    fn initial_setup_abort(&mut self, new_user: String) -> Result<(), GreeterError> {
        let user = pwd::Passwd::iter()
            .find(|user| user.name == new_user)
            .ok_or_else(|| GreeterError::HomeSetup(format!("unknown user `{new_user}`")))?;
        if user.uid < 1000 || user.uid >= 65534 {
            return Err(GreeterError::HomeSetup(format!(
                "refusing non-human uid {}",
                user.uid
            )));
        }
        let home = Path::new(&*user.dir);
        if !home.is_absolute() || !home.starts_with("/home") {
            return Err(GreeterError::HomeSetup(format!(
                "refusing unexpected home {}",
                home.display()
            )));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            let metadata = fs::metadata(home).map_err(|error| {
                GreeterError::HomeSetup(format!("inspect {}: {error}", home.display()))
            })?;
            if !metadata.is_dir() || metadata.uid() != user.uid {
                return Err(GreeterError::HomeSetup(format!(
                    "home {} is not owned by uid {}",
                    home.display(),
                    user.uid
                )));
            }
        }
        let config = Path::new("/etc/default/cos-home");
        let current = match fs::read(config) {
            Ok(current) => current,
            Err(error) if error.kind() == io::ErrorKind::NotFound => Vec::new(),
            Err(error) => {
                return Err(GreeterError::HomeSetup(format!(
                    "read {}: {error}",
                    config.display()
                )));
            }
        };
        let current_text = String::from_utf8_lossy(&current);
        let configured = current_text.lines().find_map(|line| {
            line.trim()
                .strip_prefix("COS_HOME=")
                .map(str::trim)
                .filter(|value| !value.is_empty())
        });
        if let Some(configured) = configured
            && configured != home.to_string_lossy().as_ref()
        {
            return Err(GreeterError::HomeSetup(format!(
                "refusing to abort `{new_user}` while cos-home targets {configured}"
            )));
        }
        let targets_user = configured.is_some();
        let owns_overlay = targets_user
            || is_overlay_mount(home).map_err(GreeterError::HomeSetup)?
            || overlay_state_owned_by(user.uid);
        if owns_overlay {
            systemctl("stop").map_err(GreeterError::HomeSetup)?;
            unmount_overlay(home).map_err(GreeterError::HomeSetup)?;
            cleanup_overlay_state().map_err(GreeterError::HomeSetup)?;
        }
        if targets_user {
            let default = b"# /etc/default/cos-home -- reset after incomplete initial setup.\n\
# Leave COS_HOME unset until the first-user transaction commits.\n";
            write_config_atomic(config, default).map_err(GreeterError::HomeSetup)?;
        }
        Ok(())
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    color_eyre::install().wrap_err("failed to install color_eyre error handler")?;

    let trace = tracing_subscriber::registry();
    let env_filter = EnvFilter::builder()
        .with_default_directive(LevelFilter::WARN.into())
        .from_env_lossy();

    #[cfg(feature = "systemd")]
    if let Ok(journald) = tracing_journald::layer() {
        trace
            .with(journald)
            .with(env_filter)
            .try_init()
            .wrap_err("failed to initialize logger")?;
    } else {
        trace
            .with(fmt::layer())
            .with(env_filter)
            .try_init()
            .wrap_err("failed to initialize logger")?;
        warn!("failed to connect to journald")
    }

    #[cfg(not(feature = "systemd"))]
    trace
        .with(fmt::layer())
        .with(env_filter)
        .try_init()
        .wrap_err("failed to initialize logger")?;

    let _conn = Builder::system()?
        .name("com.clawos.Greeter")?
        .serve_at("/com/clawos/Greeter", GreeterProxy)?
        .build()
        .await?;

    pending::<()>().await;

    Ok(())
}
