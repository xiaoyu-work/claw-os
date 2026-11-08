//! Root current-domain data reads. Sharing constrains data, not App authority.

use crate::agent::service::Job;
use crate::clawd::client_identity::FsIdentityGuard;
use crate::workspaces::gateway::{ContextOperation, Prelude, SemanticInput};
use crate::workspaces::{self, ReadTarget, WorkspaceRead, WorkspaceService, WorkspaceState};
use claw_embed::{local::LocalEmbedder, EmbedRequest, Embedder, SemanticStore};
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    io::Read,
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
    sync::{Mutex, OnceLock},
};

const MAX_SOURCE_BYTES: u64 = 50 * 1024 * 1024;

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/clawd/workspaces/reads.rs"
    ));
}

fn view(owner: u32, id: &str) -> Result<workspaces::WorkspaceView, String> {
    let view = workspaces::open_default()
        .and_then(|service| service.workspace(owner, id))
        .map_err(|e| e.to_string())?;
    if view.workspace.state != WorkspaceState::Active {
        return Err("Workspace is archived or deleted; data reads are closed".into());
    }
    Ok(view)
}

fn link_matches(link: &workspaces::ReadLink, job: Option<&Job>) -> bool {
    link_matches_for(link, job, None)
}

fn link_matches_for(
    link: &workspaces::ReadLink,
    job: Option<&Job>,
    conversation: Option<&crate::session::SessionMeta>,
) -> bool {
    let expiry = chrono::DateTime::parse_from_rfc3339(&link.expires_at);
    if expiry.is_err() || expiry.is_ok_and(|time| time <= chrono::Utc::now()) {
        return false;
    }
    match &link.target {
        ReadTarget::Job { id } => job.is_some_and(|job| id == &job.id),
        ReadTarget::Conversation { id } => {
            job.and_then(|job| job.session_id.as_deref())
                .or_else(|| conversation.map(|meta| meta.id.as_str()))
                == Some(id)
        }
    }
}

fn source_right(
    owner: u32,
    id: &str,
    path: &Path,
    link_id: Option<&str>,
    job: Option<&Job>,
) -> Result<String, String> {
    source_right_for(owner, id, path, link_id, job, None)
}

fn source_right_for(
    owner: u32,
    id: &str,
    path: &Path,
    link_id: Option<&str>,
    job: Option<&Job>,
    conversation: Option<&crate::session::SessionMeta>,
) -> Result<String, String> {
    let current = view(owner, id)?;
    if link_id.is_none() && current.tags.iter().any(|tag| tag.covers(path)) {
        return Ok(id.into());
    }
    let selected = current
        .workspace
        .read_links
        .iter()
        .find(|link| {
            link_matches_for(link, job, conversation)
                && link_id.is_none_or(|id| link.id == id)
                && link
                    .sources
                    .iter()
                    .any(|source| Path::new(&source.path) == path)
        })
        .ok_or("source is not shared with this exact task or conversation")?;
    let sid = job
        .and_then(|job| job.session_id.as_deref())
        .or_else(|| conversation.map(|meta| meta.id.as_str()))
        .ok_or("temporary context has no Root conversation")?
        .parse::<crate::session::SessionId>()
        .map_err(|e| e.to_string())?;
    if !workspaces::context_access::registered_link(&sid, selected)? {
        return Err("temporary context registration is incomplete; the read is closed".into());
    }
    let origin = view(owner, &selected.source_workspace_id)?;
    if !selected.sources.iter().any(|source| {
        Path::new(&source.path) == path
            && origin
                .tags
                .iter()
                .any(|tag| tag.id == source.tag_id && tag.covers(path))
    }) {
        return Err("temporary source selection was revoked or untagged".into());
    }
    Ok(origin.workspace.id)
}

