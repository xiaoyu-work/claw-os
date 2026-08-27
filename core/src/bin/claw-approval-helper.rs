use cos::clawd::protocol::Request;
use cos::clawd::routes::Command;
use cos::clawd::{client, config};
use serde_json::json;

fn main() {
    if unsafe { libc::geteuid() } != 0 {
        fail("claw-approval-helper must be launched through pkexec");
    }

    let owner_uid = std::env::var("PKEXEC_UID")
        .ok()
        .and_then(|value| value.parse::<u32>().ok())
        .unwrap_or_else(|| fail("pkexec did not provide PKEXEC_UID"));

    let mut id = None;
    let mut decision = None;
    let mut duration = None;
    let mut note = None;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--id" => id = args.next(),
            "--decision" => decision = args.next(),
            "--duration" => duration = args.next(),
            "--note" => note = args.next(),
            "-h" | "--help" => {
                println!(
                    "usage: claw-approval-helper --id ID --decision approve|deny \
                     [--duration once|session|forever] [--note TEXT]"
                );
                return;
            }
            other => fail(&format!("unknown argument: {other}")),
        }
    }

    let id = id.unwrap_or_else(|| fail("--id is required"));
    let decision = decision.unwrap_or_else(|| fail("--decision is required"));
    if !matches!(decision.as_str(), "approve" | "deny") {
        fail("--decision must be approve or deny");
    }

    let mut params = json!({
        "id": id,
        "decision": decision,
    });
    if owner_uid != 0 {
        params["owner_uid"] = json!(owner_uid);
    }
    if let Some(duration) = duration {
        params["duration"] = json!(duration);
    }
    if let Some(note) = note {
        params["note"] = json!(note);
    }

    let request = Request::build(Command::PermissionDecide, params);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap_or_else(|err| fail(&format!("failed to create runtime: {err}")));
    let response = runtime
        .block_on(client::request(config::socket_path(), request))
        .unwrap_or_else(|err| fail(&err));
    if !response.ok {
        let message = response
            .error
            .map(|error| error.message)
            .unwrap_or_else(|| "clawd rejected the approval decision".to_string());
        fail(&message);
    }
    println!("{}", response.result.unwrap_or_else(|| json!({"ok": true})));
}

fn fail(message: &str) -> ! {
    eprintln!("{message}");
    std::process::exit(1);
}
