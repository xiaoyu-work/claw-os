mod apps;
mod audit;
mod bridge;
mod router;
mod server;
mod sysinfo;

use std::env;
use std::process;

fn main() {
    let args: Vec<String> = env::args().skip(1).collect();

    // Server mode: `cos serve [--port PORT] [--host HOST]`
    if args.first().map(|s| s.as_str()) == Some("serve") {
        let mut host = "0.0.0.0".to_string();
        let mut port: u16 = 8080;
        let mut i = 1;
        while i < args.len() {
            match args[i].as_str() {
                "--port" if i + 1 < args.len() => {
                    port = args[i + 1].parse().unwrap_or(8080);
                    i += 2;
                }
                "--host" if i + 1 < args.len() => {
                    host = args[i + 1].clone();
                    i += 2;
                }
                _ => i += 1,
            }
        }
        let rt = tokio::runtime::Runtime::new().expect("failed to create tokio runtime");
        if let Err(e) = rt.block_on(server::serve(&host, port)) {
            eprintln!("[cos-api] fatal: {e}");
            process::exit(1);
        }
        return;
    }

    // CLI mode
    let result = router::dispatch(&args);

    match result {
        Ok(Some(output)) => {
            println!("{}", output);
        }
        Ok(None) => {}
        Err(e) => {
            let err = serde_json::json!({"error": e.to_string()});
            println!("{}", err);
            process::exit(1);
        }
    }
}