pub(crate) fn filesystem_read(
    owner: u32,
    id: &str,
    scope: &crate::caps::Scope,
    job: Option<&Job>,
) -> Result<(), String> {
    let crate::caps::Scope::Path(path) = scope else {
        return Err("Workspace filesystem reads require an exact shared path".into());
    };
    let path = path.strip_suffix("/**").unwrap_or(path);
    if path.contains('*') || path.contains('?') {
        return Err("broad filesystem reads cannot escape Workspace source selection".into());
    }
    workspaces::validate_path(path)?;
    source_right(owner, id, Path::new(path), None, job)?;
    let home = crate::clawd::system_caps::verified_owner_home(owner)?;
    crate::worker::derive::reject_forbidden_for_owner(Path::new(path), &home)?;
    let _identity = FsIdentityGuard::enter(owner)?;
    let file = crate::clawd::owner_files::source(Path::new(path))?;
    let metadata = file.metadata().map_err(|e| e.to_string())?;
    if !metadata.is_file() && !metadata.is_dir() {
        return Err("shared source must be a regular file or directory".into());
    }
    if metadata.is_file() && metadata.nlink() != 1 {
        return Err("shared file gained an unsafe hard link".into());
    }
    Ok(())
}

fn same_file(left: &std::fs::Metadata, right: &std::fs::Metadata) -> bool {
    left.dev() == right.dev()
        && left.ino() == right.ino()
        && left.len() == right.len()
        && left.mtime() == right.mtime()
        && left.mtime_nsec() == right.mtime_nsec()
        && left.ctime() == right.ctime()
        && left.ctime_nsec() == right.ctime_nsec()
        && right.nlink() == 1
}

