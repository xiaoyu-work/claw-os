//! Typed human Workspace transport; no local context or file authority.

use super::*;
use cos_agent_protocol::{SessionCreateRequest, workspaces::*};

#[derive(Debug, Clone)]
pub enum ReadResult {
    Note(NotePreview),
    Source(SourcePage),
    Search(SearchResult),
    Index(IndexStatus),
}

pub(crate) async fn list(endpoint: BridgeEndpoint) -> Result<WorkspaceList> {
    let (request, version) = activity_request(&endpoint, reqwest::Method::GET, &["workspaces"])?;
    let result: WorkspaceList = activity_response(request, version).await?;
    result.validate().map_err(|error| anyhow!(error))?;
    Ok(result)
}
pub(crate) async fn get(endpoint: BridgeEndpoint, id: &str) -> Result<WorkspaceView> {
    validate_id(id).map_err(|error| anyhow!(error))?;
    let (request, version) =
        activity_request(&endpoint, reqwest::Method::GET, &["workspaces", id])?;
    let result: WorkspaceView = activity_response(request, version).await?;
    result.validate().map_err(|error| anyhow!(error))?;
    anyhow::ensure!(
        result.workspace.id == id,
        "Workspace view identity mismatch"
    );
    Ok(result)
}
pub(crate) async fn control(
    endpoint: BridgeEndpoint,
    action: WorkspaceAction,
) -> Result<WorkspaceView> {
    action.validate().map_err(|error| anyhow!(error))?;
    let id = action.id().map(str::to_string);
    let revision = action.expected_revision();
    let (request, version) =
        activity_request(&endpoint, reqwest::Method::POST, &["workspaces", "control"])?;
    let result: WorkspaceView =
        activity_response(request.json(&ControlRequest { request: action }), version).await?;
    result.validate().map_err(|error| anyhow!(error))?;
    anyhow::ensure!(
        id.is_none_or(|id| id == result.workspace.id)
            && revision.is_none_or(|revision| result.workspace.revision > revision),
        "Workspace mutation acknowledgement mismatch"
    );
    Ok(result)
}
pub(crate) async fn read(
    endpoint: BridgeEndpoint,
    id: &str,
    body: WorkspaceRead,
) -> Result<ReadResult> {
    read_bound(endpoint, id, body, None).await
}

pub(crate) async fn preview(
    endpoint: BridgeEndpoint,
    id: &str,
    body: WorkspaceRead,
    target: ReadTarget,
) -> Result<ReadResult> {
    read_bound(endpoint, id, body, Some(target)).await
}

async fn read_bound(
    endpoint: BridgeEndpoint,
    id: &str,
    body: WorkspaceRead,
    target: Option<ReadTarget>,
) -> Result<ReadResult> {
    ReadRequest {
        id: id.into(),
        request: body.clone(),
        target: target.clone(),
    }
    .validate()
    .map_err(|error| anyhow!(error))?;
    let (request, version) = activity_request(
        &endpoint,
        reqwest::Method::POST,
        &["workspaces", id, "read"],
    )?;
    let request = request.timeout(Duration::from_secs(125));
    let request = match target {
        Some(target) => request.json(&PreviewRequest {
            request: body.clone(),
            target,
        }),
        None => request.json(&body),
    };
    match body {
        WorkspaceRead::Note { note_id, .. } => {
            let result: NotePreview = activity_response(request, version).await?;
            result.validate().map_err(|error| anyhow!(error))?;
            anyhow::ensure!(
                result.note.id == note_id,
                "Workspace note preview identity mismatch"
            );
            Ok(ReadResult::Note(result))
        }
        WorkspaceRead::Source {
            path,
            offset,
            revision,
            ..
        } => {
            let result: SourcePage = activity_response(request, version).await?;
            result.validate().map_err(|error| anyhow!(error))?;
            anyhow::ensure!(
                result.path == path
                    && result.offset == offset
                    && revision.is_none_or(|revision| revision == result.revision),
                "Workspace source acknowledgement mismatch"
            );
            Ok(ReadResult::Source(result))
        }
        WorkspaceRead::Search { query, .. } => {
            let result: SearchResult = activity_response(request, version).await?;
            result.validate().map_err(|error| anyhow!(error))?;
            anyhow::ensure!(
                result.query == query,
                "Workspace query acknowledgement mismatch"
            );
            Ok(ReadResult::Search(result))
        }
        WorkspaceRead::Index => {
            let result: IndexStatus = activity_response(request, version).await?;
            result.validate().map_err(|error| anyhow!(error))?;
            Ok(ReadResult::Index(result))
        }
        _ => anyhow::bail!("Use the Workspace view for shared context and notes"),
    }
}
pub(crate) async fn index_source(
    endpoint: BridgeEndpoint,
    body: IndexRequest,
) -> Result<IndexSourceResult> {
    validate_id(&body.id).map_err(|error| anyhow!(error))?;
    validate_id(&body.tag_id).map_err(|error| anyhow!(error))?;
    let (request, version) = activity_request(
        &endpoint,
        reqwest::Method::POST,
        &["workspaces", "index-source"],
    )?;
    let result: IndexSourceResult = activity_response(request.json(&body), version).await?;
    result.validate().map_err(|error| anyhow!(error))?;
    Ok(result)
}
pub(crate) async fn conversation(
    endpoint: BridgeEndpoint,
    workspace_id: Option<String>,
) -> Result<SessionSummary> {
    if let Some(id) = workspace_id.as_deref() {
        validate_id(id).map_err(|error| anyhow!(error))?;
    }
    let (request, version) = activity_request(&endpoint, reqwest::Method::POST, &["sessions"])?;
    let result: SessionSummary = activity_response(
        request.json(&SessionCreateRequest {
            workspace_id: workspace_id.clone(),
        }),
        version,
    )
    .await?;
    anyhow::ensure!(
        result.workspace_id == workspace_id
            && result.id.starts_with("ses_")
            && result.manageable
            && !result.legacy,
        "Workspace conversation acknowledgement mismatch"
    );
    Ok(result)
}
