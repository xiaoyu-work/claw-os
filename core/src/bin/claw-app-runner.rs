#[cfg(unix)]
fn main() {
    use std::os::unix::process::CommandExt;

    let mut args = std::env::args_os().skip(1);
    if args.next().as_deref() != Some(std::ffi::OsStr::new("--")) {
        fail("usage: claw-app-runner -- PROGRAM [ARG...]");
    }
    let program = args
        .next()
        .unwrap_or_else(|| fail("claw-app-runner requires PROGRAM"));
    let argv = args.collect::<Vec<_>>();
    let app_id = std::env::var("COS_APP_ID")
        .ok()
        .filter(|value| !value.is_empty());

    for _ in 0..500 {
        if cos::proc::current_session_is_bound(app_id.as_deref()) {
            let error = std::process::Command::new(&program).args(&argv).exec();
            fail(&format!("failed to exec session entrypoint: {error}"));
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    fail("process session was not bound before launch timeout");
}

#[cfg(not(unix))]
fn main() {
    fail("claw-app-runner requires Unix");
}

fn fail(message: &str) -> ! {
    eprintln!("{message}");
    std::process::exit(1);
}