fn source_text(owner: u32, path: &Path, home: &Path) -> Result<(String, String), String> {
    workspaces::validate_path(path.to_str().ok_or("source path must be UTF-8")?)?;
    crate::worker::derive::reject_forbidden_for_owner(path, home)?;
    if !claw_embed::TextExtractor::supports(path) {
        return Err(
            "unsupported source format; Workspace V1 reads supported UTF-8 text only".into(),
        );
    }
    let _identity = FsIdentityGuard::enter(owner)?;
    let mut file = crate::clawd::owner_files::source(path)?;
    let before = file.metadata().map_err(|e| e.to_string())?;
    if !before.is_file() || before.nlink() != 1 || before.len() > MAX_SOURCE_BYTES {
        return Err("source must be a regular single-link file no larger than 50 MiB".into());
    }
    let mut bytes = Vec::new();
    (&mut file)
        .take(MAX_SOURCE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    let current = crate::clawd::owner_files::source(path)?;
    if bytes.len() as u64 > MAX_SOURCE_BYTES
        || !same_file(&before, &file.metadata().map_err(|e| e.to_string())?)
        || !same_file(&before, &current.metadata().map_err(|e| e.to_string())?)
    {
        return Err("source changed while being read; restart the read".into());
    }
    let revision = crate::crypto::sha256_hex(&bytes);
    let text = String::from_utf8(bytes).map_err(|_| "source is not UTF-8 text")?;
    Ok((text, revision))
}

fn prelude(owner: u32, id: &str, job: Option<&Job>) -> Result<Prelude, String> {
    let view = view(owner, id)?;
    let notes_total = view.notes.len();
    let tags_total = view.tags.len();
    let notes = view
        .notes
        .into_iter()
        .take(8)
        .map(|mut note| {
            note.content = note.content.chars().take(256).collect();
            note
        })
        .collect();
    let mut budget = 4096;
    let tags = view
        .tags
        .into_iter()
        .take(16)
        .take_while(|tag| {
            if tag.path.len() > budget {
                return false;
            }
            budget -= tag.path.len();
            true
        })
        .collect();
    let mut temporary_bytes = 4096usize;
    let temporary_reads = view
        .workspace
        .read_links
        .into_iter()
        .filter(|link| link_matches(link, job))
        .take(4)
        .map(|mut link| {
            link.note_ids.truncate(4);
            link.sources = link
                .sources
                .into_iter()
                .take(4)
                .take_while(|source| {
                    if source.path.len() > temporary_bytes {
                        return false;
                    }
                    temporary_bytes -= source.path.len();
                    true
                })
                .collect();
            link
        })
        .collect();
    Ok(Prelude {
        workspace_id: id.into(),
        name: view.workspace.name,
        revision: view.workspace.revision,
        background: view.workspace.context,
        notes,
        tags,
        notes_total,
        tags_total,
        temporary_reads,
    })
}

pub(super) async fn read(
    owner: u32,
    id: &str,
    request: WorkspaceRead,
    job: Option<&Job>,
) -> Result<Value, String> {
    read_bound(owner, id, request, job, None).await
}

pub(super) async fn read_conversation(
    owner: u32,
    id: &str,
    request: WorkspaceRead,
    conversation: &crate::session::SessionMeta,
) -> Result<Value, String> {
    read_bound(owner, id, request, None, Some(conversation)).await
}

async fn read_bound(
    owner: u32,
    id: &str,
    request: WorkspaceRead,
    job: Option<&Job>,
    conversation: Option<&crate::session::SessionMeta>,
) -> Result<Value, String> {
    request.validate()?;
    view(owner, id)?;
    match request {
        WorkspaceRead::Context => {
            serde_json::to_value(prelude(owner, id, job)?).map_err(|e| e.to_string())
        }
        WorkspaceRead::Notes { offset, limit } => {
            let view = view(owner, id)?;
            let total = view.notes.len();
            let notes = view
                .notes
                .into_iter()
                .skip(offset)
                .take(limit)
                .map(|mut note| {
                    note.content.clear();
                    note
                })
                .collect::<Vec<_>>();
            Ok(json!({"notes":notes,"total":total,"offset":offset,
                "next_offset":(offset+limit<total).then_some(offset+limit),"scope":"current_workspace_note_index"}))
        }
        WorkspaceRead::Resources { offset, limit } => {
            let view = view(owner, id)?;
            let total = view.tags.len();
            Ok(
                json!({"tags":view.tags.into_iter().skip(offset).take(limit).collect::<Vec<_>>(),
                "total":total,"offset":offset,"next_offset":(offset+limit<total).then_some(offset+limit)}),
            )
        }
        WorkspaceRead::Note { note_id, link_id } => {
            let current = view(owner, id)?;
            let (origin, imported) = if let Some(link_id) = link_id {
                let link = current
                    .workspace
                    .read_links
                    .iter()
                    .find(|link| {
                        link.id == link_id
                            && link_matches_for(link, job, conversation)
                            && link.note_ids.contains(&note_id)
                    })
                    .ok_or("note is not selected for this exact task or conversation")?;
                let sid = job
                    .and_then(|job| job.session_id.as_deref())
                    .or_else(|| conversation.map(|meta| meta.id.as_str()))
                    .ok_or("temporary context has no Root conversation")?
                    .parse::<crate::session::SessionId>()
                    .map_err(|e| e.to_string())?;
                if !workspaces::context_access::registered_link(&sid, link)? {
                    return Err(
                        "temporary context registration is incomplete; the read is closed".into(),
                    );
                }
                (view(owner, &link.source_workspace_id)?, true)
            } else {
                (current, false)
            };
            let note = origin
                .notes
                .into_iter()
                .find(|note| note.id == note_id)
                .ok_or("shared note was removed")?;
            Ok(
                json!({"note":note,"imported":imported,"source_workspace_id":origin.workspace.id,
                "evidence":"owner_note_or_caller_report_not_current_fact"}),
            )
        }
        WorkspaceRead::Source {
            path,
            link_id,
            offset,
            max_chars,
            revision,
        } => {
            let origin = source_right_for(
                owner,
                id,
                Path::new(&path),
                link_id.as_deref(),
                job,
                conversation,
            )?;
            let home = crate::clawd::system_caps::verified_owner_home(owner)?;
            let target = PathBuf::from(&path);
            let (text, actual) = crate::clawd::memory::run_as_owner(owner, move |_| {
                source_text(owner, &target, &home)
            })?;
            if revision
                .as_ref()
                .is_some_and(|revision| revision != &actual)
            {
                return Err("source revision changed; discard prior pages and restart".into());
            }
            source_right_for(
                owner,
                id,
                Path::new(&path),
                link_id.as_deref(),
                job,
                conversation,
            )?;
            let page = crate::agent::memory::history::text_page(&text, offset, max_chars)?;
            Ok(
                json!({"path":path,"source_workspace_id":origin,"imported":origin!=id,
                "source":"current_pinned_utf8_file","revision":actual,"content":page.content,
                "offset":page.offset,"next_offset":page.next_offset,"total_chars":page.total_chars,
                "source_complete":page.source_complete}),
            )
        }
        WorkspaceRead::Search { query, limit } => file_search(owner, id, query, limit, job).await,
        WorkspaceRead::Index => index_status(owner, id).await,
    }
}

struct QueryModel {
    owner: u32,
    model: LocalEmbedder,
}

async fn embedder(
    owner: u32,
) -> Result<tokio::sync::MutexGuard<'static, Option<QueryModel>>, String> {
    static MODEL: OnceLock<tokio::sync::Mutex<Option<QueryModel>>> = OnceLock::new();
    let mut slot = MODEL
        .get_or_init(|| tokio::sync::Mutex::new(None))
        .lock()
        .await;
    if slot.as_ref().is_some_and(|model| model.owner == owner) {
        return Ok(slot);
    }
    let home = crate::clawd::system_caps::verified_owner_home(owner)?;
    let gid = crate::agentd::spawn::resolve_identity(owner)?.gid;
    // One serialized Root query cache, not a worker per Workspace. Scopes and
    // source bytes are rechecked outside this slot, never cached with inference.
    drop(slot.take());
    *slot = Some(QueryModel {
        owner,
        model: LocalEmbedder::for_owner(owner, gid, home),
    });
    Ok(slot)
}

