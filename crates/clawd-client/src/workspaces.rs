//! Shared owner-facing Workspace definitions. These carry no execution grants.

use serde::{Deserialize, Serialize};

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/workspaces.rs"
    ));
}

pub const SCHEMA_VERSION: u32 = 1;
pub const MAX_WORKSPACES: usize = 64;
pub const MAX_NOTES: usize = 64;
pub const MAX_TAGS: usize = 128;
pub const MAX_NAME_BYTES: usize = 240;
pub const MAX_CONTEXT_BYTES: usize = 8192;
pub const MAX_PATH_BYTES: usize = 4096;
pub const MAX_READ_LINKS: usize = 16;
pub const MAX_LINK_SOURCES: usize = 16;
fn initial_access_revision() -> u64 {
    1
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceState {
    Active,
    Archived,
    Deleted,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceDraft {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub context: String,
}

impl WorkspaceDraft {
    pub fn validate(&self) -> Result<(), String> {
        text("Workspace name", &self.name, MAX_NAME_BYTES, false, false)?;
        text(
            "Workspace description",
            &self.description,
            MAX_CONTEXT_BYTES,
            true,
            true,
        )?;
        text(
            "Workspace context",
            &self.context,
            MAX_CONTEXT_BYTES,
            true,
            true,
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Workspace {
    pub id: String,
    pub name: String,
    pub description: String,
    pub context: String,
    pub state: WorkspaceState,
    pub revision: u64,
    #[serde(default = "initial_access_revision")]
    pub access_revision: u64,
    pub cleanup_pending: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub read_links: Vec<ReadLink>,
    pub created_at: String,
    pub updated_at: String,
}

impl Workspace {
    pub fn validate(&self) -> Result<(), String> {
        validate_id(&self.id)?;
        WorkspaceDraft {
            name: self.name.clone(),
            description: self.description.clone(),
            context: self.context.clone(),
        }
        .validate()?;
        revision(self.revision)?;
        revision(self.access_revision)?;
        timestamp(&self.created_at)?;
        timestamp(&self.updated_at)?;
        if self.cleanup_pending && self.state != WorkspaceState::Deleted {
            return Err("only a deleted Workspace may have pending cleanup".into());
        }
        if self.read_links.len() > MAX_READ_LINKS {
            return Err("too many temporary Workspace reads".into());
        }
        let mut ids = std::collections::BTreeSet::new();
        for link in &self.read_links {
            link.validate()?;
            if link.source_workspace_id == self.id || !ids.insert(&link.id) {
                return Err("invalid temporary Workspace read identity".into());
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NoteSource {
    OwnerNote,
    AgentReport,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NoteDraft {
    pub title: String,
    pub content: String,
}

impl NoteDraft {
    pub fn validate(&self) -> Result<(), String> {
        text("note title", &self.title, MAX_NAME_BYTES, false, false)?;
        text(
            "note content",
            &self.content,
            MAX_CONTEXT_BYTES,
            false,
            true,
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceNote {
    pub id: String,
    pub workspace_id: String,
    pub title: String,
    pub content: String,
    pub source: NoteSource,
    pub source_job_id: Option<String>,
    pub revision: u64,
    pub created_at: String,
    pub updated_at: String,
}

impl WorkspaceNote {
    pub fn validate(&self) -> Result<(), String> {
        validate_id(&self.id)?;
        validate_id(&self.workspace_id)?;
        NoteDraft {
            title: self.title.clone(),
            content: self.content.clone(),
        }
        .validate()?;
        revision(self.revision)?;
        timestamp(&self.created_at)?;
        timestamp(&self.updated_at)?;
        match (&self.source, &self.source_job_id) {
            (NoteSource::OwnerNote, None) => Ok(()),
            (NoteSource::AgentReport, Some(id)) => validate_id(id),
            _ => Err("Workspace note source and task provenance disagree".into()),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResourceKind {
    File,
    Directory,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResourceTag {
    pub id: String,
    pub workspace_id: String,
    pub path: String,
    pub kind: ResourceKind,
    pub created_at: String,
}

impl ResourceTag {
    pub fn validate(&self) -> Result<(), String> {
        validate_id(&self.id)?;
        validate_id(&self.workspace_id)?;
        validate_path(&self.path)?;
        timestamp(&self.created_at)
    }

    pub fn covers(&self, path: &std::path::Path) -> bool {
        let root = std::path::Path::new(&self.path);
        match self.kind {
            ResourceKind::File => path == root,
            ResourceKind::Directory => path.starts_with(root),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ReadTarget {
    Job { id: String },
    Conversation { id: String },
}

impl ReadTarget {
    pub fn validate(&self) -> Result<(), String> {
        match self {
            Self::Job { id } => validate_id(id),
            Self::Conversation { id } => text("conversation id", id, 128, false, false),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SelectedSource {
    pub tag_id: String,
    pub path: String,
}

impl SelectedSource {
    pub fn validate(&self) -> Result<(), String> {
        validate_id(&self.tag_id)?;
        validate_path(&self.path)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReadLink {
    pub id: String,
    pub source_workspace_id: String,
    pub target: ReadTarget,
    pub note_ids: Vec<String>,
    pub sources: Vec<SelectedSource>,
    pub expires_at: String,
    pub created_at: String,
}

impl ReadLink {
    pub fn validate(&self) -> Result<(), String> {
        validate_id(&self.id)?;
        validate_id(&self.source_workspace_id)?;
        self.target.validate()?;
        validate_selection(&self.note_ids, &self.sources)?;
        timestamp(&self.expires_at)?;
        timestamp(&self.created_at)
    }
}

pub fn validate_selection(notes: &[String], sources: &[SelectedSource]) -> Result<(), String> {
    if notes.len() + sources.len() == 0 || notes.len() + sources.len() > MAX_LINK_SOURCES {
        return Err("select 1-16 exact notes or files for a temporary read".into());
    }
    let mut ids = std::collections::BTreeSet::new();
    for id in notes {
        validate_id(id)?;
        if !ids.insert(id) {
            return Err("duplicate selected note".into());
        }
    }
    let mut paths = std::collections::BTreeSet::new();
    for source in sources {
        source.validate()?;
        if !paths.insert(&source.path) {
            return Err("duplicate selected file".into());
        }
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "command", rename_all = "snake_case", deny_unknown_fields)]
pub enum WorkspaceRead {
    Context,
    Notes {
        #[serde(default)]
        offset: usize,
        #[serde(default = "default_search_limit")]
        limit: usize,
    },
    Resources {
        #[serde(default)]
        offset: usize,
        #[serde(default = "default_search_limit")]
        limit: usize,
    },
    Note {
        note_id: String,
        #[serde(default)]
        link_id: Option<String>,
    },
    Source {
        path: String,
        #[serde(default)]
        link_id: Option<String>,
        #[serde(default)]
        offset: usize,
        #[serde(default = "default_read_chars")]
        max_chars: usize,
        #[serde(default)]
        revision: Option<String>,
    },
    Search {
        query: String,
        #[serde(default = "default_search_limit")]
        limit: usize,
    },
    Index,
}

impl<'de> Deserialize<'de> for WorkspaceRead {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        // Empty struct variants reject extra selectors; serde unit variants ignore them.
        #[derive(Deserialize)]
        #[serde(tag = "command", rename_all = "snake_case", deny_unknown_fields)]
        enum Raw {
            Context {},
            Notes {
                #[serde(default)]
                offset: usize,
                #[serde(default = "default_search_limit")]
                limit: usize,
            },
            Resources {
                #[serde(default)]
                offset: usize,
                #[serde(default = "default_search_limit")]
                limit: usize,
            },
            Note {
                note_id: String,
                #[serde(default)]
                link_id: Option<String>,
            },
            Source {
                path: String,
                #[serde(default)]
                link_id: Option<String>,
                #[serde(default)]
                offset: usize,
                #[serde(default = "default_read_chars")]
                max_chars: usize,
                #[serde(default)]
                revision: Option<String>,
            },
            Search {
                query: String,
                #[serde(default = "default_search_limit")]
                limit: usize,
            },
            Index {},
        }
        Ok(match Raw::deserialize(deserializer)? {
            Raw::Context {} => Self::Context,
            Raw::Notes { offset, limit } => Self::Notes { offset, limit },
            Raw::Resources { offset, limit } => Self::Resources { offset, limit },
            Raw::Note { note_id, link_id } => Self::Note { note_id, link_id },
            Raw::Source {
                path,
                link_id,
                offset,
                max_chars,
                revision,
            } => Self::Source {
                path,
                link_id,
                offset,
                max_chars,
                revision,
            },
            Raw::Search { query, limit } => Self::Search { query, limit },
            Raw::Index {} => Self::Index,
        })
    }
}

fn default_read_chars() -> usize {
    4096
}
fn default_search_limit() -> usize {
    10
}

impl WorkspaceRead {
    pub fn link_id(&self) -> Option<&str> {
        match self {
            Self::Note { link_id, .. } | Self::Source { link_id, .. } => link_id.as_deref(),
            _ => None,
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        match self {
            Self::Context | Self::Index => Ok(()),
            Self::Notes { offset, limit } | Self::Resources { offset, limit } => {
                if *offset > MAX_TAGS || !(1..=16).contains(limit) {
                    return Err("resource index page exceeds its bounds".into());
                }
                Ok(())
            }
            Self::Note { note_id, link_id } => {
                validate_id(note_id)?;
                link_id.as_deref().map(validate_id).transpose().map(|_| ())
            }
            Self::Source {
                path,
                link_id,
                offset,
                max_chars,
                revision,
            } => {
                validate_path(path)?;
                if let Some(id) = link_id {
                    validate_id(id)?;
                }
                if !(1..=16384).contains(max_chars) || *offset > 50 * 1024 * 1024 {
                    return Err("source read window exceeds its bounds".into());
                }
                if *offset > 0 && revision.is_none() {
                    return Err("continued reads require the displayed source revision".into());
                }
                if revision
                    .as_ref()
                    .is_some_and(|r| r.len() != 64 || !r.bytes().all(|b| b.is_ascii_hexdigit()))
                {
                    return Err("invalid source revision".into());
                }
                Ok(())
            }
            Self::Search { query, limit } => {
                text("search query", query, 2048, false, true)?;
                if !(1..=50).contains(limit) {
                    return Err("search limit must be 1-50".into());
                }
                Ok(())
            }
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ReadRequest {
    pub id: String,
    pub request: WorkspaceRead,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<ReadTarget>,
}

impl ReadRequest {
    pub fn validate(&self) -> Result<(), String> {
        validate_id(&self.id)?;
        self.request.validate()?;
        match (&self.target, self.request.link_id()) {
            (Some(target), Some(_)) => target.validate(),
            (None, None) => Ok(()),
            _ => Err(
                "temporary preview requires --link and exactly one Job or conversation target"
                    .into(),
            ),
        }
    }
}

impl<'de> Deserialize<'de> for ReadRequest {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Raw {
            id: String,
            request: WorkspaceRead,
            #[serde(default)]
            target: Option<ReadTarget>,
        }
        let raw = Raw::deserialize(deserializer)?;
        let request = Self {
            id: raw.id,
            request: raw.request,
            target: raw.target,
        };
        request.validate().map_err(serde::de::Error::custom)?;
        Ok(request)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PreviewRequest {
    pub request: WorkspaceRead,
    pub target: ReadTarget,
}

/// Human HTTP compatibility only. Private Agent frames still carry WorkspaceRead.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum ReadBody {
    Preview(PreviewRequest),
    Direct(WorkspaceRead),
}

impl ReadBody {
    pub fn into_request(self, id: String) -> Result<ReadRequest, String> {
        let request = match self {
            Self::Preview(preview) => ReadRequest {
                id,
                request: preview.request,
                target: Some(preview.target),
            },
            Self::Direct(request) => ReadRequest {
                id,
                request,
                target: None,
            },
        };
        request.validate()?;
        Ok(request)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IndexRequest {
    pub id: String,
    pub expected_revision: u64,
    pub tag_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceView {
    pub schema: u32,
    pub workspace: Workspace,
    pub notes: Vec<WorkspaceNote>,
    pub tags: Vec<ResourceTag>,
    pub activity_ids: Vec<String>,
}

impl WorkspaceView {
    pub fn validate(&self) -> Result<(), String> {
        if self.schema != SCHEMA_VERSION
            || self.notes.len() > MAX_NOTES
            || self.tags.len() > MAX_TAGS
        {
            return Err("unsupported or oversized Workspace view".into());
        }
        self.workspace.validate()?;
        if self.activity_ids.len() > 1000 {
            return Err("too many Workspace Activities".into());
        }
        let mut ids = std::collections::BTreeSet::new();
        for note in &self.notes {
            note.validate()?;
            if note.workspace_id != self.workspace.id || !ids.insert(&note.id) {
                return Err("Workspace note identity mismatch".into());
            }
        }
        ids.clear();
        for tag in &self.tags {
            tag.validate()?;
            if tag.workspace_id != self.workspace.id || !ids.insert(&tag.id) {
                return Err("Workspace tag identity mismatch".into());
            }
        }
        ids.clear();
        for id in &self.activity_ids {
            validate_id(id)?;
            if !ids.insert(id) {
                return Err("duplicate Workspace Activity".into());
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceList {
    pub schema: u32,
    pub workspaces: Vec<Workspace>,
}

impl WorkspaceList {
    pub fn validate(&self) -> Result<(), String> {
        if self.schema != SCHEMA_VERSION || self.workspaces.len() > MAX_WORKSPACES {
            return Err("invalid Workspace list".into());
        }
        let mut ids = std::collections::BTreeSet::new();
        for workspace in &self.workspaces {
            workspace.validate()?;
            if !ids.insert(&workspace.id) {
                return Err("duplicate Workspace identity".into());
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourcePage {
    pub path: String,
    pub source_workspace_id: String,
    pub imported: bool,
    pub source: String,
    pub revision: String,
    pub content: String,
    pub offset: usize,
    pub next_offset: Option<usize>,
    pub total_chars: usize,
    pub source_complete: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NotePreview {
    pub note: WorkspaceNote,
    pub imported: bool,
    pub source_workspace_id: String,
    pub evidence: String,
}

impl NotePreview {
    pub fn validate(&self) -> Result<(), String> {
        self.note.validate()?;
        validate_id(&self.source_workspace_id)?;
        if self.note.workspace_id != self.source_workspace_id
            || self.evidence != "owner_note_or_caller_report_not_current_fact"
        {
            return Err("invalid sourced note preview".into());
        }
        Ok(())
    }
}

impl SourcePage {
    pub fn validate(&self) -> Result<(), String> {
        validate_path(&self.path)?;
        validate_id(&self.source_workspace_id)?;
        digest(&self.revision)?;
        if self.source != "current_pinned_utf8_file"
            || self.content.len() > 65536
            || self.total_chars > 50 * 1024 * 1024
            || self.offset > self.total_chars
            || self
                .next_offset
                .is_some_and(|offset| offset <= self.offset || offset >= self.total_chars)
            || (self.source_complete && (self.offset != 0 || self.next_offset.is_some()))
        {
            return Err("invalid current-source page".into());
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceReadHandle {
    pub command: String,
    pub path: String,
    pub revision: String,
    pub offset: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SearchHit {
    pub path: String,
    pub key: String,
    pub revision: String,
    pub snippet: String,
    pub score: f32,
    pub model: String,
    pub indexed_at_ms: i64,
    pub source_workspace_id: String,
    pub imported: bool,
    pub source: String,
    pub read: SourceReadHandle,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SearchResult {
    pub query: String,
    pub hits: Vec<SearchHit>,
    pub limit: usize,
    pub model: String,
    pub score_kind: String,
    pub scope: String,
    pub coverage: String,
    pub source_errors: String,
}

impl SearchResult {
    pub fn validate(&self) -> Result<(), String> {
        text("Workspace query", &self.query, 2048, false, true)?;
        if !(1..=50).contains(&self.limit)
            || self.hits.len() > self.limit
            || self.model != "Qwen3-Embedding-0.6B"
            || self.score_kind != "cosine_similarity_not_confidence"
            || self.scope != "live_shared_sources_only"
        {
            return Err("invalid scoped file search".into());
        }
        text("coverage", &self.coverage, 8192, false, true)?;
        text("source errors", &self.source_errors, 8192, false, true)?;
        for hit in &self.hits {
            validate_path(&hit.path)?;
            validate_id(&hit.source_workspace_id)?;
            digest(&hit.revision)?;
            if hit.key.len() > 128
                || !hit.key.starts_with(&format!("sha256:{}:", hit.revision))
                || !hit
                    .key
                    .rsplit(':')
                    .next()
                    .is_some_and(|id| id.parse::<u32>().is_ok())
                || hit.snippet.len() > 2048
                || !hit.score.is_finite()
                || hit.model != self.model
                || hit.indexed_at_ms < 0
                || hit.source != "indexed_snapshot_rechecked_against_current_file"
                || hit.read.command != "source"
                || hit.read.path != hit.path
                || hit.read.revision != hit.revision
                || hit.read.offset != 0
            {
                return Err("invalid rechecked file search hit".into());
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IndexEmbedding {
    pub configured: bool,
    pub model: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dim: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IndexCorpus {
    pub indexed_sources: u64,
    pub indexed_chunks: u64,
    pub state: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub freshness: Option<String>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IndexStatus {
    pub schema: u32,
    pub embedding: IndexEmbedding,
    pub corpus: IndexCorpus,
    pub index_source_registration: String,
    pub store: String,
    pub formats: String,
}
impl IndexStatus {
    pub fn validate(&self) -> Result<(), String> {
        if self.schema != 1
            || self.embedding.model != "Qwen3-Embedding-0.6B"
            || self.embedding.dim.is_some_and(|dim| dim != 1024)
            || (self.embedding.configured && self.embedding.dim != Some(1024))
            || !matches!(
                self.corpus.state.as_str(),
                "not_indexed" | "indexed_metadata"
            )
        {
            return Err("invalid local index diagnostic".into());
        }
        for value in [&self.index_source_registration, &self.store, &self.formats] {
            text("index diagnostic", value, 8192, false, true)?;
        }
        if let Some(detail) = &self.embedding.detail {
            text("index diagnostic", detail, 8192, true, true)?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IndexSourceResult {
    pub configuration_saved: bool,
    pub directory: String,
    pub coverage: String,
    pub indexing: String,
}
impl IndexSourceResult {
    pub fn validate(&self) -> Result<(), String> {
        validate_path(&self.directory)?;
        if !self.configuration_saved {
            return Err("index registration was not acknowledged".into());
        }
        text("index coverage", &self.coverage, 8192, false, true)?;
        text("indexing", &self.indexing, 8192, false, true)
    }
}

fn digest(value: &str) -> Result<(), String> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err("invalid source revision".into());
    }
    Ok(())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GetRequest {
    pub id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum WorkspaceAction {
    Create {
        draft: WorkspaceDraft,
    },
    Update {
        id: String,
        expected_revision: u64,
        draft: WorkspaceDraft,
    },
    Archive {
        id: String,
        expected_revision: u64,
        archived: bool,
    },
    Delete {
        id: String,
        expected_revision: u64,
        confirm: bool,
    },
    Tag {
        id: String,
        expected_revision: u64,
        path: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        kind: Option<ResourceKind>,
    },
    Untag {
        id: String,
        expected_revision: u64,
        tag_id: String,
    },
    PutNote {
        id: String,
        expected_revision: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        note_id: Option<String>,
        note: NoteDraft,
    },
    ForgetNote {
        id: String,
        expected_revision: u64,
        note_id: String,
    },
    BindActivity {
        id: String,
        expected_revision: u64,
        activity_id: String,
    },
    UnbindActivity {
        id: String,
        expected_revision: u64,
        activity_id: String,
    },
    ReadLink {
        id: String,
        expected_revision: u64,
        source_workspace_id: String,
        target: ReadTarget,
        note_ids: Vec<String>,
        sources: Vec<SelectedSource>,
        expires_at: String,
    },
    RevokeLink {
        id: String,
        expected_revision: u64,
        link_id: String,
    },
}

impl WorkspaceAction {
    pub fn id(&self) -> Option<&str> {
        match self {
            Self::Create { .. } => None,
            Self::Update { id, .. }
            | Self::Archive { id, .. }
            | Self::Delete { id, .. }
            | Self::Tag { id, .. }
            | Self::Untag { id, .. }
            | Self::PutNote { id, .. }
            | Self::ForgetNote { id, .. }
            | Self::BindActivity { id, .. }
            | Self::UnbindActivity { id, .. }
            | Self::ReadLink { id, .. }
            | Self::RevokeLink { id, .. } => Some(id),
        }
    }

    pub fn expected_revision(&self) -> Option<u64> {
        match self {
            Self::Create { .. } => None,
            Self::Update {
                expected_revision, ..
            }
            | Self::Archive {
                expected_revision, ..
            }
            | Self::Delete {
                expected_revision, ..
            }
            | Self::Tag {
                expected_revision, ..
            }
            | Self::Untag {
                expected_revision, ..
            }
            | Self::PutNote {
                expected_revision, ..
            }
            | Self::ForgetNote {
                expected_revision, ..
            }
            | Self::BindActivity {
                expected_revision, ..
            }
            | Self::UnbindActivity {
                expected_revision, ..
            }
            | Self::ReadLink {
                expected_revision, ..
            }
            | Self::RevokeLink {
                expected_revision, ..
            } => Some(*expected_revision),
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        if let Some(id) = self.id() {
            validate_id(id)?;
            revision(
                self.expected_revision()
                    .ok_or("missing Workspace revision")?,
            )?;
        }
        match self {
            Self::Create { draft } | Self::Update { draft, .. } => draft.validate(),
            Self::Tag { path, .. } => validate_path(path),
            Self::Untag { tag_id, .. } => validate_id(tag_id),
            Self::PutNote { note_id, note, .. } => {
                if let Some(id) = note_id {
                    validate_id(id)?;
                }
                note.validate()
            }
            Self::ForgetNote { note_id, .. } => validate_id(note_id),
            Self::BindActivity { activity_id, .. } | Self::UnbindActivity { activity_id, .. } => {
                validate_id(activity_id)
            }
            Self::Delete { confirm: false, .. } => {
                Err("Workspace deletion requires confirmation".into())
            }
            Self::ReadLink {
                id,
                source_workspace_id,
                target,
                note_ids,
                sources,
                expires_at,
                ..
            } => {
                validate_id(source_workspace_id)?;
                if id == source_workspace_id {
                    return Err("temporary reads must select another Workspace".into());
                }
                target.validate()?;
                validate_selection(note_ids, sources)?;
                timestamp(expires_at)
            }
            Self::RevokeLink { link_id, .. } => validate_id(link_id),
            _ => Ok(()),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ControlRequest {
    pub request: WorkspaceAction,
}

impl<'de> Deserialize<'de> for ControlRequest {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Raw {
            request: WorkspaceAction,
        }
        let raw = Raw::deserialize(deserializer)?;
        raw.request.validate().map_err(serde::de::Error::custom)?;
        Ok(Self {
            request: raw.request,
        })
    }
}

pub fn validate_id(value: &str) -> Result<(), String> {
    let id = uuid::Uuid::parse_str(value).map_err(|_| "Workspace identifier must be a UUID")?;
    if id.to_string() != value {
        return Err("Workspace identifiers must use canonical UUID spelling".into());
    }
    Ok(())
}

pub fn validate_path(value: &str) -> Result<(), String> {
    text(
        "Workspace resource path",
        value,
        MAX_PATH_BYTES,
        false,
        false,
    )?;
    if !value.starts_with('/')
        || value == "/"
        || value.split('/').any(|part| part == "." || part == "..")
    {
        return Err("Workspace resource path must be an absolute, normalized Linux path".into());
    }
    Ok(())
}

fn revision(value: u64) -> Result<(), String> {
    if value == 0 || value > i64::MAX as u64 {
        return Err("Workspace revision must be a positive SQLite integer".into());
    }
    Ok(())
}

fn timestamp(value: &str) -> Result<(), String> {
    chrono::DateTime::parse_from_rfc3339(value)
        .map(|_| ())
        .map_err(|_| "invalid Workspace timestamp".into())
}

fn text(field: &str, value: &str, max: usize, empty: bool, multiline: bool) -> Result<(), String> {
    if (!empty && value.trim().is_empty())
        || value.len() > max
        || value
            .chars()
            .any(|c| c.is_control() && !(multiline && matches!(c, '\n' | '\r' | '\t')))
    {
        return Err(format!(
            "{field} must be bounded text of at most {max} UTF-8 bytes"
        ));
    }
    Ok(())
}
