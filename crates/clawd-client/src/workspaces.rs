//! Shared owner-facing Workspace definitions. These carry no execution grants.

use serde::{Deserialize, Serialize};

pub const SCHEMA_VERSION: u32 = 1;
pub const MAX_WORKSPACES: usize = 64;
pub const MAX_NOTES: usize = 64;
pub const MAX_TAGS: usize = 128;
pub const MAX_NAME_BYTES: usize = 240;
pub const MAX_CONTEXT_BYTES: usize = 8192;
pub const MAX_PATH_BYTES: usize = 4096;

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
    pub cleanup_pending: bool,
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
        timestamp(&self.created_at)?;
        timestamp(&self.updated_at)?;
        if self.cleanup_pending && self.state != WorkspaceState::Deleted {
            return Err("only a deleted Workspace may have pending cleanup".into());
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

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GetRequest {
    pub id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
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
            | Self::UnbindActivity { id, .. } => Some(id),
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