fn file_store(owner: u32, home: &Path) -> Result<Option<SemanticStore>, String> {
    let path = home.join(".local/state/claw-semantic/semantic.db");
    let _identity = FsIdentityGuard::enter(owner)?;
    let metadata = match std::fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.to_string()),
    };
    if !metadata.is_file()
        || metadata.file_type().is_symlink()
        || metadata.uid() != owner
        || metadata.nlink() != 1
    {
        return Err("unsafe owner file index database".into());
    }
    crate::clawd::owner_files::directory(path.parent().ok_or("file index has no directory")?)?;
    SemanticStore::open_read_only(&path)
        .map(Some)
        .map_err(|e| e.to_string())
}

struct FileSearchScope {
    workspace_id: String,
    tags: Vec<workspaces::ResourceTag>,
    temporary_sources: BTreeMap<PathBuf, String>,
}

impl FileSearchScope {
    fn load(owner: u32, id: &str, job: Option<&Job>) -> Result<Self, String> {
        let current = view(owner, id)?;
        let mut temporary_sources = BTreeMap::new();
        for link in current
            .workspace
            .read_links
            .iter()
            .filter(|link| link_matches(link, job))
        {
            let sid = job
                .and_then(|job| job.session_id.as_deref())
                .ok_or("temporary context has no Root conversation")?
                .parse::<crate::session::SessionId>()
                .map_err(|error| error.to_string())?;
            if !workspaces::context_access::registered_link(&sid, link)? {
                return Err(
                    "temporary context registration is incomplete; the read is closed".into(),
                );
            }
            let origin = view(owner, &link.source_workspace_id)?;
            for source in &link.sources {
                let path = Path::new(&source.path);
                if !origin
                    .tags
                    .iter()
                    .any(|tag| tag.id == source.tag_id && tag.covers(path))
                {
                    return Err("temporary source selection was revoked or untagged".into());
                }
                temporary_sources
                    .entry(path.to_path_buf())
                    .or_insert_with(|| origin.workspace.id.clone());
            }
        }
        Ok(Self {
            workspace_id: current.workspace.id,
            tags: current.tags,
            temporary_sources,
        })
    }

