//! Authenticated HTTP presentation of the shared, owner-scoped Activity broker.

use axum::extract::rejection::{JsonRejection, QueryRejection};
use axum::extract::{Extension, Path, Query};
use axum::http::StatusCode;
use axum::Json;
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::{json, Value};

use crate::activities::{
    Activity, ActivityContinuityDocument, ActivityContinuityImport, ActivityContinuityLineage,
    ActivityDraft, ActivityExecutionPlacement, ActivityMonetaryBudget, ActivityResource,
    ActivitySchedulingPolicy, ActivitySchedulingPriority, ActivityState, MonetaryBudgetDraft,
    PortableActivityIntent, PortableActivityReference, PortableActivityRules,
    PortableExecutionLimits, PortableSchedulingPreference, MAX_RATE_MICROUSD_PER_MILLION_TOKENS,
    MAX_TOTAL_MICROUSD,
};
use crate::agent::web::auth::AuthenticatedToken;
use crate::clawd::routes::Command;
use crate::clawd::wire::requests::{
    ActivityCapabilityPolicyEnabled, ActivityCapabilityPolicyGet, ActivityCapabilityPolicySet,
    ActivityContinuityExport, ActivityContinuityImport as ActivityContinuityImportRequest,
    ActivityExecutionLimitsEnabled, ActivityExecutionLimitsGet, ActivityExecutionLimitsSet,
    ActivityMonetaryBudgetEnabled, ActivityMonetaryBudgetGet, ActivityMonetaryBudgetSet,
    ActivitySchedulingPolicyGet, ActivitySchedulingPolicySet,
};
use crate::clawd::wire::requests::{
    ActivityCreate, ActivityGet, ActivityList, ActivityObjectAttach, ActivityObjectState,
    ActivityObjectStateRecord, ActivityObjects, ActivityOperationPreview, ActivityReceipts,
    ActivityRun, ActivityTransition, ActivityUpdate, NoBody,
};

