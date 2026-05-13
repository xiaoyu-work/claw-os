mod agent;
mod apps;
mod audit;
mod bridge;
mod browser;
mod caps;
mod checkpoint;
mod config;
mod credential;
mod cron;
mod crypto;
mod engine_pkg;
pub mod errors;
mod filelock;
mod i18n;
mod ipc;
mod model;
mod netfilter;
mod paths;
mod policy;
mod proc;
mod router;
mod sandbox;
mod service;
mod sysinfo;
mod trace;
mod watch;

use std::env;
use std::process;

fn main() {
    let args: Vec<String> = env::args().skip(1).collect();

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