    fn origin(&self, path: &Path) -> Option<&str> {
        if self.tags.iter().any(|tag| tag.covers(path)) {
            Some(&self.workspace_id)
        } else {
            self.temporary_sources.get(path).map(String::as_str)
        }
    }
}

async fn file_search(
    owner: u32,
    id: &str,
    query: String,
    limit: usize,
    job: Option<&Job>,
) -> Result<Value, String> {
    let slot = embedder(owner).await?;
    let embedding = slot
        .as_ref()
        .ok_or("local query worker is unavailable")?
        .model
        .embed(EmbedRequest {
            inputs: vec![query.clone()],
        })
        .await
        .map_err(|e| e.to_string())?;
    drop(slot);
    claw_embed::local::validate_embeddings(&embedding, 1)?;
    let home = crate::clawd::system_caps::verified_owner_home(owner)?;
    // Root sharing/session stores are not readable under the owner's fsuid.
    // Resolve authority before owner I/O, then recheck it at Root before reply.
    let scope = FileSearchScope::load(owner, id, job)?;
    let id_owned = id.to_string();
    let vector = embedding
        .embeddings
        .into_iter()
        .next()
        .ok_or("local model returned no vector")?;
    let result = crate::clawd::memory::run_as_owner(owner, move |_| {
        let store = file_store(owner,&home)?.ok_or("global personal-file index is not ready; register a source and run the existing indexer")?;
        let revisions = std::cell::RefCell::new(BTreeMap::<String, Option<String>>::new());
        let hits = store
            .search_scoped_with_vector(
                &vector,
                limit,
                claw_embed::local::LOCAL_MODEL,
                &|ns, key| {
                    let Some(path) = ns.strip_prefix("file/") else {
                        return false;
                    };
                    if scope.origin(Path::new(path)).is_none() {
                        return false;
                    }
                    let mut revisions = revisions.borrow_mut();
                    let revision = revisions.entry(path.into()).or_insert_with(|| {
                        source_text(owner, Path::new(path), &home)
                            .ok()
                            .map(|(_, revision)| revision)
                    });
                    let Some(revision) = revision else {
                        return false;
                    };
                    let Some(ordinal) = key.strip_prefix(&format!("sha256:{revision}:")) else {
                        return false;
                    };
                    ordinal.parse::<u32>().is_ok()
                },
            )
            .map_err(|e| e.to_string())?;
        let mut results = Vec::new();
        for hit in hits {
            let path = hit
                .namespace
                .strip_prefix("file/")
                .ok_or("invalid file namespace")?;
            let origin = scope
                .origin(Path::new(path))
                .ok_or("source is outside the resolved Workspace scope")?;
            let (text, revision) = source_text(owner, Path::new(path), &home)?;
            if !hit.key.starts_with(&format!("sha256:{revision}:")) || !text.contains(&hit.text) {
                return Err("indexed source changed or snapshot does not match current file; reindex and search again".into());
            }
            results.push(json!({"path":path,"key":hit.key,"revision":revision,
                "snippet":hit.text.chars().take(512).collect::<String>(),"score":hit.score,
                "model":hit.model,"indexed_at_ms":hit.ts_ms,"source_workspace_id":origin,
                "imported":origin!=id_owned,"source":"indexed_snapshot_rechecked_against_current_file",
                "read":{"command":"source","path":path,"revision":revision,"offset":0}}));
        }
        Ok(results)
    })?;
    view(owner, id)?;
    for hit in &result {
        let path = hit
            .get("path")
            .and_then(Value::as_str)
            .ok_or("rechecked source result has no path")?;
        let origin = source_right(owner, id, Path::new(path), None, job)?;
        if hit.get("source_workspace_id").and_then(Value::as_str) != Some(origin.as_str()) {
            return Err("source sharing changed while searching; repeat the query".into());
        }
    }
    Ok(
        json!({"query":query,"hits":result,"limit":limit,"model":claw_embed::local::LOCAL_MODEL,
        "score_kind":"cosine_similarity_not_confidence","scope":"live_shared_sources_only",
        "coverage":"supported UTF-8 files in the independent global personal-file index; not all formats",
        "source_errors":"unsafe, unreadable and stale indexed sources are excluded before ranking"}),
    )
}

