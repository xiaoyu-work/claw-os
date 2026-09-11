use std::io::Write;
use std::process::Command;
use std::time::{Duration, Instant};

pub use claw_display_control::session::Subscription;
use claw_display_control::wire::SessionEvent;
use claw_display_control::Error;

pub fn run() -> Result<(), Error> {
    let mut arguments = std::env::args_os().skip(1);
    let mode = arguments
        .next()
        .ok_or(Error::Protocol("expected --watch or --exec"))?;
    if mode != "--watch" && mode != "--exec" {
        return Err(Error::Protocol("expected --watch or --exec"));
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
    let mut child = Command::new(executable)
        .args(arguments)
        .env("WAYLAND_DISPLAY", subscription.display())
        .env("XDG_RUNTIME_DIR", subscription.runtime_dir())
        .env("XDG_SESSION_TYPE", "wayland")
        .env("DBUS_SESSION_BUS_ADDRESS", subscription.bus_address())
        .env_remove("WAYLAND_SOCKET")
        .env_remove("DISPLAY")
        .env_remove("XAUTHORITY")
        .env_remove("X_PRIVILEGED_WAYLAND_SOCKET")
        .env_remove("COSMIC_SESSION_SOCK")
        .env_remove("CLAW_DISPLAY_CONTROL_FD")
        .spawn()?;
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
