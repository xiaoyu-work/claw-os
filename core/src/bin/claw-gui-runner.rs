#[cfg(target_os = "linux")]
fn main() {
    use std::os::unix::process::CommandExt;

    if let Err(refusal) =
        cos::update::runtime::enforce_startup(cos::update::runtime::Scope::CompiledEpoch)
    {
        fail(&refusal.message);
    }
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    let marker = args.get(1).and_then(|value| value.to_str());
    if args.first().is_none_or(|value| value != "--launch-gate")
        || args.get(2).is_none_or(|value| value != "--")
        || args.len() < 4
        || !marker.is_some_and(|value| {
            value.len() == 32
                && value
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        })
    {
        fail("claw-gui-runner requires the Root GUI launch gate and a program");
    }
    let runner = std::env::current_exe()
        .unwrap_or_else(|error| fail(&format!("resolve GUI runner installation: {error}")))
        .with_file_name("claw-app-runner");
    if let Err(error) = cos::worker::gui_transport::initialize_runner() {
        fail(&error);
    }
    let error = std::process::Command::new(runner).args(args).exec();
    fail(&format!("exec gated OS App runner: {error}"));
}

#[cfg(not(target_os = "linux"))]
fn main() {
    fail("claw-gui-runner requires Linux");
}

fn fail(message: &str) -> ! {
    eprintln!("{message}");
    std::process::exit(1);
}