use super::clawd::ApiError;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DetailQuery {
    limit: Option<u64>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ObjectStateQuery {
    reference: Option<String>,
    limit: Option<u64>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MonetaryBudgetHttpDraft {
    currency: String,
    max_total_microusd: String,
    input_microusd_per_million_tokens: String,
    output_microusd_per_million_tokens: String,
    max_output_tokens_per_turn: u32,
}

impl MonetaryBudgetHttpDraft {
    fn into_core(self) -> Result<MonetaryBudgetDraft, ApiError> {
        let budget = MonetaryBudgetDraft {
            currency: self.currency,
            max_total_microusd: decimal_u64(
                "max_total_microusd",
                &self.max_total_microusd,
                MAX_TOTAL_MICROUSD,
            )?,
            input_microusd_per_million_tokens: decimal_u64(
                "input_microusd_per_million_tokens",
                &self.input_microusd_per_million_tokens,
                MAX_RATE_MICROUSD_PER_MILLION_TOKENS,
            )?,
            output_microusd_per_million_tokens: decimal_u64(
                "output_microusd_per_million_tokens",
                &self.output_microusd_per_million_tokens,
                MAX_RATE_MICROUSD_PER_MILLION_TOKENS,
            )?,
            max_output_tokens_per_turn: self.max_output_tokens_per_turn,
        };
        budget
            .validate()
            .map_err(|error| bad_request(error.to_string()))?;
        Ok(budget)
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MonetaryBudgetHttpSet {
    #[serde(deserialize_with = "required_nullable")]
    expected_revision: Option<String>,
    budget: MonetaryBudgetHttpDraft,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MonetaryBudgetHttpEnabled {
    expected_revision: String,
    enabled: bool,
}

#[derive(Debug, Serialize)]
#[serde(deny_unknown_fields)]
pub struct MonetaryBudgetHttpPolicy {
    activity_id: String,
    owner_uid: u32,
    revision: String,
    enabled: bool,
    spent_microusd: String,
    reserved_microusd: String,
    budget: MonetaryBudgetHttpPolicyDraft,
    created_at: String,
    updated_at: String,
}

#[derive(Debug, Serialize)]
#[serde(deny_unknown_fields)]
pub struct MonetaryBudgetHttpPolicyDraft {
    currency: String,
    max_total_microusd: String,
    input_microusd_per_million_tokens: String,
    output_microusd_per_million_tokens: String,
    max_output_tokens_per_turn: u32,
}

#[derive(Debug, Serialize)]
#[serde(deny_unknown_fields)]
pub struct MonetaryBudgetHttpView {
    schema: u32,
    activity_id: String,
    monetary_budget: Option<MonetaryBudgetHttpPolicy>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct BrokerMonetaryBudgetView {
    schema: u32,
    activity_id: String,
    #[serde(deserialize_with = "required_nullable")]
    monetary_budget: Option<ActivityMonetaryBudget>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SchedulingPriorityHttpSet {
    #[serde(deserialize_with = "required_nullable")]
    expected_revision: Option<String>,
    priority: ActivitySchedulingPriority,
}

#[derive(Debug, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SchedulingPriorityHttpPolicy {
    activity_id: String,
    owner_uid: u32,
    revision: String,
    priority: ActivitySchedulingPriority,
    created_at: String,
    updated_at: String,
}

#[derive(Debug, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SchedulingPriorityHttpView {
    schema: u32,
    activity_id: String,
    scheduling_policy: Option<SchedulingPriorityHttpPolicy>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct BrokerSchedulingPriorityView {
    schema: u32,
    activity_id: String,
    #[serde(deserialize_with = "required_nullable")]
    scheduling_policy: Option<ActivitySchedulingPolicy>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ContinuityLineageHttp {
    id: String,
    revision: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ContinuityIntentHttp {
    title: String,
    goal: String,
    completion_criteria: String,
    boundaries: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ContinuityReferenceHttp {
    label: String,
    reference: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ContinuityExecutionLimitsHttp {
    enabled: bool,
    max_attempts: u32,
    max_turns_per_attempt: u32,
    expires_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ContinuitySchedulingHttp {
    priority: ActivitySchedulingPriority,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ContinuityRulesHttp {
    #[serde(deserialize_with = "required_nullable")]
    execution_limits: Option<ContinuityExecutionLimitsHttp>,
    #[serde(deserialize_with = "required_nullable")]
    scheduling: Option<ContinuitySchedulingHttp>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ContinuityDocumentHttp {
    kind: String,
    schema_version: u32,
    lineage: ContinuityLineageHttp,
    snapshot: String,
    intent: ContinuityIntentHttp,
    references: Vec<ContinuityReferenceHttp>,
    rules: ContinuityRulesHttp,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContinuityImportHttp {
    placement: ActivityExecutionPlacement,
    document: ContinuityDocumentHttp,
}

#[derive(Debug, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ContinuityImportHttpView {
    activity: Activity,
    continuity_id: String,
    continuity_revision: String,
    placement: ActivityExecutionPlacement,
}

impl ContinuityDocumentHttp {
    fn from_core(document: ActivityContinuityDocument) -> Result<Self, ApiError> {
        document
            .to_json()
            .map_err(|error| bad_gateway(format!("invalid continuity document: {error}")))?;
        Ok(Self {
            kind: document.kind,
            schema_version: document.schema_version,
            lineage: ContinuityLineageHttp {
                id: document.lineage.id,
                revision: document.lineage.revision.to_string(),
            },
            snapshot: document.snapshot,
            intent: ContinuityIntentHttp {
                title: document.intent.title,
                goal: document.intent.goal,
                completion_criteria: document.intent.completion_criteria,
                boundaries: document.intent.boundaries,
            },
            references: document
                .references
                .into_iter()
                .map(|reference| ContinuityReferenceHttp {
                    label: reference.label,
                    reference: reference.reference,
                })
                .collect(),
            rules: ContinuityRulesHttp {
                execution_limits: document.rules.execution_limits.map(|limits| {
                    ContinuityExecutionLimitsHttp {
                        enabled: limits.enabled,
                        max_attempts: limits.max_attempts,
                        max_turns_per_attempt: limits.max_turns_per_attempt,
                        expires_at: limits.expires_at,
                    }
                }),
                scheduling: document
                    .rules
                    .scheduling
                    .map(|scheduling| ContinuitySchedulingHttp {
                        priority: scheduling.priority,
                    }),
            },
        })
    }

    fn into_core(self) -> Result<ActivityContinuityDocument, ApiError> {
        let document = ActivityContinuityDocument {
            kind: self.kind,
            schema_version: self.schema_version,
            lineage: ActivityContinuityLineage {
                id: self.lineage.id,
                revision: decimal_u64(
                    "continuity lineage revision",
                    &self.lineage.revision,
                    i64::MAX as u64,
                )?,
            },
            snapshot: self.snapshot,
            intent: PortableActivityIntent {
                title: self.intent.title,
                goal: self.intent.goal,
                completion_criteria: self.intent.completion_criteria,
                boundaries: self.intent.boundaries,
            },
            references: self
                .references
                .into_iter()
                .map(|reference| PortableActivityReference {
                    label: reference.label,
                    reference: reference.reference,
                })
                .collect(),
            rules: PortableActivityRules {
                execution_limits: self.rules.execution_limits.map(|limits| {
                    PortableExecutionLimits {
                        enabled: limits.enabled,
                        max_attempts: limits.max_attempts,
                        max_turns_per_attempt: limits.max_turns_per_attempt,
                        expires_at: limits.expires_at,
                    }
                }),
                scheduling: self
                    .rules
                    .scheduling
                    .map(|scheduling| PortableSchedulingPreference {
                        priority: scheduling.priority,
                    }),
            },
        };
        document
            .to_json()
            .map_err(|error| bad_request(error.to_string()))?;
        Ok(document)
    }
}

pub async fn list(
    query: Result<Query<ActivityList>, QueryRejection>,
) -> Result<Json<Value>, ApiError> {
    let Query(query) = query.map_err(|error| bad_request(error.body_text()))?;
    request(Command::ActivityList, query).await
}

pub async fn create(
    body: Result<Json<ActivityCreate>, JsonRejection>,
) -> Result<Json<Value>, ApiError> {
    request(Command::ActivityCreate, json_body(body)?).await
}

pub async fn get(
    Path(id): Path<String>,
    query: Result<Query<DetailQuery>, QueryRejection>,
) -> Result<Json<Value>, ApiError> {
    let Query(query) = query.map_err(|error| bad_request(error.body_text()))?;
    if query.limit.is_some_and(|limit| !(1..=100).contains(&limit)) {
        return Err(bad_request("limit must be between 1 and 100"));
    }
    request(
        Command::ActivityGet,
        with_id::<ActivityGet>(id, json!({ "limit": query.limit }))?,
    )
    .await
}

pub async fn attention(
    Path(id): Path<String>,
    query: Result<Query<DetailQuery>, QueryRejection>,
) -> Result<Json<Value>, ApiError> {
    let Query(query) = query.map_err(|error| bad_request(error.body_text()))?;
    if query.limit.is_some_and(|limit| !(1..=100).contains(&limit)) {
        return Err(bad_request("limit must be between 1 and 100"));
    }
    request(
        Command::ActivityAttention,
        with_id::<ActivityGet>(id, json!({ "limit": query.limit }))?,
    )
    .await
}

pub async fn update(
    Path(id): Path<String>,
    body: Result<Json<Value>, JsonRejection>,
) -> Result<Json<Value>, ApiError> {
    request(
        Command::ActivityUpdate,
        with_id::<ActivityUpdate>(id, json_body(body)?)?,
    )
    .await
}

pub async fn transition(
    Path(id): Path<String>,
    body: Result<Json<Value>, JsonRejection>,
) -> Result<Json<Value>, ApiError> {
    request(
        Command::ActivityTransition,
        with_id::<ActivityTransition>(id, json_body(body)?)?,
    )
    .await
}

pub async fn run(
    Path(id): Path<String>,
    body: Result<Json<Value>, JsonRejection>,
) -> Result<Json<Value>, ApiError> {
    request(
        Command::ActivityRun,
        with_id::<ActivityRun>(id, json_body(body)?)?,
    )
    .await
}

pub async fn objects(
    Path(id): Path<String>,
    query: Result<Query<NoBody>, QueryRejection>,
) -> Result<Json<Value>, ApiError> {
    query.map_err(|error| bad_request(error.body_text()))?;
    request(
        Command::ActivityObjects,
        with_id::<ActivityObjects>(id, json!({}))?,
    )
    .await
}

pub async fn attach_object(
    Path(id): Path<String>,
    body: Result<Json<Value>, JsonRejection>,
) -> Result<Json<Value>, ApiError> {
    request(
        Command::ActivityObjectAttach,
        with_id::<ActivityObjectAttach>(id, json_body(body)?)?,
    )
    .await
}

pub async fn operation_preview(
    Path(id): Path<String>,
    body: Result<Json<Value>, JsonRejection>,
) -> Result<Json<Value>, ApiError> {
    request(
        Command::ActivityOperationPreview,
        with_id::<ActivityOperationPreview>(id, json_body(body)?)?,
    )
    .await
}

pub async fn receipts(
    Path(id): Path<String>,
    query: Result<Query<DetailQuery>, QueryRejection>,
) -> Result<Json<Value>, ApiError> {
    let Query(query) = query.map_err(|error| bad_request(error.body_text()))?;
    request(
        Command::ActivityReceipts,
        with_id::<ActivityReceipts>(id, json!({ "limit": query.limit }))?,
    )
    .await
}

pub async fn object_state(
    Path(id): Path<String>,
    query: Result<Query<ObjectStateQuery>, QueryRejection>,
) -> Result<Json<Value>, ApiError> {
    let Query(query) = query.map_err(|error| bad_request(error.body_text()))?;
    request(
        Command::ActivityObjectStateList,
        with_id::<ActivityObjectState>(
            id,
            json!({
                "reference":query.reference,"limit":query.limit,
            }),
        )?,
    )
    .await
}

pub async fn record_object_state(
    Path(id): Path<String>,
    body: Result<Json<Value>, JsonRejection>,
) -> Result<Json<Value>, ApiError> {
    request(
        Command::ActivityObjectStateRecord,
        with_id::<ActivityObjectStateRecord>(id, json_body(body)?)?,
    )
    .await
}

pub async fn execution_limits(
    Path(id): Path<String>,
    query: Result<Query<NoBody>, QueryRejection>,
) -> Result<Json<Value>, ApiError> {
    query.map_err(|error| bad_request(error.body_text()))?;
    request(
        Command::ActivityExecutionLimitsGet,
        with_id::<ActivityExecutionLimitsGet>(id, json!({}))?,
    )
    .await
}

pub async fn set_execution_limits(
    Path(id): Path<String>,
    body: Result<Json<Value>, JsonRejection>,
) -> Result<Json<Value>, ApiError> {
    request(
        Command::ActivityExecutionLimitsSet,
        with_id::<ActivityExecutionLimitsSet>(id, json_body(body)?)?,
    )
    .await
}

pub async fn enable_execution_limits(
    Path(id): Path<String>,
    body: Result<Json<Value>, JsonRejection>,
) -> Result<Json<Value>, ApiError> {
    request(
        Command::ActivityExecutionLimitsEnabled,
        with_id::<ActivityExecutionLimitsEnabled>(id, json_body(body)?)?,
    )
    .await
}

pub async fn monetary_budget(
    Extension(authenticated): Extension<AuthenticatedToken>,
    Path(id): Path<String>,
    query: Result<Query<NoBody>, QueryRejection>,
) -> Result<Json<MonetaryBudgetHttpView>, ApiError> {
    query.map_err(|error| bad_request(error.body_text()))?;
    let Json(value) = request(
        Command::ActivityMonetaryBudgetGet,
        with_id::<ActivityMonetaryBudgetGet>(id.clone(), json!({}))?,
    )
    .await?;
    let response: BrokerMonetaryBudgetView = serde_json::from_value(value)
        .map_err(|error| bad_gateway(format!("invalid monetary-budget response: {error}")))?;
    if response.schema != 1 || !same_activity(&response.activity_id, &id) {
        return Err(bad_gateway(
            "monetary-budget response did not match the requested Activity",
        ));
    }
    let monetary_budget = response
        .monetary_budget
        .map(|policy| validate_monetary_policy(policy, &id, authenticated.uid))
        .transpose()?;
    Ok(Json(MonetaryBudgetHttpView {
        schema: 1,
        activity_id: response.activity_id,
        monetary_budget,
    }))
}

pub async fn set_monetary_budget(
    Extension(authenticated): Extension<AuthenticatedToken>,
    Path(id): Path<String>,
    body: Result<Json<MonetaryBudgetHttpSet>, JsonRejection>,
) -> Result<Json<MonetaryBudgetHttpPolicy>, ApiError> {
    let body = json_body(body)?;
    let expected_revision = body
        .expected_revision
        .as_deref()
        .map(|revision| decimal_revision("expected_revision", revision))
        .transpose()?;
    let expected_next = expected_revision
        .unwrap_or(0)
        .checked_add(1)
        .ok_or_else(|| bad_request("expected_revision cannot be incremented"))?;
    let budget = body.budget.into_core()?;
    let Json(value) = request(
        Command::ActivityMonetaryBudgetSet,
        with_id::<ActivityMonetaryBudgetSet>(
            id.clone(),
            json!({"expected_revision": expected_revision, "budget": budget}),
        )?,
    )
    .await?;
    let policy: ActivityMonetaryBudget = serde_json::from_value(value).map_err(|error| {
        bad_gateway(format!("invalid monetary-budget acknowledgement: {error}"))
    })?;
    if policy.revision != expected_next
        || policy.budget != budget
        || (expected_revision.is_none() && !policy.enabled)
    {
        return Err(bad_gateway(
            "monetary-budget acknowledgement did not match the submitted budget or revision",
        ));
    }
    validate_monetary_policy(policy, &id, authenticated.uid).map(Json)
}

pub async fn enable_monetary_budget(
    Extension(authenticated): Extension<AuthenticatedToken>,
    Path(id): Path<String>,
    body: Result<Json<MonetaryBudgetHttpEnabled>, JsonRejection>,
) -> Result<Json<MonetaryBudgetHttpPolicy>, ApiError> {
    let body = json_body(body)?;
    let expected_revision = decimal_revision("expected_revision", &body.expected_revision)?;
    let expected_next = expected_revision
        .checked_add(1)
        .ok_or_else(|| bad_request("expected_revision cannot be incremented"))?;
    let Json(value) = request(
        Command::ActivityMonetaryBudgetEnabled,
        with_id::<ActivityMonetaryBudgetEnabled>(
            id.clone(),
            json!({"expected_revision": expected_revision, "enabled": body.enabled}),
        )?,
    )
    .await?;
    let policy: ActivityMonetaryBudget = serde_json::from_value(value).map_err(|error| {
        bad_gateway(format!("invalid monetary-budget acknowledgement: {error}"))
    })?;
    if policy.revision != expected_next || policy.enabled != body.enabled {
        return Err(bad_gateway(
            "monetary-budget acknowledgement did not match the requested enabled state or revision",
        ));
    }
    validate_monetary_policy(policy, &id, authenticated.uid).map(Json)
}

pub async fn scheduling_priority(
    Extension(authenticated): Extension<AuthenticatedToken>,
    Path(id): Path<String>,
    query: Result<Query<NoBody>, QueryRejection>,
) -> Result<Json<SchedulingPriorityHttpView>, ApiError> {
    query.map_err(|error| bad_request(error.body_text()))?;
    let Json(value) = request(
        Command::ActivitySchedulingPolicyGet,
        with_id::<ActivitySchedulingPolicyGet>(id.clone(), json!({}))?,
    )
    .await?;
    let response: BrokerSchedulingPriorityView = serde_json::from_value(value)
        .map_err(|error| bad_gateway(format!("invalid scheduling-priority response: {error}")))?;
    if response.schema != 1 || !same_activity(&response.activity_id, &id) {
        return Err(bad_gateway(
            "scheduling-priority response did not match the requested Activity",
        ));
    }
    let scheduling_policy = response
        .scheduling_policy
        .map(|policy| validate_scheduling_policy(policy, &id, authenticated.uid))
        .transpose()?;
    Ok(Json(SchedulingPriorityHttpView {
        schema: 1,
        activity_id: response.activity_id,
        scheduling_policy,
    }))
}

pub async fn set_scheduling_priority(
    Extension(authenticated): Extension<AuthenticatedToken>,
    Path(id): Path<String>,
    body: Result<Json<SchedulingPriorityHttpSet>, JsonRejection>,
) -> Result<Json<SchedulingPriorityHttpPolicy>, ApiError> {
    let body = json_body(body)?;
    let expected_revision = body
        .expected_revision
        .as_deref()
        .map(|revision| decimal_revision("expected_revision", revision))
        .transpose()?;
    let expected_next = expected_revision
        .unwrap_or(0)
        .checked_add(1)
        .ok_or_else(|| bad_request("expected_revision cannot be incremented"))?;
    let Json(value) = request(
        Command::ActivitySchedulingPolicySet,
        with_id::<ActivitySchedulingPolicySet>(
            id.clone(),
            json!({
                "expected_revision": expected_revision,
                "priority": body.priority,
            }),
        )?,
    )
    .await?;
    let policy: ActivitySchedulingPolicy = serde_json::from_value(value).map_err(|error| {
        bad_gateway(format!(
            "invalid scheduling-priority acknowledgement: {error}"
        ))
    })?;
    if policy.revision != expected_next || policy.priority != body.priority {
        return Err(bad_gateway(
            "scheduling-priority acknowledgement did not match the submitted priority or revision",
        ));
    }
    validate_scheduling_policy(policy, &id, authenticated.uid).map(Json)
}

pub async fn export_continuity(
    Path(id): Path<String>,
    query: Result<Query<NoBody>, QueryRejection>,
) -> Result<Json<ContinuityDocumentHttp>, ApiError> {
    query.map_err(|error| bad_request(error.body_text()))?;
    let Json(value) = request(
        Command::ActivityContinuityExport,
        with_id::<ActivityContinuityExport>(id, json!({}))?,
    )
    .await?;
    let data = serde_json::to_vec(&value)
        .map_err(|error| bad_gateway(format!("encode continuity response: {error}")))?;
    let document = ActivityContinuityDocument::from_json(&data)
        .map_err(|error| bad_gateway(format!("invalid continuity response: {error}")))?;
    ContinuityDocumentHttp::from_core(document).map(Json)
}

pub async fn import_continuity(
    Extension(authenticated): Extension<AuthenticatedToken>,
    body: Result<Json<ContinuityImportHttp>, JsonRejection>,
) -> Result<Json<ContinuityImportHttpView>, ApiError> {
    let body = json_body(body)?;
    let placement = body.placement;
    let document = body.document.into_core()?;
    let canonical = String::from_utf8(
        document
            .to_json()
            .map_err(|error| bad_request(error.to_string()))?,
    )
    .expect("serialized Activity continuity JSON is UTF-8");
    let broker_request: ActivityContinuityImportRequest = serde_json::from_value(json!({
        "placement": placement,
        "document": canonical,
    }))
    .map_err(|error| bad_request(error.to_string()))?;
    let Json(value) = request(Command::ActivityContinuityImport, broker_request).await?;
    let imported: ActivityContinuityImport = serde_json::from_value(value)
        .map_err(|error| bad_gateway(format!("invalid continuity acknowledgement: {error}")))?;
    validate_import_acknowledgement(imported, &document, placement, authenticated.uid).map(Json)
}

pub async fn capability_policy_catalog(
    query: Result<Query<NoBody>, QueryRejection>,
) -> Result<Json<Value>, ApiError> {
    query.map_err(|error| bad_request(error.body_text()))?;
    let verbs: Vec<Value> = crate::caps::CATALOG
        .iter()
        .map(|entry| {
            json!({
                "verb": entry.verb,
                "scope_kind": entry.scope_kind,
                "label": entry.label.current(),
                "description": entry.blurb.current(),
            })
        })
        .collect();
    Ok(Json(json!({"schema": 1, "verbs": verbs})))
}

pub async fn capability_policy(
    Path(id): Path<String>,
    query: Result<Query<NoBody>, QueryRejection>,
) -> Result<Json<Value>, ApiError> {
    query.map_err(|error| bad_request(error.body_text()))?;
    request(
        Command::ActivityCapabilityPolicyGet,
        with_id::<ActivityCapabilityPolicyGet>(id, json!({}))?,
    )
    .await
}

pub async fn set_capability_policy(
    Path(id): Path<String>,
    body: Result<Json<Value>, JsonRejection>,
) -> Result<Json<Value>, ApiError> {
    request(
        Command::ActivityCapabilityPolicySet,
        with_id::<ActivityCapabilityPolicySet>(id, json_body(body)?)?,
    )
    .await
}

pub async fn enable_capability_policy(
    Path(id): Path<String>,
    body: Result<Json<Value>, JsonRejection>,
) -> Result<Json<Value>, ApiError> {
    request(
        Command::ActivityCapabilityPolicyEnabled,
        with_id::<ActivityCapabilityPolicyEnabled>(id, json_body(body)?)?,
    )
    .await
}

fn json_body<T>(body: Result<Json<T>, JsonRejection>) -> Result<T, ApiError> {
    body.map(|Json(body)| body)
        .map_err(|error| (error.status(), Json(json!({ "error": error.body_text() }))))
}

fn with_id<T: DeserializeOwned>(id: String, mut body: Value) -> Result<T, ApiError> {
    let fields = body
        .as_object_mut()
        .ok_or_else(|| bad_request("Activity body must be a JSON object"))?;
    if fields.contains_key("id") {
        return Err(bad_request("Activity id belongs in the URL, not the body"));
    }
    fields.insert("id".to_string(), json!(id));
    // The broker's bounded, closed DTOs remain the request contract. There is
    // no Web-owned lifecycle, persistence, or caller-supplied owner identity.
    serde_json::from_value(body).map_err(|error| bad_request(error.to_string()))
}

fn validate_monetary_policy(
    policy: ActivityMonetaryBudget,
    activity_id: &str,
    owner_uid: u32,
) -> Result<MonetaryBudgetHttpPolicy, ApiError> {
    if !same_activity(&policy.activity_id, activity_id)
        || policy.owner_uid != owner_uid
        || policy.revision == 0
        || chrono::DateTime::parse_from_rfc3339(&policy.created_at).is_err()
        || chrono::DateTime::parse_from_rfc3339(&policy.updated_at).is_err()
    {
        return Err(bad_gateway(
            "monetary-budget response returned an invalid Activity, owner, revision, or timestamp",
        ));
    }
    policy
        .budget
        .validate()
        .map_err(|error| bad_gateway(format!("invalid monetary-budget policy: {error}")))?;
    Ok(MonetaryBudgetHttpPolicy {
        activity_id: policy.activity_id,
        owner_uid: policy.owner_uid,
        revision: policy.revision.to_string(),
        enabled: policy.enabled,
        spent_microusd: policy.spent_microusd.to_string(),
        reserved_microusd: policy.reserved_microusd.to_string(),
        budget: MonetaryBudgetHttpPolicyDraft {
            currency: policy.budget.currency,
            max_total_microusd: policy.budget.max_total_microusd.to_string(),
            input_microusd_per_million_tokens: policy
                .budget
                .input_microusd_per_million_tokens
                .to_string(),
            output_microusd_per_million_tokens: policy
                .budget
                .output_microusd_per_million_tokens
                .to_string(),
            max_output_tokens_per_turn: policy.budget.max_output_tokens_per_turn,
        },
        created_at: policy.created_at,
        updated_at: policy.updated_at,
    })
}

fn validate_scheduling_policy(
    policy: ActivitySchedulingPolicy,
    activity_id: &str,
    owner_uid: u32,
) -> Result<SchedulingPriorityHttpPolicy, ApiError> {
    if !same_activity(&policy.activity_id, activity_id)
        || policy.owner_uid != owner_uid
        || policy.revision == 0
        || chrono::DateTime::parse_from_rfc3339(&policy.created_at).is_err()
        || chrono::DateTime::parse_from_rfc3339(&policy.updated_at).is_err()
    {
        return Err(bad_gateway(
            "scheduling-priority response returned an invalid Activity, owner, revision, or timestamp",
        ));
    }
    Ok(SchedulingPriorityHttpPolicy {
        activity_id: policy.activity_id,
        owner_uid: policy.owner_uid,
        revision: policy.revision.to_string(),
        priority: policy.priority,
        created_at: policy.created_at,
        updated_at: policy.updated_at,
    })
}

fn validate_import_acknowledgement(
    imported: ActivityContinuityImport,
    document: &ActivityContinuityDocument,
    placement: ActivityExecutionPlacement,
    owner_uid: u32,
) -> Result<ContinuityImportHttpView, ApiError> {
    if imported.placement != ActivityExecutionPlacement::Local
        || imported.placement != placement
        || imported.continuity_id != document.lineage.id
        || imported.continuity_revision != document.lineage.revision
    {
        return Err(bad_gateway(
            "continuity acknowledgement did not match the submitted lineage or local placement",
        ));
    }
    validate_imported_activity(&imported.activity, document, owner_uid)?;
    Ok(ContinuityImportHttpView {
        activity: imported.activity,
        continuity_id: imported.continuity_id,
        continuity_revision: imported.continuity_revision.to_string(),
        placement: imported.placement,
    })
}

fn validate_imported_activity(
    activity: &Activity,
    document: &ActivityContinuityDocument,
    owner_uid: u32,
) -> Result<(), ApiError> {
    let canonical_id = uuid::Uuid::parse_str(&activity.id)
        .map_err(|_| bad_gateway("continuity acknowledgement returned an invalid Activity ID"))?
        .to_string();
    let expected_resources: Vec<ActivityResource> = document
        .references
        .iter()
        .map(|reference| ActivityResource {
            label: reference.label.clone(),
            reference: reference.reference.clone(),
        })
        .collect();
    let draft = ActivityDraft {
        title: activity.title.clone(),
        goal: activity.goal.clone(),
        completion_criteria: activity.completion_criteria.clone(),
        boundaries: activity.boundaries.clone(),
        resources: activity.resources.clone(),
    };
    draft
        .validate()
        .map_err(|error| bad_gateway(format!("invalid imported Activity: {error}")))?;
    if canonical_id != activity.id
        || activity.owner_uid != owner_uid
        || activity.state != ActivityState::Paused
        || activity.completion_note.is_some()
        || activity.title != document.intent.title
        || activity.goal != document.intent.goal
        || activity.completion_criteria != document.intent.completion_criteria
        || activity.boundaries != document.intent.boundaries
        || activity.resources != expected_resources
        || chrono::DateTime::parse_from_rfc3339(&activity.created_at).is_err()
        || chrono::DateTime::parse_from_rfc3339(&activity.updated_at).is_err()
    {
        return Err(bad_gateway(
            "continuity acknowledgement did not contain the exact new paused Activity",
        ));
    }
    Ok(())
}

fn decimal_u64(field: &str, value: &str, maximum: u64) -> Result<u64, ApiError> {
    if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(bad_request(format!(
            "{field} must be a positive decimal integer"
        )));
    }
    let parsed = value
        .parse::<u64>()
        .map_err(|_| bad_request(format!("{field} is not representable")))?;
    if !(1..=maximum).contains(&parsed) {
        return Err(bad_request(format!(
            "{field} must be between 1 and {maximum}"
        )));
    }
    Ok(parsed)
}

fn decimal_revision(field: &str, value: &str) -> Result<u64, ApiError> {
    decimal_u64(field, value, u64::MAX - 1)
}

fn same_activity(left: &str, right: &str) -> bool {
    match (uuid::Uuid::parse_str(left), uuid::Uuid::parse_str(right)) {
        (Ok(left), Ok(right)) => left == right,
        _ => false,
    }
}

fn required_nullable<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer)
}

async fn request(command: Command, body: impl Serialize) -> Result<Json<Value>, ApiError> {
    let params = serde_json::to_value(body).map_err(|error| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": format!("encode Activity request: {error}") })),
        )
    })?;
    super::clawd::request(command, params)
        .await
        .map(Json)
        .map_err(super::clawd::RpcError::into_api_error)
}

fn bad_request(message: impl Into<String>) -> ApiError {
    (
        StatusCode::BAD_REQUEST,
        Json(json!({ "error": message.into() })),
    )
}

fn bad_gateway(message: impl Into<String>) -> ApiError {
    (
        StatusCode::BAD_GATEWAY,
        Json(json!({ "error": message.into() })),
    )
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/agent/web/routes/activities.rs"
    ));
}
