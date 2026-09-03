#[cfg(unix)]
fn main() {
    use std::io::Read;

    if let Err(refusal) =
        cos::update::runtime::enforce_startup(cos::update::runtime::Scope::CompiledEpoch)
    {
        fail(&refusal.message);
    }
    let mut args = std::env::args_os().skip(1);
    let launch_gate = match args.next().as_deref() {
        Some(value) if value == std::ffi::OsStr::new("--") => None,
        Some(value) if value == std::ffi::OsStr::new("--launch-gate") => {
            let token = args
                .next()
                .unwrap_or_else(|| fail("claw-app-runner requires a launch-gate token"));
            let token = token
                .into_string()
                .unwrap_or_else(|_| fail("claw-app-runner launch-gate token is not UTF-8"));
            if token.len() != 32
                || !token
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
            {
                fail("claw-app-runner launch-gate token is invalid");
            }
            if args.next().as_deref() != Some(std::ffi::OsStr::new("--")) {
                fail("claw-app-runner launch gate must precede `--`");
            }
            Some(token)
        }
        _ => fail("usage: claw-app-runner [--launch-gate TOKEN] -- PROGRAM [ARG...]"),
    };
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

    if let Some(expected) = launch_gate.as_deref() {
        disable_dumpability();
        let mut received = [0u8; 32];
        if std::io::stdin().lock().read_exact(&mut received).is_err()
            || received != expected.as_bytes()
        {
            fail("single-call App launch was not authorized");
        }
        exec(&program, &argv);
    }

    for _ in 0..500 {
        let bound = match isolated_session.as_deref() {
            Some(session_id) => {
                cos::proc::session_id_is_bound_for_app(session_id, app_id.as_deref())
            }
            None => cos::proc::current_session_is_bound(app_id.as_deref()),
        };
        if bound {
            disable_dumpability();
            exec(&program, &argv);
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    fail(&format!(
        "process session was not bound before launch timeout (session={}, proc_data={})",
        isolated_session.as_deref().unwrap_or("<none>"),
        std::env::var("COS_PROC_DATA_DIR").unwrap_or_else(|_| "<none>".to_string())
    ));
}

#[cfg(unix)]
fn exec(program: &std::ffi::OsStr, argv: &[std::ffi::OsString]) -> ! {
    use std::os::unix::process::CommandExt;

    let error = std::process::Command::new(program).args(argv).exec();
    fail(&format!("failed to exec session entrypoint: {error}"));
}

fn disable_dumpability() {
    #[cfg(target_os = "linux")]
    if unsafe { libc::prctl(libc::PR_SET_DUMPABLE, 0, 0, 0, 0) } != 0 {
        fail("failed to disable dumpability before extension exec");
    }
}

#[cfg(not(unix))]
fn main() {
    fail("claw-app-runner requires Unix");
}

fn fail(message: &str) -> ! {
    eprintln!("{message}");
    std::process::exit(1);
}
