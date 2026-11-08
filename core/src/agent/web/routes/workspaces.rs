//! Authenticated human adapters over the shared Workspace broker.

use crate::clawd::routes::Command;
use crate::workspaces::{WorkspaceList, WorkspaceView};
use axum::{extract::Path, http::StatusCode, Json};
use serde_json::{json, Value};

type ApiResult = Result<Json<Value>, (StatusCode, Json<Value>)>;

fn invalid(message: impl ToString) -> (StatusCode, Json<Value>) {
    (
        StatusCode::BAD_REQUEST,
        Json(json!({"error":message.to_string()})),
    )
}

fn decimal_revision(value: &mut Value) -> Result<(), String> {
    if let Some(revision) = value.get_mut("expected_revision") {
        let text = revision
            .as_str()
            .ok_or("Workspace revisions must be displayed decimal strings")?;
        if text.is_empty()
            || text.starts_with('0')
            || !text.bytes().all(|byte| byte.is_ascii_digit())
        {
            return Err("invalid Workspace revision".into());
        }
        let number: u64 = text.parse().map_err(|_| "invalid Workspace revision")?;
        if number > i64::MAX as u64 {
            return Err("Workspace revision exceeds its range".into());
        }
        *revision = json!(number);
    }
    Ok(())
}

pub(super) fn project(mut value: Value) -> Value {
    match &mut value {
        Value::Object(fields) => {
            for (name, field) in fields {
                if matches!(name.as_str(), "revision" | "access_revision") {
                    if let Some(number) = field.as_u64() {
                        *field = json!(number.to_string());
                    }
                } else {
                    *field = project(field.take());
                }
            }
        }
        Value::Array(items) => {
            for item in items {
                *item = project(item.take());
            }
        }
        _ => {}
    }
    value
}

fn checked_view(value: Value) -> ApiResult {
    let view: WorkspaceView = serde_json::from_value(value).map_err(invalid)?;
    view.validate().map_err(invalid)?;
    Ok(Json(project(serde_json::to_value(view).map_err(invalid)?)))
}

pub async fn list() -> ApiResult {
    let value = super::clawd::request(Command::WorkspaceList, json!({}))
        .await
        .map_err(super::clawd::RpcError::into_api_error)?;
    let list: WorkspaceList = serde_json::from_value(value).map_err(invalid)?;
    if list.schema != 1 || list.workspaces.len() > crate::workspaces::MAX_WORKSPACES {
        return Err(invalid("invalid Workspace list"));
    }
    let mut ids = std::collections::BTreeSet::new();
    for workspace in &list.workspaces {
        workspace.validate().map_err(invalid)?;
        if !ids.insert(&workspace.id) {
            return Err(invalid("duplicate Workspace identity"));
        }
    }
    Ok(Json(project(serde_json::to_value(list).map_err(invalid)?)))
}

pub async fn get(Path(id): Path<String>) -> ApiResult {
    crate::workspaces::validate_id(&id).map_err(invalid)?;
    checked_view(
        super::clawd::request(Command::WorkspaceGet, json!({"id":id}))
            .await
            .map_err(super::clawd::RpcError::into_api_error)?,
    )
}

pub async fn control(Json(mut body): Json<Value>) -> ApiResult {
    let request = body
        .get_mut("request")
        .ok_or_else(|| invalid("missing Workspace action"))?;
    decimal_revision(request).map_err(invalid)?;
    let request: crate::workspaces::ControlRequest =
        serde_json::from_value(body).map_err(invalid)?;
    let id = request.request.id().map(str::to_string);
    let value = super::clawd::request(
        Command::WorkspaceControl,
        serde_json::to_value(request).map_err(invalid)?,
    )
    .await
    .map_err(super::clawd::RpcError::into_api_error)?;
    let view: WorkspaceView = serde_json::from_value(value).map_err(invalid)?;
    if id.as_ref().is_some_and(|id| *id != view.workspace.id) {
        return Err(invalid("Workspace acknowledgement identity mismatch"));
    }
    checked_view(serde_json::to_value(view).map_err(invalid)?)
}

pub async fn read(
    Path(id): Path<String>,
    Json(body): Json<crate::workspaces::ReadBody>,
) -> ApiResult {
    let request = body.into_request(id).map_err(invalid)?;
    let value = super::clawd::request(
        Command::WorkspaceRead,
        serde_json::to_value(request).map_err(invalid)?,
    )
    .await
    .map_err(super::clawd::RpcError::into_api_error)?;
    Ok(Json(project(value)))
}

pub async fn index_source(Path(id): Path<String>, Json(mut body): Json<Value>) -> ApiResult {
    crate::workspaces::validate_id(&id).map_err(invalid)?;
    decimal_revision(&mut body).map_err(invalid)?;
    let fields = body
        .as_object_mut()
        .ok_or_else(|| invalid("invalid index source request"))?;
    if fields.contains_key("id") {
        return Err(invalid("index source identity comes from the route"));
    }
    fields.insert("id".into(), json!(id));
    let request: crate::workspaces::IndexRequest = serde_json::from_value(body).map_err(invalid)?;
    crate::workspaces::validate_id(&request.tag_id).map_err(invalid)?;
    Ok(Json(
        super::clawd::request(
            Command::WorkspaceIndexSource,
            serde_json::to_value(request).map_err(invalid)?,
        )
        .await
        .map_err(super::clawd::RpcError::into_api_error)?,
    ))
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/agent/web/routes/workspaces.rs"
    ));
}
