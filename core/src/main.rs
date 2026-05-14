mod agent;
mod ai;
mod apps;
mod approvals;
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
mod perms;
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
            // If a primitive returned a structured JSON error envelope as
            // its Err string (e.g. `{"error":"agent not configured",
            // "fix":"cos agent setup"}`), surface it as-is instead of
            // re-wrapping it in another `{"error":"..."}` layer. That
            // double-encoding made the structured fields invisible to
            // both humans and `jq` consumers.
            let err = match serde_json::from_str::<serde_json::Value>(&e) {
                Ok(v) if v.is_object() => v,
                _ => serde_json::json!({"error": e.to_string()}),
            };
            println!("{}", err);
            process::exit(1);
        }
    }
}
