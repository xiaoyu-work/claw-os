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
    let isolated_session = (std::env::var("COS_EXTENSION_CHILD_ISOLATION").as_deref() == Ok("1"))
        .then(|| std::env::var("COS_SESSION").ok())
        .flatten();

    for _ in 0..500 {
        let bound = match isolated_session.as_deref() {
            Some(session_id) => {
                cos::proc::session_id_is_bound_for_app(session_id, app_id.as_deref())
            }
            None => cos::proc::current_session_is_bound(app_id.as_deref()),
        };
        if bound {
            #[cfg(target_os = "linux")]
            if unsafe { libc::prctl(libc::PR_SET_DUMPABLE, 0, 0, 0, 0) } != 0 {
                fail("failed to disable dumpability before extension exec");
            }
            let error = std::process::Command::new(&program).args(&argv).exec();
            fail(&format!("failed to exec session entrypoint: {error}"));
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    fail(&format!(
        "process session was not bound before launch timeout (session={}, proc_data={})",
        isolated_session.as_deref().unwrap_or("<none>"),
        std::env::var("COS_PROC_DATA_DIR").unwrap_or_else(|_| "<none>".to_string())
    ));
}

#[cfg(not(unix))]
fn main() {
    fail("claw-app-runner requires Unix");
}

fn fail(message: &str) -> ! {
    eprintln!("{message}");
    std::process::exit(1);
}
