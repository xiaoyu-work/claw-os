use std::io::Read;

use clawd_client::system_review::{ReviewDecision, SystemReview, MAX_DECISION_BYTES};
use cos::clawd::protocol::Request;
use cos::clawd::routes::Command;
use cos::clawd::{client, config};
use serde_json::json;

fn main() {
    if unsafe { libc::geteuid() } != 0 {
        fail("claw-approval-helper must be launched through pkexec");
    }
    // A privileged helper reached through polkit: refuse to act when
    // this build is behind the security floor this system accepted.
    if let Err(refusal) =
        cos::update::runtime::enforce_startup(cos::update::runtime::Scope::CompiledEpoch)
    {
        fail(&refusal.message);
    }

    let owner_uid = std::env::var("PKEXEC_UID")
        .ok()
        .and_then(|value| value.parse::<u32>().ok())
        .unwrap_or_else(|| fail("pkexec did not provide PKEXEC_UID"));

    let mut id = None;
    let mut decision = None;
    let mut duration = None;
    let mut note = None;
    let mut system_review = false;
    let mut system_review_json = false;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--id" => set_option(&mut id, args.next(), &arg).unwrap_or_else(|error| fail(&error)),
            "--decision" => {
                set_option(&mut decision, args.next(), &arg).unwrap_or_else(|error| fail(&error))
            }
            "--duration" => {
                set_option(&mut duration, args.next(), &arg).unwrap_or_else(|error| fail(&error))
            }
            "--note" => {
                set_option(&mut note, args.next(), &arg).unwrap_or_else(|error| fail(&error))
            }
            "--system-review" if !system_review => system_review = true,
            "--system-review-json" if !system_review_json => system_review_json = true,
            "-h" | "--help" => {
                println!(
                    "usage: claw-approval-helper --id ID --decision approve|deny \
                     [--duration once|session|forever] [--note TEXT] [--system-review]\n\
                     or: claw-approval-helper --system-review-json < decision.json"
                );
                return;
            }
            other => fail(&format!("unknown argument: {other}")),
        }
    }

    let request = if system_review_json {
        if system_review
            || id.is_some()
            || decision.is_some()
            || duration.is_some()
            || note.is_some()
        {
            fail("--system-review-json cannot be combined with legacy decision arguments");
        }
        let review =
            read_review_decision(std::io::stdin().lock()).unwrap_or_else(|error| fail(&error));
        Request::build(
            Command::SystemReviewDecide,
            json!({"owner_uid": owner_uid, "review": review}),
        )
    } else {
        let id = id.unwrap_or_else(|| fail("--id is required"));
        let decision = decision.unwrap_or_else(|| fail("--decision is required"));
        if !matches!(decision.as_str(), "approve" | "deny") {
            fail("--decision must be approve or deny");
        }
        if system_review && (duration.is_some() || note.is_some()) {
            fail("system review confirmation does not accept capability grant duration or notes");
        }
        let mut params = json!({
            "id": id,
            "decision": decision,
            "owner_uid": owner_uid,
        });
        if let Some(duration) = duration {
            params["duration"] = json!(duration);
        }
        if let Some(note) = note {
            params["note"] = json!(note);
        }
        Request::build(
            if system_review {
                Command::SystemReviewDecide
            } else {
                Command::PermissionDecide
            },
            params,
        )
    };
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap_or_else(|err| fail(&format!("failed to create runtime: {err}")));
    let response = runtime
        .block_on(client::request(config::socket_path(), request))
        .unwrap_or_else(|err| fail(&err.to_string()));
    if !response.ok {
        let message = response
            .error
            .map(|error| error.message)
            .unwrap_or_else(|| "clawd rejected the approval decision".to_string());
        fail(&message);
    }
    let result = response
        .result
        .unwrap_or_else(|| fail("clawd approval response has no result"));
    if system_review_json || system_review {
        let review: SystemReview = serde_json::from_value(result.clone())
            .unwrap_or_else(|error| fail(&format!("invalid system review response: {error}")));
        review
            .validate()
            .unwrap_or_else(|error| fail(&error.to_string()));
    }
    println!("{result}");
}

fn read_review_decision(reader: impl Read) -> Result<ReviewDecision, String> {
    let mut bytes = Vec::new();
    reader
        .take(MAX_DECISION_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("read system review decision: {error}"))?;
    ReviewDecision::decode(&bytes).map_err(|error| error.to_string())
}

fn set_option(
    target: &mut Option<String>,
    value: Option<String>,
    flag: &str,
) -> Result<(), String> {
    if target.is_some() {
        return Err(format!("{flag} was supplied more than once"));
    }
    *target = Some(value.ok_or_else(|| format!("{flag} requires a value"))?);
    Ok(())
}

fn fail(message: &str) -> ! {
    eprintln!("{message}");
    std::process::exit(1);
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/bin/claw-approval-helper.rs"
    ));
}
