//! Reviewed, exact Flatpak application installation. The resolver is libflatpak.

use serde::{Deserialize, Serialize};

pub(crate) const NOT_DISPATCHED: i32 = 2;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Scope {
    User,
    System,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Target {
    pub scope: Scope,
    remote: String,
    #[serde(rename = "ref")]
    reference: String,
}

impl Target {
    pub(crate) fn validate(&self) -> Result<(), String> {
        let parts: Vec<_> = self.reference.split('/').collect();
        let word = |value: &str| {
            !value.is_empty()
                && value.len() <= 128
                && value.as_bytes()[0].is_ascii_alphanumeric()
                && value
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte))
        };
        if !word(&self.remote)
            || parts.len() != 4
            || parts[0] != "app"
            || !parts[1].contains('.')
            || !parts[1..].iter().all(|part| word(part))
            || self.reference.len() > 255
        {
            return Err("Flatpak requires a named remote and a complete application ref".into());
        }
        Ok(())
    }

    pub(crate) fn capability(&self) -> crate::caps::Cap {
        let scope = if self.scope == Scope::User {
            "user"
        } else {
            "system"
        };
        crate::caps::Cap::new(
            crate::caps::Verb::SYS_PACKAGE,
            crate::caps::Scope::name(format!(
                "flatpak:{scope}:{}:{}",
                self.remote, self.reference
            )),
        )
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Plan {
    install: bool,
    remote_url: String,
    previous_commit: Option<String>,
    operations: Vec<Resolved>,
}

impl Plan {
    pub(crate) fn review_summary(&self) -> String {
        let title = if self.install {
            "Install Flatpak"
        } else {
            "Update Flatpak"
        };
        let mut summary = format!("{title} from {}. Exact deployments:", self.remote_url);
        for item in &self.operations {
            summary.push_str(&format!(
                "\n{} {} from {} ({})",
                item.operation, item.reference, item.remote_url, item.commit
            ));
        }
        summary
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct Resolved {
    remote: String,
    remote_url: String,
    reference: String,
    commit: String,
    operation: String,
}

#[cfg(target_os = "linux")]
mod linux {
    use super::*;
    use std::os::unix::process::CommandExt;
    use std::path::Path;
    use std::process::Stdio;
    use std::time::Duration;

    use serde_json::{json, Value};

    const PROVIDER: &str = include_str!("flatpak_transaction.py");

    pub(crate) async fn prepare(
        owner: u32,
        home: &Path,
        install: bool,
        target: &Target,
    ) -> Result<Plan, String> {
        let result = run(
            owner,
            home,
            target,
            json!({"mode": "plan", "install": install}),
            None,
        )
        .await?;
        serde_json::from_value(result)
            .map_err(|_| "Flatpak returned an invalid resolved plan".into())
    }

    pub(crate) async fn apply(
        authorized: &crate::clawd::authority::Authorized,
        id: &str,
        owner: u32,
        home: &Path,
        target: &Target,
        plan: &Plan,
    ) -> Result<Value, String> {
        if authorized.spent() != [target.capability()] {
            return Err("Flatpak was not authorized for the exact selected ref".into());
        }
        run(
            owner,
            home,
            target,
            json!({"mode": "apply", "plan": plan}),
            Some(id.into()),
        )
        .await?;
        verify(owner, home, target, plan).await
    }

    pub(crate) async fn verify(
        owner: u32,
        home: &Path,
        target: &Target,
        plan: &Plan,
    ) -> Result<Value, String> {
        run(
            owner,
            home,
            target,
            json!({"mode": "verify", "plan": plan}),
            None,
        )
        .await
    }

    pub(crate) async fn refresh(owner: u32, home: &Path) -> Result<Value, String> {
        static REFRESH: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
        let _refresh = REFRESH
            .try_lock()
            .map_err(|_| "Flatpak sources are already refreshing")?;
        let (user, system) = tokio::join!(
            run_helper(
                owner,
                home,
                Scope::User,
                json!({"mode": "refresh", "scope": Scope::User}),
                None
            ),
            run_helper(
                owner,
                home,
                Scope::System,
                json!({"mode": "refresh", "scope": Scope::System}),
                None
            ),
        );
        user?;
        system?;
        Ok(json!({"refreshed": true}))
    }

    pub(crate) async fn query(
        owner: u32,
        home: &Path,
        scope: Scope,
        mode: &str,
        cursor: Option<&str>,
    ) -> Result<Value, String> {
        run_helper(
            owner,
            home,
            scope,
            json!({"mode": mode, "scope": scope, "cursor": cursor}),
            None,
        )
        .await
    }

    async fn run(
        owner: u32,
        home: &Path,
        target: &Target,
        mut input: Value,
        tracked: Option<String>,
    ) -> Result<Value, String> {
        target.validate()?;
        input["target"] = json!(target);
        run_helper(owner, home, target.scope, input, tracked).await
    }

    async fn run_helper(
        owner: u32,
        home: &Path,
        scope: Scope,
        input: Value,
        tracked: Option<String>,
    ) -> Result<Value, String> {
        if owner == 0 || !home.is_absolute() {
            return Err("Flatpak requires a verified non-root desktop owner".into());
        }
        let mut bytes = serde_json::to_vec(&input).map_err(|error| error.to_string())?;
        bytes.push(b'\n');
        let catalog = input["mode"] == "catalog";
        let read_only = catalog
            || matches!(
                input["mode"].as_str(),
                Some("installed" | "updates" | "verify")
            );
        let unprivileged = scope == Scope::User || read_only;
        let home = if unprivileged {
            home.to_path_buf()
        } else {
            "/root".into()
        };
        let gid = crate::clawd::client_identity::owner_groups(owner)?.0;
        tokio::task::spawn_blocking(move || {
            use std::io::{BufRead, Write};

            let parent = std::process::id() as libc::pid_t;
            let mut command = std::process::Command::new("/usr/bin/python3");
            command
                .args(["-I", "-B", "-c", PROVIDER])
                .env_clear()
                .env("PATH", "/usr/sbin:/usr/bin:/sbin:/bin")
                .env("LANG", "C.UTF-8")
                .env("HOME", home)
                .current_dir("/")
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .process_group(0);
            unsafe {
                command.pre_exec(move || {
                    crate::agentd::spawn::mark_inherited_descriptors_cloexec_except(3, -1);
                    if libc::setgroups(0, std::ptr::null()) != 0
                        || unprivileged && (libc::setgid(gid) != 0 || libc::setuid(owner) != 0)
                        || libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) != 0
                        || libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL, 0, 0, 0) != 0
                        || libc::getppid() != parent
                    {
                        return Err(std::io::Error::last_os_error());
                    }
                    if !unprivileged {
                        // System deployments must be readable by desktop users,
                        // unlike the daemon's private state.
                        libc::umask(0o022);
                    }
                    if read_only {
                        let limit = libc::rlimit {
                            rlim_cur: 512 * 1024 * 1024,
                            rlim_max: 512 * 1024 * 1024,
                        };
                        if libc::setrlimit(libc::RLIMIT_AS, &limit) != 0 {
                            return Err(std::io::Error::last_os_error());
                        }
                    }
                    Ok(())
                });
            }
            // The child cannot begin a transaction until its PID has been retained.
            let mut child = command.spawn().map_err(|error| error.to_string())?;
            let pid = child.id();
            let run = (|| {
                let start = crate::proc::read_start_time_ticks_pub(pid)
                    .ok_or("Flatpak process identity is unavailable")?;
                if let Some(id) = &tracked {
                    crate::clawd::package_transactions::apt_started(id, pid, start)?;
                }
                child
                    .stdin
                    .take()
                    .expect("piped Flatpak input")
                    .write_all(&bytes)
                    .map_err(|error| error.to_string())?;
                let out = child.stdout.take().expect("piped Flatpak output");
                let err = child.stderr.take().expect("piped Flatpak errors");
                let output_limit = if catalog {
                    5 * 1024 * 1024
                } else {
                    1024 * 1024
                };
                let reader = std::thread::spawn(move || {
                    crate::worker::exec::read_bounded(out, output_limit)
                });
                let errors =
                    std::thread::spawn(move || crate::worker::exec::read_bounded(err, 64 * 1024));
                let timeout = if tracked.is_some() { 15 * 60 } else { 20 };
                let (status, timed_out) = crate::worker::exec::wait_bounded(
                    &mut child,
                    Some(Duration::from_secs(timeout)),
                    || unsafe {
                        libc::kill(-(pid as i32), libc::SIGKILL);
                    },
                )?;
                if let (Some(id), Some(code)) = (&tracked, status.code().filter(|_| !timed_out)) {
                    crate::clawd::package_transactions::apt_finished(id, pid, start, code)?;
                }
                let (stdout, overflow) = reader.join().map_err(|_| "Flatpak output was lost")?;
                let (stderr, _) = errors.join().map_err(|_| "Flatpak diagnostics were lost")?;
                if !status.success() || timed_out || overflow {
                    let message = stderr
                        .as_slice()
                        .lines()
                        .last()
                        .and_then(Result::ok)
                        .unwrap_or_default();
                    return Err(format!(
                        "Flatpak did not confirm the requested operation: {}",
                        message.chars().take(512).collect::<String>()
                    ));
                }
                serde_json::from_slice(&stdout).map_err(|_| "Invalid Flatpak outcome".into())
            })();
            if run.is_err() && child.try_wait().is_ok_and(|status| status.is_none()) {
                unsafe {
                    libc::kill(-(pid as i32), libc::SIGKILL);
                }
                let _ = child.wait();
            }
            run
        })
        .await
        .map_err(|error| error.to_string())?
    }
}

#[cfg(target_os = "linux")]
pub(crate) use linux::{apply, prepare, query, refresh, verify};
