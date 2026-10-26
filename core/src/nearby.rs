//! Human terminal adapter to the same nearby sharing service as every UI.

use crate::clawd::routes::Command;
use clawd_client::nearby::*;
use serde_json::{json, Value};

pub fn run(command: &str, args: &[String]) -> Result<Value, String> {
    if command == "status" || command == "doctor" {
        if !args.is_empty() {
            return Err("status accepts no owner or state selectors".into());
        }
        return crate::operations::cli::request(Command::NearbyStatus, json!({}));
    }
    if command == "watch" {
        return watch(args);
    }
    let view: NearbyView = serde_json::from_value(crate::operations::cli::request(
        Command::NearbyStatus,
        json!({}),
    )?)
    .map_err(|e| format!("invalid nearby view: {e}"))?;
    let action = parse(command, args, &view)?;
    action.validate()?;
    crate::router::request_with_scheduler_consent(Command::NearbyControl, json!({"request":action}))
}

pub(crate) fn parse(
    command: &str,
    args: &[String],
    view: &NearbyView,
) -> Result<NearbyAction, String> {
    let session = || {
        view.session
            .as_ref()
            .filter(|session| matches!(session.phase, SessionPhase::Ready | SessionPhase::Starting))
            .map(|session| session.id.clone())
            .ok_or("Open a nearby window first; no stale session is resumed.".to_string())
    };
    let one = || {
        if args.len() == 1 {
            Ok(args[0].clone())
        } else {
            Err(format!("cos share {command} requires one exact identifier"))
        }
    };
    match command {
        "open" | "receive" => {
            let mut duration = 300;
            let mut disruptive = false;
            let mut seen = std::collections::HashSet::new();
            let mut index = 0;
            while let Some(arg) = args.get(index) {
                if !seen.insert(arg.as_str()) {
                    return Err("repeated share flag".into());
                }
                match arg.as_str() {
                    "--allow-wifi-disruption" => disruptive = true,
                    "--seconds" => {
                        index += 1;
                        duration = args
                            .get(index)
                            .ok_or("--seconds requires a value")?
                            .parse()
                            .map_err(|_| "invalid duration")?;
                    }
                    _ => return Err(format!("unknown share flag: {arg}")),
                }
                index += 1;
            }
            Ok(NearbyAction::Open {
                duration_secs: duration,
                receive: command == "receive",
                expected_mode: view
                    .probe
                    .mode
                    .ok_or("No compatible physical radio is available")?,
                allow_disruption: disruptive,
            })
        }
        "browse" => {
            if !args.is_empty() {
                return Err("browse accepts no arguments".into());
            }
            Ok(NearbyAction::Discover {
                session_id: session()?,
            })
        }
        "send" => {
            if args.len() < 2 {
                return Err("usage: cos share send PEER_ID FILE [FILE ...]".into());
            }
            let mut paths = Vec::new();
            for raw in &args[1..] {
                paths.push(
                    std::path::absolute(raw)
                        .map_err(|e| e.to_string())?
                        .to_str()
                        .ok_or("paths must be UTF-8")?
                        .to_string(),
                );
            }
            Ok(NearbyAction::Send {
                session_id: session()?,
                peer_id: args[0].clone(),
                paths,
                uploads: Vec::new(),
            })
        }
        "accept" | "decline" => Ok(NearbyAction::Decide {
            session_id: session()?,
            offer_id: one()?,
            accept: command == "accept",
        }),
        "cancel" => Ok(NearbyAction::Cancel {
            session_id: session()?,
            transfer_id: one()?,
        }),
        "stop" => {
            if !args.is_empty() {
                return Err("stop accepts no arguments".into());
            }
            Ok(NearbyAction::Stop {
                session_id: session()?,
            })
        }
        "delete-upload" => Ok(NearbyAction::UploadDelete { upload_id: one()? }),
        _ => Err("Unknown nearby command; use cos share --help.".into()),
    }
}

fn watch(args: &[String]) -> Result<Value, String> {
    if !args.is_empty() {
        return Err("watch accepts no arguments; Ctrl-C detaches without stopping sharing".into());
    }
    loop {
        let value = crate::operations::cli::request(Command::NearbyStatus, json!({}))?;
        let view: NearbyView = serde_json::from_value(value.clone()).map_err(|e| e.to_string())?;
        println!(
            "{}",
            serde_json::to_string(&value).map_err(|e| e.to_string())?
        );
        if view.session.as_ref().is_none_or(|session| {
            matches!(
                session.phase,
                SessionPhase::Ended | SessionPhase::Failed | SessionPhase::Interrupted
            )
        }) {
            return Ok(value);
        }
        std::thread::sleep(std::time::Duration::from_secs(1));
    }
}

pub(crate) fn presentation(view: &NearbyView) -> String {
    let mut lines=vec![
        "Nearby Sharing - AirDrop files; no Agent task or phone App required".into(),
        "Everyone mode: device names are not verified identities. Received files are never auto-opened.".into(),
        format!("Backend ready: {}; mode: {:?}; inbox: {}",view.probe.available,view.probe.mode,view.inbox),
    ];
    lines.extend(view.probe.diagnostics.iter().cloned());
    if let Some(session) = &view.session {
        lines.push(format!(
            "Window {} | {:?} | receive: {} | expires {}",
            session.id, session.phase, session.receive, session.expires_at
        ));
        if let Some(message) = &session.message {
            lines.push(message.clone());
        }
        lines.push(if session.discovering {
            "Discovering devices...".into()
        } else {
            "Devices (temporary IDs):".into()
        });
        for peer in &session.peers {
            lines.push(format!("{}  {}", peer.id, peer.name));
        }
        for offer in &session.offers {
            lines.push(format!(
                "Offer {} from {}: {:?}; expires {}",
                offer.id, offer.sender, offer.phase, offer.expires_at
            ));
            for file in &offer.files {
                lines.push(format!("  {} ({:?} bytes)", file.name, file.bytes));
            }
        }
        for transfer in &session.transfers {
            lines.push(format!(
                "Transfer {} {:?}: {} / {:?} bytes",
                transfer.id, transfer.phase, transfer.bytes, transfer.total_bytes
            ));
            lines.extend(transfer.received_paths.iter().cloned());
            if let Some(message) = &transfer.message {
                lines.push(message.clone());
            }
        }
    }
    for upload in &view.uploads {
        lines.push(format!(
            "Upload {} {}: {}/{} bytes, ready={}",
            upload.id, upload.name, upload.written, upload.bytes, upload.ready
        ));
    }
    lines.push("R refresh | D browse | O send window | I receive window | S send | A accept | N decline | X stop | Esc detach".into());
    lines.join("\n")
}
