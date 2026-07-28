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

        let config = Path::new("/etc/default/cos-home");
        if let Ok(existing) = fs::read_to_string(config) {
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
            }
        }
        let tmp = config.with_extension(format!("tmp.{}", std::process::id()));
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o644);
        }
        let mut file = options
            .open(&tmp)
            .map_err(|error| GreeterError::HomeSetup(format!("create {}: {error}", tmp.display())))?;
        writeln!(
            file,
            "# Written by cosmic-initial-setup after creating the first user.\nCOS_HOME={}",
            home.display()
        )
        .map_err(|error| GreeterError::HomeSetup(format!("write {}: {error}", tmp.display())))?;
        file.sync_all()
            .map_err(|error| GreeterError::HomeSetup(format!("sync {}: {error}", tmp.display())))?;
        fs::rename(&tmp, config)
            .map_err(|error| GreeterError::HomeSetup(format!("commit {}: {error}", config.display())))?;
        fs::File::open(config.parent().unwrap_or(Path::new("/etc/default")))
            .and_then(|directory| directory.sync_all())
            .map_err(|error| GreeterError::HomeSetup(format!("sync /etc/default: {error}")))?;

        let status = Command::new("/usr/bin/systemctl")
            .args(["restart", "cos-home-setup.service"])
            .status()
            .map_err(|error| GreeterError::HomeSetup(format!("start cos-home setup: {error}")))?;
        if !status.success() {
            return Err(GreeterError::HomeSetup(format!(
                "cos-home-setup.service exited with {}",
                status.code().unwrap_or(-1)
            )));
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
