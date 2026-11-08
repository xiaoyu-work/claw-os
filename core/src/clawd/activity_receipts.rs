//! Receipt reports are owner data, never proof of execution or authority.

use std::collections::BTreeSet;

use serde_json::{json, Value};

use crate::activities::{
    self, ActivityReceipt, ActivityService, ReceiptDeclaration, ReceiptEffect, ReceiptReport,
};
use crate::operations::receipts::{diagnostic, sanitize};

use super::activities::{decode, encode, owner, service_error};
use super::client_identity::ClientIdentity;
use super::protocol::BrokerError;
use super::wire::requests as body;

pub(crate) fn attach_reported_artifacts(
    owner: u32,
    activity_id: &str,
    producer: &str,
    references: &[String],
) -> Result<(), String> {
    use crate::objects::ObjectCatalog;
    if references.len() > 32 {
        return Err("an artifact report allows at most 32 references".into());
    }
    if references.is_empty() {
        return Ok(());
    }
    let service = activities::open_default().map_err(|error| error.to_string())?;
    let activity = service
        .get(owner, activity_id)
        .map_err(|error| error.to_string())?;
    if matches!(
        activity.state,
        activities::ActivityState::Completed | activities::ActivityState::Cancelled
    ) {
        return Ok(());
    }
    let mut known: BTreeSet<String> = activity
        .resources
        .iter()
        .map(|resource| resource.reference.clone())
        .collect();
    let catalog = crate::objects::default_catalog();
    let mut pending = Vec::new();
    for reference in references {
        let object =
            crate::objects::parse_reference(reference).map_err(|error| error.to_string())?;
        catalog
            .describe(&object)
            .map_err(|error| error.to_string())?;
        if known.insert(reference.clone()) {
            pending.push(reference.clone());
        }
    }
    if known.len() > 32 {
        return Err("the Activity's 32-resource limit would be exceeded".into());
    }
    for reference in pending {
        service
            .add_resource(
                owner,
                &activity.id,
                activities::ActivityResource {
                    label: format!("Artifact reported by {producer}"),
                    reference,
                },
            )
            .map_err(|error| error.to_string())?;
    }
    Ok(())
}

fn result_artifacts(
    result: &crate::agent::tools::mcp::protocol::CallToolResult,
) -> Result<Vec<String>, String> {
    use crate::agent::tools::mcp::protocol::ContentItem;
    let mut references = BTreeSet::new();
    for item in &result.content {
        let ContentItem::Text { text } = item else {
            continue;
        };
        let Ok(value) = serde_json::from_str::<Value>(text) else {
            continue;
        };
        let mut pending = vec![&value];
        let mut visited = 0;
        while let Some(value) = pending.pop() {
            visited += 1;
            if visited > 16_384 {
                return Err("App artifact metadata exceeds the traversal bound".into());
            }
            match value {
                Value::Object(object) => {
                    for field in ["object", "reference"] {
                        if let Some(Value::String(reference)) = object.get(field) {
                            if reference.starts_with("app://") {
                                references.insert(reference.clone());
                            }
                        }
                    }
                    if let Some(Value::Array(artifacts)) = object.get("artifacts") {
                        for reference in artifacts.iter().filter_map(Value::as_str) {
                            references.insert(reference.to_string());
                        }
                    }
                    pending.extend(
                        object
                            .values()
                            .filter(|value| value.is_object() || value.is_array()),
                    );
                }
                Value::Array(values) => pending.extend(values),
                _ => {}
            }
            if references.len() > 32 {
                return Err("App result reports more than 32 artifacts".into());
            }
        }
    }
    Ok(references.into_iter().collect())
}

pub fn list(params: Value, client: &ClientIdentity) -> Result<Value, BrokerError> {
    let owner = owner(client)?;
    let request: body::ActivityReceipts = decode(params)?;
    activities::validate_id(request.id.as_str()).map_err(service_error)?;
    let limit = match request.limit {
        None => activities::DEFAULT_LIST_LIMIT,
        Some(limit) if (1..=activities::MAX_LIST_LIMIT as u64).contains(&limit) => limit as usize,
        Some(_) => {
            return Err(BrokerError::execution(
                "receipt limit must be between 1 and 100",
            ))
        }
    };
    let service = activities::open_default().map_err(service_error)?;
    let activity = service
        .get(owner, request.id.as_str())
        .map_err(service_error)?;
    let receipts = service
        .receipts(owner, &activity.id, limit)
        .map_err(service_error)?;
    Ok(json!({"schema":1,"activity_id":activity.id,"receipts":receipts}))
}

pub fn record(params: Value, client: &ClientIdentity) -> Result<Value, BrokerError> {
    let owner = owner(client)?;
    let request: body::ActivityReceiptRecord = decode(params)?;
    encode(record_for_owner(
        owner,
        request.id.as_str(),
        request.report.0,
    )?)
}

/// The caller supplies an authenticated owner and an already-owned association,
/// never an identity copied from a model-authored report.
pub(crate) fn record_for_owner(
    owner: u32,
    activity_id: &str,
    mut report: ReceiptReport,
) -> Result<ActivityReceipt, BrokerError> {
    activities::validate_id(activity_id).map_err(service_error)?;
    report.validate().map_err(service_error)?;
    let service = activities::open_default().map_err(service_error)?;
    let activity = service.get(owner, activity_id).map_err(service_error)?;
    sanitize(&mut report);
    let (declaration, declaration_error) = match declaration(&report) {
        Ok(declaration) => (Some(declaration), None),
        Err(error) => (None, Some(diagnostic(&error))),
    };
    service
        .record_receipt(owner, &activity.id, report, declaration, declaration_error)
        .map_err(service_error)
}