async fn index_status(owner: u32, id: &str) -> Result<Value, String> {
    let slot = embedder(owner).await?;
    let embedding = match slot
        .as_ref()
        .ok_or("local query worker is unavailable")?
        .model
        .status()
        .await
    {
        Ok(status) => serde_json::to_value(status).map_err(|e| e.to_string())?,
        Err(error) => {
            json!({"configured":false,"model":claw_embed::local::LOCAL_MODEL,"detail":error})
        }
    };
    drop(slot);
    let home = crate::clawd::system_caps::verified_owner_home(owner)?;
    let workspace = view(owner, id)?;
    let tags = workspace.tags;
    let corpus = crate::clawd::memory::run_as_owner(owner, move |_| {
        let Some(store) = file_store(owner, &home)? else {
            return Ok(json!({"indexed_sources":0,"indexed_chunks":0,"state":"not_indexed"}));
        };
        let mut sources = 0usize;
        let mut chunks = 0i64;
        for namespace in store.namespaces().map_err(|e| e.to_string())? {
            if namespace
                .strip_prefix("file/")
                .is_some_and(|path| tags.iter().any(|tag| tag.covers(Path::new(path))))
            {
                sources += 1;
                chunks += store.count(Some(&namespace)).map_err(|e| e.to_string())?;
            }
        }
        Ok(
            json!({"indexed_sources":sources,"indexed_chunks":chunks,"state":"indexed_metadata",
            "freshness":"not checked by diagnostics; every retrieval checks live source bytes"}),
        )
    })?;
    Ok(json!({"schema":1,"embedding":embedding,"corpus":corpus,
        "index_source_registration":"explicit owner action; global index coverage is independent of Workspace tags",
        "store":"owner default ~/.local/state/claw-semantic/semantic.db; custom XDG stores are not broker selectors",
        "formats":"supported UTF-8 text only"}))
}

fn memory_scope(
    owner: u32,
    workspace: Option<&str>,
    job: &Job,
) -> Result<(Vec<String>, bool), String> {
    let mut selected = Vec::new();
    for meta in crate::session::list().map_err(|e| e.to_string())? {
        if meta.owner_uid != Some(owner) {
            continue;
        }
        if workspace.is_some_and(|id| meta.workspace_id.as_deref() == Some(id)) {
            if workspaces::context_access::recallable(owner, &meta, job)? {
                selected.push(meta.id.to_string());
            }
        } else if workspace.is_none() && meta.workspace_id.is_some() {
            selected.push(meta.id.to_string());
        }
    }
    Ok((selected, workspace.is_none()))
}

