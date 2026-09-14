//! Strict translation for portable continuity without importing core models.

use cos_agent_protocol::{
    ActivityContinuityDocument, ActivityContinuityImportAcknowledgement,
    ActivityExecutionPlacement, ActivityState, ImportedActivity, ImportedActivityResource,
};
use serde::Deserialize;
use serde_json::Value;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CoreResource {
    label: String,
    reference: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CoreImportedActivity {
    id: String,
    owner_uid: u32,
    title: String,
    goal: String,
    completion_criteria: String,
    boundaries: String,
    resources: Vec<CoreResource>,
    state: ActivityState,
    completion_note: Option<String>,
    created_at: String,
    updated_at: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CoreImportAcknowledgement {
    activity: CoreImportedActivity,
    continuity_id: String,
    continuity_revision: u64,
    placement: ActivityExecutionPlacement,
}

pub fn export(value: Value) -> Result<ActivityContinuityDocument, String> {
    let data = serde_json::to_vec(&value)
        .map_err(|error| format!("invalid activity.continuity.export result: {error}"))?;
    ActivityContinuityDocument::from_json(&data)
        .map_err(|error| format!("invalid activity.continuity.export result: {error}"))
}

pub fn import(
    value: Value,
    owner_uid: u32,
    document: &ActivityContinuityDocument,
    placement: ActivityExecutionPlacement,
) -> Result<ActivityContinuityImportAcknowledgement, String> {
    let response: CoreImportAcknowledgement = serde_json::from_value(value)
        .map_err(|error| format!("invalid activity.continuity.import result: {error}"))?;
    if response.activity.owner_uid != owner_uid {
        return Err("Activity continuity import returned another owner".into());
    }
    let acknowledgement = ActivityContinuityImportAcknowledgement {
        activity: ImportedActivity {
            id: response.activity.id,
            title: response.activity.title,
            goal: response.activity.goal,
            completion_criteria: response.activity.completion_criteria,
            boundaries: response.activity.boundaries,
            resources: response
                .activity
                .resources
                .into_iter()
                .map(|resource| ImportedActivityResource {
                    label: resource.label,
                    reference: resource.reference,
                })
                .collect(),
            state: response.activity.state,
            completion_note: response.activity.completion_note,
            created_at: response.activity.created_at,
            updated_at: response.activity.updated_at,
        },
        continuity_id: response.continuity_id,
        continuity_revision: response.continuity_revision,
        placement: response.placement,
    };
    if !acknowledgement.matches(document, placement) {
        return Err(
            "Activity continuity import acknowledgement did not match the document, paused state, lineage, revision, or local placement"
                .into(),
        );
    }
    Ok(acknowledgement)
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/translation/continuity.rs"
    ));
}
