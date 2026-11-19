use std::ffi::OsStr;
use std::io::Write;
use std::path::Path;
use std::process::{Child, Command};
use std::time::{Duration, Instant};

pub use claw_display_control::session::Subscription;
use claw_display_control::wire::SessionEvent;
use claw_display_control::Error;

pub fn run() -> Result<(), Error> {
    let mut arguments = std::env::args_os().skip(1);
    let mode = arguments
        .next()
        .ok_or(Error::Protocol("expected --watch, --exec or --desktop"))?;
    if mode != "--watch" && mode != "--exec" && mode != "--desktop" {
        return Err(Error::Protocol("expected --watch, --exec or --desktop"));
    }
    let arguments: Vec<_> = arguments.collect();
    if mode == "--watch" {
        if !arguments.is_empty() {
            return Err(Error::Protocol("unexpected display watcher argument"));
        }
        let subscription = Subscription::connect()?;
        println!(
            "{}",
            serde_json::to_string(&SessionEvent::Ready {
                epoch: subscription.epoch(),
                wayland_display: subscription.display().to_string(),
            })
            .map_err(|_| Error::Protocol("display event encoding failed"))?
        );
        std::io::stdout().flush()?;
        while !subscription.ended(Instant::now() + Duration::from_secs(1))? {}
        return Ok(());
    }
    if mode == "--desktop" {
        if !arguments.is_empty() {
            return Err(Error::Protocol("unexpected desktop startup argument"));
        }
        return desktop_session(&Subscription::connect()?);
    }
    let (executable, arguments) = arguments
        .split_first()
        .ok_or(Error::Protocol("missing session entrypoint"))?;
    if executable.is_empty()
        || arguments.len() > 128
        || std::iter::once(executable)
            .chain(arguments)
            .any(|argument| {
                argument.as_encoded_bytes().len() > 4096 || argument.as_encoded_bytes().contains(&0)
            })
    {
        return Err(Error::Protocol("invalid session entrypoint arguments"));
    }
    let subscription = Subscription::connect()?;
    let mut child = session_command(&subscription, executable)
        .args(arguments)
        .spawn()?;
    wait_for_entrypoint(&subscription, &mut child)
}

fn session_command(subscription: &Subscription, executable: impl AsRef<OsStr>) -> Command {
    let mut command = Command::new(executable);
    command
        .env("WAYLAND_DISPLAY", subscription.display())
        .env("XDG_RUNTIME_DIR", subscription.runtime_dir())
        .env("XDG_SESSION_TYPE", "wayland")
        .env("DBUS_SESSION_BUS_ADDRESS", subscription.bus_address())
        .env_remove("WAYLAND_SOCKET")
        .env_remove("DISPLAY")
        .env_remove("XAUTHORITY")
        .env_remove("X_PRIVILEGED_WAYLAND_SOCKET")
        .env_remove("CLAW_DISPLAY_CONTROL_FD");
    command
}

fn desktop_session(subscription: &Subscription) -> Result<(), Error> {
    let command = |program: &str| {
        let mut command = session_command(subscription, program);
        command
            .env("XDG_CURRENT_DESKTOP", "Hyprland")
            .env("XDG_SESSION_DESKTOP", "Hyprland")
            .env("GDK_BACKEND", "wayland")
            .env("QT_QPA_PLATFORM", "wayland")
            .env("MOZ_ENABLE_WAYLAND", "1")
            .env_remove("COS_SESSION");
        command
    };
    if !command("/usr/bin/dbus-update-activation-environment")
        .args([
            "--systemd",
            "WAYLAND_DISPLAY",
            "XDG_CURRENT_DESKTOP",
            "XDG_SESSION_DESKTOP",
            "XDG_SESSION_TYPE",
        ])
        .status()?
        .success()
    {
        return Err(Error::Protocol(
            "could not initialize the authenticated desktop user environment",
        ));
    }
    let presenter = "/usr/libexec/claw-desktop-reviews";
    if !Path::new(presenter).try_exists()? {
        eprintln!("Graphical reviews are unavailable until the Desktop package is updated");
        return run_desktop(&command, subscription);
    }
    let mut reviews = command(presenter).spawn()?;
    let result = run_desktop(&command, subscription);
    let retired = stop_child(&mut reviews);
    result.and(retired)
}

fn run_desktop(
    command: &impl Fn(&str) -> Command,
    subscription: &Subscription,
) -> Result<(), Error> {
    if Path::new("/etc/cos/first-run").is_file() {
        let entrypoint = "/usr/libexec/claw-oobe";
        let mut setup = command(entrypoint).spawn()?;
        wait_for_entrypoint(subscription, &mut setup)?;
        if !command(entrypoint)
            .arg("--check-complete")
            .status()?
            .success()
        {
            return Err(Error::Protocol("first-run setup did not record completion"));
        }
    }
    let target = "claw-desktop-session.target";
    if !command("/usr/bin/systemctl")
        .args(["--user", "start", target])
        .status()?
        .success()
    {
        return Err(Error::Protocol(
            "could not start the desktop session target",
        ));
    }
    let result = (|| {
        let mut files = command("/usr/lib/cos/bin/claw-file-manager").spawn()?;
        let result = subscription.run_shell();
        stop_child(&mut files)?;
        result
    })();
    let stopped = command("/usr/bin/systemctl")
        .args(["--user", "stop", target])
        .status();
    result?;
    if !stopped?.success() {
        return Err(Error::Protocol("could not stop the desktop session target"));
    }
    Ok(())
}

fn stop_child(child: &mut Child) -> Result<(), Error> {
    if child.try_wait()?.is_none() {
        // The child is not reaped until this function observes its status.
        if unsafe { libc::kill(child.id() as i32, libc::SIGTERM) } != 0 {
            return Err(std::io::Error::last_os_error().into());
        }
    }
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(status) = child.try_wait()? {
            return if status.success() {
                Ok(())
            } else {
                Err(Error::Protocol("desktop helper did not exit cleanly"))
            };
        }
        if Instant::now() >= deadline {
            child.kill()?;
            child.wait()?;
            return Err(Error::Protocol("desktop helper retirement timed out"));
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

fn wait_for_entrypoint(subscription: &Subscription, child: &mut Child) -> Result<(), Error> {
    loop {
        if let Some(status) = child.try_wait()? {
            return if status.success() {
                Ok(())
            } else {
                Err(Error::Protocol("session entrypoint failed"))
            };
        }
        match subscription.ended(Instant::now() + Duration::from_millis(100)) {
            Ok(false) => {}
            result => {
                child.kill()?;
                child.wait()?;
                return match result {
                    Ok(_) => Err(Error::Protocol(
                        "authenticated display ended before the session entrypoint completed",
                    )),
                    Err(error) => Err(error),
                };
            }
        }
    }
}