pub(crate) async fn for_job(job: &Job, operation: ContextOperation) -> Result<Value, String> {
    operation.validate()?;
    workspaces::association::check_job(job)?;
    let owner = job
        .owner_uid
        .ok_or("context reads require a Root-bound owner")?;
    match operation {
        ContextOperation::Check => Ok(json!({"current":true})),
        ContextOperation::Workspace { request } => {
            read(
                owner,
                job.workspace_id.as_deref().ok_or("task has no Workspace")?,
                request,
                Some(job),
            )
            .await
        }
        ContextOperation::Report { note } => {
            let id = job.workspace_id.as_deref().ok_or("task has no Workspace")?;
            let sid = job
                .session_id
                .as_deref()
                .ok_or("Workspace report has no conversation")?
                .parse::<crate::session::SessionId>()
                .map_err(|e| e.to_string())?;
            if workspaces::context_access::has_temporary(&sid)? {
                return Err("temporary context cannot be automatically imported into shared findings; an owner can add an explicit note".into());
            }
            let store = crate::agent::service::Store::open_default().map_err(|e| e.to_string())?;
            let _context_lock = store.lock_context_admission().map_err(|e| e.to_string())?;
            workspaces::association::check_job(job)?;
            let service = workspaces::open_default().map_err(|e| e.to_string())?;
            let note = service
                .report_workspace(owner, id, &job.id, note)
                .map_err(|e| e.to_string())?;
            let revision = service
                .workspace(owner, id)
                .map_err(|e| e.to_string())?
                .workspace
                .revision;
            store
                .acknowledge_workspace_report(&job.id, revision)
                .map_err(|e| {
                    format!("Agent report persisted but Root context acknowledgement failed: {e}")
                })?;
            store
                .refresh_workspace_context(owner, id, revision)
                .map_err(|e| {
                    format!("Agent report persisted; active context refresh failed: {e}")
                })?;
            Ok(
                json!({"note":note,"source":"agent_report","evidence":"Root-bound Job provenance, not owner confirmation"}),
            )
        }
        ContextOperation::Recall { input } => {
            let (sessions, exclude) = memory_scope(owner, job.workspace_id.as_deref(), job)?;
            crate::clawd::memory::run_as_owner(owner, move |path| {
                let db = crate::clawd::memory::open_owner_db(&path, owner)?
                    .ok_or("conversation memory is not initialized")?
                    .with_session_scope(&sessions, exclude)
                    .map_err(|e| e.to_string())?;
                crate::agent::memory::scoped::recall(&db, input)
            })
        }
        ContextOperation::SemanticRecall { input } => semantic_recall(job, input).await,
    }
}

