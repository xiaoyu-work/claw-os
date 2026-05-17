use std::path::PathBuf;

use cos::clawd::{config, server};

fn main() {
    tracing_subscriber::fmt::init();

    let mut socket_path = None;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--socket" => {
                let Some(path) = args.next() else {
                    eprintln!("clawd: --socket requires a path");
                    std::process::exit(2);
                };
                socket_path = Some(PathBuf::from(path));
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
    };

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
  --socket PATH   Bind the daemon to PATH instead of the default runtime socket
  -h, --help      Show this help text
"
    );
}
