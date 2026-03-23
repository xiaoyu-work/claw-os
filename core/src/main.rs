mod apps;
mod audit;
mod bridge;
mod router;
mod sysinfo;

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