async fn semantic_recall(job: &Job, input: SemanticInput) -> Result<Value, String> {
    input.validate()?;
    let owner = job.owner_uid.ok_or("semantic reads require an owner")?;
    let (sessions, exclude) = memory_scope(owner, job.workspace_id.as_deref(), job)?;
    let namespace = input.namespace();
    let ns_filter = namespace.clone();
    let workspace_bound = job.workspace_id.is_some();
    let (store, db) = crate::clawd::memory::run_as_owner(owner, move |path| {
        let db = crate::clawd::memory::open_owner_db(&path, owner)?
            .ok_or("conversation memory is not initialized")?
            .with_session_scope(&sessions, exclude)
            .map_err(|e| e.to_string())?;
        let path = crate::paths::clawd_user_agent_state_dir(owner).join("semantic.db");
        let store = SemanticStore::open_read_only(path).map_err(|e| e.to_string())?;
        Ok((store, db))
    })?;
    let read_error = Mutex::new(None::<String>);
    let allowed = |ns: &str, key: &str| {
        if ns_filter.as_deref().is_some_and(|filter| filter != ns) {
            return false;
        }
        if let Some(session) = ns.strip_prefix("session/") {
            let Some((role, id)) = key.rsplit_once('-') else {
                return false;
            };
            let Ok(id) = id.parse::<i64>() else {
                return false;
            };
            return match db.replayable_message(id) {
                Ok(row) => row.is_some_and(|row| row.session_id == session && row.role == role),
                Err(error) => {
                    if let Ok(mut recorded) = read_error.lock() {
                        *recorded = Some(error.to_string());
                    }
                    false
                }
            };
        }
        !workspace_bound && ns.starts_with("app/")
    };
    match input.command.as_str() {
        "read" => {
            let ns = namespace
                .as_deref()
                .ok_or("semantic read requires namespace")?;
            let key = input.key.as_deref().ok_or("semantic read requires key")?;
            if !allowed(ns, key) {
                return Err("semantic source is outside this context domain".into());
            }
            let row = store
                .get(ns, key)
                .map_err(|e| e.to_string())?
                .ok_or("semantic source was removed")?;
            let text = if let Some(sid) = ns.strip_prefix("session/") {
                let (_, id) = key.rsplit_once('-').ok_or("invalid semantic message key")?;
                let source = db
                    .replayable_message(id.parse::<i64>().map_err(|e| e.to_string())?)
                    .map_err(|e| e.to_string())?
                    .filter(|source| source.session_id == sid)
                    .ok_or("canonical scoped source was removed")?;
                crate::agent::memory::history::sanitize_stored_content(
                    &source.role,
                    &source.content,
                )
            } else {
                row.text
            };
            let revision = crate::agent::memory::history::text_revision(&text);
            if input
                .revision
                .as_ref()
                .is_some_and(|expected| expected != &revision)
            {
                return Err("semantic source changed; restart the read".into());
            }
            let page =
                crate::agent::memory::history::text_page(&text, input.offset, input.max_chars)?;
            Ok(
                json!({"namespace":ns,"key":key,"text":page.content,"revision":revision,
                "next_offset":page.next_offset,"offset":page.offset,"total_chars":page.total_chars,
                "source_complete":page.source_complete,"source_kind":"scoped_canonical_source_window","indexed_at_ms":row.ts_ms}),
            )
        }
        "count" => {
            let mut count = 0i64;
            for ns in store.namespaces().map_err(|e| e.to_string())? {
                if namespace.as_deref().is_some_and(|filter| filter != ns) {
                    continue;
                }
                if let Some(sid) = ns.strip_prefix("session/") {
                    if db.count_session(sid).map_err(|e| e.to_string())? > 0 {
                        count += store.count(Some(&ns)).map_err(|e| e.to_string())?;
                    }
                } else if !workspace_bound && ns.starts_with("app/") {
                    count += store.count(Some(&ns)).map_err(|e| e.to_string())?;
                }
            }
            Ok(
                json!({"namespace":namespace,"count":count,"coverage":"scoped index rows; not proof of current evidence"}),
            )
        }
        "search" => {
            let home = crate::clawd::system_caps::verified_owner_home(owner)?;
            let config = crate::config::load_for_home(&home);
            let embed =
                crate::model::tasks::embed::build_from_with_agent(&config.embed, &config.agent)?
                    .ok_or(
                        "conversation semantic embedding is disabled; use scoped exact recall",
                    )?;
            let response = crate::paths::with_user_override(
                owner,
                home,
                embed.embed(EmbedRequest {
                    inputs: vec![input.query.clone()],
                }),
            )
            .await
            .map_err(|e| e.to_string())?;
            let vector = response
                .embeddings
                .first()
                .ok_or("semantic query returned no vector")?;
            let mut hits = store
                .search_scoped_with_vector(vector, input.limit, &response.model, &allowed)
                .map_err(|e| e.to_string())?;
            if let Some(error) = read_error
                .into_inner()
                .map_err(|_| "semantic validation error state was poisoned")?
            {
                return Err(error);
            }
            for hit in &mut hits {
                if let Some(sid) = hit.namespace.strip_prefix("session/") {
                    let (_, id) = hit
                        .key
                        .rsplit_once('-')
                        .ok_or("invalid semantic message key")?;
                    let source = db
                        .replayable_message(id.parse::<i64>().map_err(|e| e.to_string())?)
                        .map_err(|e| e.to_string())?
                        .filter(|source| source.session_id == sid)
                        .ok_or("canonical scoped source was removed")?;
                    hit.text = crate::agent::memory::history::sanitize_stored_content(
                        &source.role,
                        &source.content,
                    );
                }
            }
            Ok(
                json!({"query":input.query,"namespace":namespace,"limit":input.limit,
                "score_kind":"cosine_similarity_not_confidence","source_kind":"semantic_index_snapshot",
                "hits":hits.into_iter().map(|hit|json!({"namespace":hit.namespace,"key":hit.key,
                    "text":hit.text.chars().take(512).collect::<String>(),"revision":crate::agent::memory::history::text_revision(&hit.text),
                    "score":hit.score,"indexed_at_ms":hit.ts_ms,"model":hit.model,
                    "read":{"tool":"cos_recall_semantic","command":"read","namespace":hit.namespace,"key":hit.key}})).collect::<Vec<_>>() }),
            )
        }
        _ => Err("unsupported semantic read".into()),
    }
}
