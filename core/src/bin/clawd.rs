use std::path::PathBuf;

use cos::clawd::{config, server};

fn main() {
    // Declare this the privileged broker before anything else can pull
    // a provider client, MCP session, App launch or model-visible tool
    // registry into the root address space. `agentd::guard` turns each
    // of those surfaces into a hard error from here on.
    cos::agentd::guard::mark_broker_process();
    cos::storage::set_private_umask();
    let raw_args = std::env::args().skip(1).collect::<Vec<_>>();
    if raw_args
        .first()
        .is_some_and(|arg| arg == "--desktop-wayland-helper")
    {
        match cos::clawd::desktop_wayland::helper(&raw_args[1..]) {
            Ok(value) => {
                println!("{value}");
                return;
            }
            Err(error) => {
                eprintln!("{error}");
                std::process::exit(1);
            }
        }
    }
    if raw_args
        .first()
        .is_some_and(|arg| arg == "--a11y-wayland-helper")
    {
        match cos::clawd::a11y_wayland::helper(&raw_args[1..]) {
            Ok(value) => {
                println!("{value}");
                return;
            }
            Err(error) => {
                eprintln!("{error}");
                std::process::exit(1);
            }
        }
    }
    if raw_args
        .first()
        .is_some_and(|arg| arg == "--location-helper")
    {
        match cos::clawd::location::helper(&raw_args[1..]) {
            Ok(value) => {
                println!("{value}");
                return;
            }
            Err(error) => {
                eprintln!("{error}");
                std::process::exit(1);
            }
        }
    }
    tracing_subscriber::fmt::init();

    let mut socket_path = None;
    let mut socket_mode = None;
    let mut args = raw_args.into_iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--socket" => {
                let Some(path) = args.next() else {
                    eprintln!("clawd: --socket requires a path");
                    std::process::exit(2);
                };
                socket_path = Some(PathBuf::from(path));
            }
            "--socket-mode" => {
                let Some(mode) = args.next() else {
                    eprintln!("clawd: --socket-mode requires an octal mode");
                    std::process::exit(2);
                };
                let parsed =
                    u32::from_str_radix(mode.trim_start_matches("0o"), 8).unwrap_or_else(|_| {
                        eprintln!("clawd: invalid --socket-mode `{mode}`");
                        std::process::exit(2);
                    });
                socket_mode = Some(parsed);
            }
            "-h" | "--help" => {
                print_help();
                return;
            }
            other => {
                eprintln!("clawd: unknown argument: {other}");
                print_help();
                std::process::exit(2);
            }
        }
    }

    let options = server::ServerOptions {
        socket_path: socket_path.unwrap_or_else(config::socket_path),
        socket_mode: socket_mode.unwrap_or_else(config::socket_mode),
    };

    if let Err(err) = cos::storage::harden_clawd_state() {
        eprintln!("clawd: failed to secure persistent state: {err}");
        std::process::exit(1);
    }

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("failed to create tokio runtime");

    if let Err(err) = runtime.block_on(server::run(options)) {
        eprintln!("clawd: {err}");
        std::process::exit(1);
    }
}

fn print_help() {
    println!(
        "\
clawd — Claw OS agent daemon

Usage:
  clawd [--socket PATH]

Options:
  --socket PATH       Bind the daemon to PATH instead of the default system socket
  --socket-mode MODE  chmod the socket to octal MODE after bind (default: 0600)
  -h, --help          Show this help text
"
    );
}