pub(crate) fn link_to_task(
    store: &crate::agent::service::Store,
    task_id: &str,
    receipt: &ActivityReceipt,
) -> Result<(), String> {
    let link = || {
        let (_, job) = store
            .locate(task_id)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "receipt task no longer exists".to_string())?;
        if job.owner_uid != Some(receipt.owner_uid)
            || job.activity_id.as_deref() != Some(receipt.activity_id.as_str())
        {
            return Err("receipt task has a different owner or Activity".to_string());
        }
        store
            .append_stream_progress(
                task_id,
                json!({
                    "kind":"activity_receipt",
                    "activity_id":receipt.activity_id,
                    "receipt_id":receipt.id,
                    "source":receipt.source,
                    "reported_outcome":receipt.report.outcome,
                }),
            )
            .map_err(|error| error.to_string())
    };
    link().map_err(|error| {
        format!(
            "receipt {} was stored but its task link could not be recorded: {error}",
            receipt.id
        )
    })
}

pub(crate) fn record_service_result(
    owner: u32,
    activity_id: &str,
    app_id: &str,
    tool: &str,
    package_digest: &str,
    task_id: Option<&str>,
    mut outcome: Result<crate::agent::tools::mcp::protocol::CallToolResult, BrokerError>,
) -> Result<crate::agent::tools::mcp::protocol::CallToolResult, BrokerError> {
    use crate::operations::receipts::{capture_session, render_mcp_result};
    let rendered = outcome
        .as_ref()
        .map(render_mcp_result)
        .map_err(|error| error.message.clone());
    let report = capture_session(app_id, tool, package_digest, rendered);
    let recorded = record_for_owner(owner, activity_id, report.clone())
        .map_err(|error| error.to_string())
        .and_then(|receipt| {
            if let Some(task_id) = task_id {
                let store = crate::agent::service::Store::open_default()
                    .map_err(|error| error.to_string())?;
                link_to_task(&store, task_id, &receipt)?;
            }
            Ok(())
        });
    if let Err(error) = recorded {
        let message = match serde_json::to_string(&report) {
            Ok(report) => format!(
                "Activity receipt recording failed: {}. Do not repeat the App call. \
                 Retry only this report with cos activity record-receipt {activity_id} --stdin:\n{report}",
                diagnostic(&error),
            ),
            Err(encoding) => format!(
                "Activity receipt recording failed: {}. Report encoding failed: {}. Do not repeat the App call.",
                diagnostic(&error), diagnostic(&encoding.to_string()),
            ),
        };
        tracing::error!(owner_uid = owner, activity_id, error = %diagnostic(&error),
            "App service receipt recording failed");
        match &mut outcome {
            Ok(result) => {
                result
                    .content
                    .push(crate::agent::tools::mcp::protocol::ContentItem::Text { text: message });
                result.is_error = Some(true);
            }
            Err(error) => {
                error.message.push_str("\n\n");
                error.message.push_str(&message);
            }
        }
    } else if let Ok(result) = &mut outcome {
        if result.is_error != Some(true) {
            let attached = result_artifacts(result).and_then(|references| {
                attach_reported_artifacts(owner, activity_id, app_id, &references)
            });
            if let Err(error) = attached {
                result.content.push(crate::agent::tools::mcp::protocol::ContentItem::Text {
                    text: format!(
                        "Activity artifact attachment failed: {}. The original App result is preserved. \
                         Do not repeat the App call; retry only metadata through the existing Activity object attachment controls.",
                        diagnostic(&error),
                    ),
                });
                result.is_error = Some(true);
            }
        }
    }
    outcome
}

fn declaration(report: &ReceiptReport) -> Result<ReceiptDeclaration, String> {
    let root = std::env::var_os("COS_APPS_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| "/usr/lib/cos/apps".into());
    let app = crate::apps::find_verified(&root, &report.app_id)?;
    let package = app.require_verified()?;
    package
        .assert_current(&crate::provenance::trust_store())
        .map_err(|error| error.to_string())?;
    if package.content_digest() != report.package_digest {
        return Err(
            "Current App package differs from the reported package; declaration is not matched"
                .into(),
        );
    }
    let manifest = crate::caps::manifest::Manifest::from_json(
        &package.manifest_text().map_err(|error| error.to_string())?,
    )
    .map_err(|error| error.to_string())?;
    let (label, effects) = if let Some(name) = report.session_tool_name() {
        let tool = manifest
            .mcp
            .as_ref()
            .and_then(|session| session.tools.iter().find(|tool| tool.name == name))
            .ok_or_else(|| {
                "Reported session tool is not declared by the current App package".to_string()
            })?;
        (tool.summary.current().to_string(), &tool.effects)
    } else {
        let operation = manifest.operations.get(&report.operation).ok_or_else(|| {
            "Reported operation is not declared by the current App package".to_string()
        })?;
        (operation.label.current().to_string(), &operation.effects)
    };
    if manifest.version.len() > 128 || label.len() > 512 {
        return Err("App declaration labels exceed the receipt metadata bound".into());
    }
    let declaration = ReceiptDeclaration {
        app_version: manifest.version,
        operation_label: label,
        effects: effects
            .iter()
            .map(|effect| ReceiptEffect {
                kind: effect.kind,
                label: effect.label.current().to_string(),
                recovery: effect.recovery,
                target_arg: effect.target_arg.clone(),
            })
            .collect(),
    };
    declaration.validate().map_err(|error| error.to_string())?;
    Ok(declaration)
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/clawd/activity_receipts.rs"
    ));
}
