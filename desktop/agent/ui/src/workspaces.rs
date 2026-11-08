//! Human Workspace reducer and Fluent widgets over one canonical service.

use crate::{Message as AppMessage, fl};
use cos_agent_protocol::workspaces::*;
use cosmic::{
    Element,
    widget::{self, Column, Row, button, scrollable, text},
};

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/workspaces.rs"
    ));
}

fn input<'a>(
    label: String,
    value: &'a str,
    map: fn(String) -> Message,
) -> cosmic::widget::TextInput<'a, AppMessage> {
    widget::text_input(label, value).on_input(move |value| AppMessage::Workspaces(map(value)))
}

#[derive(Debug, Clone)]
pub enum Message {
    Show,
    Close,
    Refresh,
    Select(String),
    Create,
    Save,
    ReviewRevision,
    Name(String),
    Description(String),
    Context(String),
    CreateName(String),
    TagPath(String),
    PickTag(bool),
    Tag,
    RemoveTag(String),
    NoteTitle(String),
    NoteContent(String),
    AddNote,
    ForgetNote(String),
    Activity(String),
    AttachActivity,
    DetachActivity(String),
    Query(String),
    Search,
    SourcePath(String),
    ReadSource,
    ReadHit(String, String),
    NextSource,
    Index,
    RegisterIndex(String),
    BeginDelete,
    ConfirmDelete,
    CancelDelete,
    Archive(bool),
    LinkDraft(String),
    AllowLink,
    RevokeLink(String),
    PreviewNote(String, String),
    PreviewSource(String, String),
    InspectOrigin(String),
    FileChosen(u64, Result<Option<String>, String>),
    Fetched(u64, Result<Box<Response>, String>),
}
#[derive(Debug, Clone)]
pub(crate) enum Action {
    List,
    Get(String),
    Origin(String),
    Control(WorkspaceAction),
    Read(String, WorkspaceRead),
    Preview(String, WorkspaceRead, ReadTarget),
    Index(IndexRequest),
}
pub(crate) struct Request {
    pub generation: u64,
    pub action: Action,
}
#[derive(Debug, Clone)]
pub enum Response {
    List(WorkspaceList),
    View(WorkspaceView),
    Origin(WorkspaceView),
    Read(crate::bridge::workspaces::ReadResult),
    Index(IndexSourceResult),
}
pub(crate) struct Workspaces {
    visible: bool,
    generation: u64,
    busy: bool,
    mutation: bool,
    error: Option<String>,
    notice: Option<String>,
    records: Vec<Workspace>,
    selected: Option<String>,
    view: Option<WorkspaceView>,
    origin: Option<WorkspaceView>,
    draft: WorkspaceDraft,
    draft_revision: Option<u64>,
    dirty: bool,
    create_name: String,
    tag_path: String,
    note_title: String,
    note_content: String,
    activity: String,
    query: String,
    source_path: String,
    source: Option<SourcePage>,
    source_preview: Option<(String, ReadTarget)>,
    note_preview: Option<NotePreview>,
    search: Option<SearchResult>,
    index: Option<IndexStatus>,
    delete: Option<Workspace>,
    link_draft: String,
    pending_save: bool,
}
impl Default for Workspaces {
    fn default() -> Self {
        Self{visible:false,generation:0,busy:false,mutation:false,error:None,notice:None,
        records:Vec::new(),selected:None,view:None,origin:None,draft:WorkspaceDraft{name:String::new(),description:String::new(),context:String::new()},
        draft_revision:None,dirty:false,create_name:String::new(),tag_path:String::new(),note_title:String::new(),note_content:String::new(),
        activity:String::new(),query:String::new(),source_path:String::new(),source:None,source_preview:None,note_preview:None,search:None,index:None,delete:None,
        link_draft:r#"{"source_workspace_id":"UUID","target":{"kind":"conversation","id":"ses_..."},"note_ids":["UUID"],"sources":[],"expires_at":"RFC3339"}"#.into(),pending_save:false}
    }
}
impl Workspaces {
    pub(crate) fn is_visible(&self) -> bool {
        self.visible
    }
    pub(crate) fn selected(&self) -> Option<&str> {
        self.selected.as_deref()
    }
    pub(crate) fn select_for_chat(&mut self, id: Option<String>) {
        self.selected = id;
        if self.selected.is_none() {
            self.view = None;
        }
    }
    pub(crate) fn hide(&mut self) {
        self.visible = false;
        if !self.mutation {
            self.generation = self.generation.wrapping_add(1);
            self.busy = false;
        }
    }
    fn adopt(&mut self, view: WorkspaceView) {
        if !self.dirty {
            self.draft = WorkspaceDraft {
                name: view.workspace.name.clone(),
                description: view.workspace.description.clone(),
                context: view.workspace.context.clone(),
            };
            self.draft_revision = Some(view.workspace.revision);
        }
        self.selected = Some(view.workspace.id.clone());
        self.records.retain(|record| record.id != view.workspace.id);
        if view.workspace.state != WorkspaceState::Deleted {
            self.records.push(view.workspace.clone());
        }
        self.view = Some(view);
        self.source = None;
        self.source_preview = None;
        self.note_preview = None;
        self.search = None;
        self.index = None;
        self.origin = None;
        self.delete = None;
    }
    pub(crate) fn update(&mut self, message: Message, connected: bool) -> Option<Request> {
        if let Message::Fetched(generation, result) = message {
            if generation != self.generation {
                return None;
            }
            self.busy = false;
            self.mutation = false;
            match result {
                Ok(response) => {
                    self.error = None;
                    match *response {
                        Response::List(list) => self.records = list.workspaces,
                        Response::View(view) => {
                            if self.pending_save {
                                self.dirty = false;
                            }
                            self.notice = Some(if view.workspace.cleanup_pending {
                                fl!("spaces-cleanup-pending")
                            } else {
                                fl!("spaces-acknowledged")
                            });
                            self.adopt(view);
                        }
                        Response::Origin(view) => self.origin = Some(view),
                        Response::Read(crate::bridge::workspaces::ReadResult::Source(page)) => {
                            self.source = Some(page)
                        }
                        Response::Read(crate::bridge::workspaces::ReadResult::Note(note)) => {
                            self.note_preview = Some(note)
                        }
                        Response::Read(crate::bridge::workspaces::ReadResult::Search(result)) => {
                            self.source = None;
                            self.source_preview = None;
                            self.search = Some(result);
                        }
                        Response::Read(crate::bridge::workspaces::ReadResult::Index(status)) => {
                            self.index = Some(status)
                        }
                        Response::Index(result) => {
                            self.notice = Some(format!("{}\n{}", result.coverage, result.indexing))
                        }
                    }
                }
                Err(error) => self.error = Some(format!("{error}\n{}", fl!("spaces-no-replay"))),
            }
            self.pending_save = false;
            return None;
        }
        if let Message::FileChosen(generation, result) = message {
            if generation == self.generation {
                self.busy = false;
                match result {
                    Ok(Some(path)) => self.tag_path = path,
                    Ok(None) => {}
                    Err(error) => self.error = Some(error),
                }
            }
            return None;
        }
        if matches!(message, Message::Close) {
            self.hide();
            return None;
        }
        if self.busy {
            return None;
        }
        let action = match message {
            Message::Show => {
                self.visible = true;
                Action::List
            }
            Message::Refresh => {
                self.error = None;
                match &self.selected {
                    Some(id) => Action::Get(id.clone()),
                    None => Action::List,
                }
            }
            Message::Select(id) => {
                self.selected = Some(id.clone());
                self.view = None;
                self.dirty = false;
                self.error = None;
                Action::Get(id)
            }
            Message::CreateName(value) => {
                self.create_name = value;
                return None;
            }
            Message::Name(value) => {
                self.draft.name = value;
                self.dirty = true;
                return None;
            }
            Message::Description(value) => {
                self.draft.description = value;
                self.dirty = true;
                return None;
            }
            Message::Context(value) => {
                self.draft.context = value;
                self.dirty = true;
                return None;
            }
            Message::ReviewRevision => {
                self.draft_revision = self.view.as_ref().map(|view| view.workspace.revision);
                return None;
            }
            Message::TagPath(value) => {
                self.tag_path = value;
                return None;
            }
            Message::NoteTitle(value) => {
                self.note_title = value;
                return None;
            }
            Message::NoteContent(value) => {
                self.note_content = value;
                return None;
            }
            Message::Activity(value) => {
                self.activity = value;
                return None;
            }
            Message::Query(value) => {
                self.query = value;
                return None;
            }
            Message::SourcePath(value) => {
                self.source_path = value;
                return None;
            }
            Message::LinkDraft(value) => {
                self.link_draft = value;
                return None;
            }
            Message::InspectOrigin(id) => Action::Origin(id),
            Message::FileChosen(generation, result) => {
                if generation == self.generation {
                    match result {
                        Ok(Some(path)) => self.tag_path = path,
                        Ok(None) => {}
                        Err(error) => self.error = Some(error),
                    }
                }
                return None;
            }
            Message::Create => Action::Control(WorkspaceAction::Create {
                draft: WorkspaceDraft {
                    name: self.create_name.clone(),
                    description: String::new(),
                    context: String::new(),
                },
            }),
            message => {
                let view = self.view.as_ref()?;
                let id = view.workspace.id.clone();
                let expected_revision = view.workspace.revision;
                match message {
                    Message::Save => {
                        let revision = self.draft_revision?;
                        if revision != expected_revision {
                            self.error = Some(fl!("spaces-stale-draft"));
                            return None;
                        }
                        Action::Control(WorkspaceAction::Update {
                            id,
                            expected_revision: revision,
                            draft: self.draft.clone(),
                        })
                    }
                    Message::Tag => Action::Control(WorkspaceAction::Tag {
                        id,
                        expected_revision,
                        path: self.tag_path.clone(),
                        kind: None,
                    }),
                    Message::RemoveTag(tag_id) => Action::Control(WorkspaceAction::Untag {
                        id,
                        expected_revision,
                        tag_id,
                    }),
                    Message::AddNote => Action::Control(WorkspaceAction::PutNote {
                        id,
                        expected_revision,
                        note_id: None,
                        note: NoteDraft {
                            title: self.note_title.clone(),
                            content: self.note_content.clone(),
                        },
                    }),
                    Message::ForgetNote(note_id) => Action::Control(WorkspaceAction::ForgetNote {
                        id,
                        expected_revision,
                        note_id,
                    }),
                    Message::AttachActivity => Action::Control(WorkspaceAction::BindActivity {
                        id,
                        expected_revision,
                        activity_id: self.activity.clone(),
                    }),
                    Message::DetachActivity(activity_id) => {
                        Action::Control(WorkspaceAction::UnbindActivity {
                            id,
                            expected_revision,
                            activity_id,
                        })
                    }
                    Message::Archive(archived) => Action::Control(WorkspaceAction::Archive {
                        id,
                        expected_revision,
                        archived,
                    }),
                    Message::BeginDelete => {
                        self.delete = Some(view.workspace.clone());
                        return None;
                    }
                    Message::CancelDelete => {
                        self.delete = None;
                        return None;
                    }
                    Message::ConfirmDelete => {
                        let selected = self.delete.as_ref()?;
                        Action::Control(WorkspaceAction::Delete {
                            id: selected.id.clone(),
                            expected_revision: selected.revision,
                            confirm: true,
                        })
                    }
                    Message::Search => Action::Read(
                        id,
                        WorkspaceRead::Search {
                            query: self.query.clone(),
                            limit: 10,
                        },
                    ),
                    Message::ReadSource => Action::Read(
                        id,
                        WorkspaceRead::Source {
                            path: self.source_path.clone(),
                            link_id: None,
                            offset: 0,
                            max_chars: 4096,
                            revision: None,
                        },
                    ),
                    Message::ReadHit(path, revision) => Action::Read(
                        id,
                        WorkspaceRead::Source {
                            path,
                            link_id: None,
                            offset: 0,
                            max_chars: 4096,
                            revision: Some(revision),
                        },
                    ),
                    Message::NextSource => {
                        let page = self.source.as_ref()?;
                        let request = WorkspaceRead::Source {
                            path: page.path.clone(),
                            link_id: self.source_preview.as_ref().map(|(id, _)| id.clone()),
                            offset: page.next_offset?,
                            max_chars: 4096,
                            revision: Some(page.revision.clone()),
                        };
                        match &self.source_preview {
                            Some((_, target)) => Action::Preview(id, request, target.clone()),
                            None => Action::Read(id, request),
                        }
                    }
                    Message::Index => Action::Read(id, WorkspaceRead::Index),
                    Message::RegisterIndex(tag_id) => Action::Index(IndexRequest {
                        id,
                        expected_revision,
                        tag_id,
                    }),
                    Message::RevokeLink(link_id) => Action::Control(WorkspaceAction::RevokeLink {
                        id,
                        expected_revision,
                        link_id,
                    }),
                    Message::PreviewNote(link_id, note_id) => {
                        let link = self
                            .view
                            .as_ref()?
                            .workspace
                            .read_links
                            .iter()
                            .find(|link| link.id == link_id && link.note_ids.contains(&note_id))?;
                        Action::Preview(
                            id,
                            WorkspaceRead::Note {
                                note_id,
                                link_id: Some(link_id),
                            },
                            link.target.clone(),
                        )
                    }
                    Message::PreviewSource(link_id, path) => {
                        let link =
                            self.view
                                .as_ref()?
                                .workspace
                                .read_links
                                .iter()
                                .find(|link| {
                                    link.id == link_id
                                        && link.sources.iter().any(|source| source.path == path)
                                })?;
                        Action::Preview(
                            id,
                            WorkspaceRead::Source {
                                path,
                                link_id: Some(link_id),
                                offset: 0,
                                max_chars: 4096,
                                revision: None,
                            },
                            link.target.clone(),
                        )
                    }
                    Message::AllowLink => {
                        #[derive(serde::Deserialize)]
                        #[serde(deny_unknown_fields)]
                        struct Selection {
                            source_workspace_id: String,
                            target: ReadTarget,
                            note_ids: Vec<String>,
                            sources: Vec<SelectedSource>,
                            expires_at: String,
                        }
                        let selected: Selection = match serde_json::from_str(&self.link_draft) {
                            Ok(value) => value,
                            Err(error) => {
                                self.error = Some(error.to_string());
                                return None;
                            }
                        };
                        Action::Control(WorkspaceAction::ReadLink {
                            id,
                            expected_revision,
                            source_workspace_id: selected.source_workspace_id,
                            target: selected.target,
                            note_ids: selected.note_ids,
                            sources: selected.sources,
                            expires_at: selected.expires_at,
                        })
                    }
                    _ => return None,
                }
            }
        };
        if !connected {
            self.error = Some(fl!("bridge-offline"));
            return None;
        }
        if let Action::Control(action) = &action {
            if let Err(error) = action.validate() {
                self.error = Some(error);
                return None;
            }
            self.pending_save = matches!(action, WorkspaceAction::Update { .. });
        }
        match &action {
            Action::Read(_, WorkspaceRead::Source { .. }) => {
                self.source = None;
                self.source_preview = None;
            }
            Action::Preview(
                _,
                WorkspaceRead::Source {
                    link_id: Some(link_id),
                    ..
                },
                target,
            ) => {
                self.source = None;
                self.source_preview = Some((link_id.clone(), target.clone()));
            }
            _ => {}
        }
        self.busy = true;
        self.mutation = matches!(action, Action::Control(_) | Action::Index(_));
        self.error = None;
        self.notice = None;
        self.generation = self.generation.wrapping_add(1);
        Some(Request {
            generation: self.generation,
            action,
        })
    }
    pub(crate) fn pick(&mut self, folder: bool) -> cosmic::app::Task<AppMessage> {
        if self.busy {
            return cosmic::app::Task::none();
        }
        self.busy = true;
        self.generation = self.generation.wrapping_add(1);
        let generation = self.generation;
        cosmic::app::Task::perform(
            async move {
                let dialog =
                    cosmic::dialog::file_chooser::open::Dialog::new().title(fl!("spaces-tag"));
                let response = if folder {
                    dialog.open_folder().await
                } else {
                    dialog.open_file().await
                };
                match response {
                    Ok(response) => response
                        .url()
                        .to_file_path()
                        .map_err(|_| fl!("spaces-path-error"))?
                        .into_os_string()
                        .into_string()
                        .map(Some)
                        .map_err(|_| fl!("spaces-path-error")),
                    Err(cosmic::dialog::file_chooser::Error::Cancelled) => Ok(None),
                    Err(error) => Err(error.to_string()),
                }
            },
            move |result| {
                cosmic::Action::App(AppMessage::Workspaces(Message::FileChosen(
                    generation, result,
                )))
            },
        )
    }
    pub(crate) fn view(&self) -> Element<'_, AppMessage> {
        let control = |label: String, message: Message, enabled: bool| {
            let button = button::text(label);
            if enabled && !self.busy {
                button.on_press(AppMessage::Workspaces(message))
            } else {
                button
            }
        };
        let usable = self.error.is_none();
        let mut content = Column::new()
            .spacing(12)
            .push(
                Row::new()
                    .spacing(8)
                    .push(text(fl!("spaces-title")).size(22.0))
                    .push(control(fl!("spaces-refresh"), Message::Refresh, true))
                    .push(
                        button::text(fl!("spaces-close"))
                            .on_press(AppMessage::Workspaces(Message::Close)),
                    ),
            )
            .push(text(fl!("spaces-description")).size(13.0))
            .push(text(fl!("spaces-read-only")).size(12.0));
        if let Some(error) = &self.error {
            content = content.push(text(error));
        }
        if let Some(notice) = &self.notice {
            content = content.push(text(notice).size(12.0));
        }
        for workspace in &self.records {
            content = content.push(control(
                format!("{} · {:?}", workspace.name, workspace.state),
                Message::Select(workspace.id.clone()),
                true,
            ));
        }
        content = content
            .push(input(
                fl!("spaces-create-name"),
                &self.create_name,
                Message::CreateName,
            ))
            .push(control(
                fl!("spaces-create"),
                Message::Create,
                !self.create_name.trim().is_empty(),
            ));
        if let Some(view) = &self.view {
            let workspace = &view.workspace;
            let active = workspace.state == WorkspaceState::Active;
            let retained = workspace.state != WorkspaceState::Deleted;
            content = content
                .push(
                    text(format!(
                        "{} · {:?} · revision {}",
                        workspace.name, workspace.state, workspace.revision
                    ))
                    .size(18.0),
                )
                .push(text(&workspace.id).size(11.0))
                .push(button::text(fl!("spaces-new-chat")).on_press_maybe(
                    (active && usable && !self.busy).then_some(
                        AppMessage::NewWorkspaceConversation(Some(workspace.id.clone())),
                    ),
                ))
                .push(input(fl!("spaces-name"), &self.draft.name, Message::Name))
                .push(input(
                    fl!("spaces-description-label"),
                    &self.draft.description,
                    Message::Description,
                ))
                .push(input(
                    fl!("spaces-background"),
                    &self.draft.context,
                    Message::Context,
                ))
                .push(text(format!("Draft revision {:?}", self.draft_revision)).size(11.0));
            if self.draft_revision != Some(workspace.revision) {
                content = content.push(control(
                    fl!("spaces-review-revision"),
                    Message::ReviewRevision,
                    usable,
                ));
            }
            content = content
                .push(control(
                    fl!("spaces-save"),
                    Message::Save,
                    retained && usable && self.draft_revision == Some(workspace.revision),
                ))
                .push(
                    Row::new()
                        .spacing(8)
                        .push(control(
                            if active {
                                fl!("spaces-archive")
                            } else {
                                fl!("spaces-restore")
                            },
                            Message::Archive(active),
                            retained && usable,
                        ))
                        .push(control(
                            fl!("spaces-delete"),
                            Message::BeginDelete,
                            retained || workspace.cleanup_pending,
                        )),
                )
                .push(text(fl!("spaces-notes")).size(18.0));
            for note in &view.notes {
                content = content
                    .push(
                        text(format!(
                            "{} · {:?} · {} · revision {}",
                            note.title, note.source, note.id, note.revision
                        ))
                        .size(13.0),
                    )
                    .push(text(&note.content).size(12.0));
                if let Some(job) = &note.source_job_id {
                    content = content.push(text(format!("Agent report Job {job}")).size(11.0));
                }
                content = content.push(control(
                    fl!("spaces-forget-note"),
                    Message::ForgetNote(note.id.clone()),
                    retained && usable,
                ));
            }
            content = content
                .push(input(
                    fl!("spaces-note-title"),
                    &self.note_title,
                    Message::NoteTitle,
                ))
                .push(input(
                    fl!("spaces-note-content"),
                    &self.note_content,
                    Message::NoteContent,
                ))
                .push(control(
                    fl!("spaces-add-note"),
                    Message::AddNote,
                    retained
                        && usable
                        && !self.note_title.trim().is_empty()
                        && !self.note_content.trim().is_empty(),
                ))
                .push(text(fl!("spaces-sources")).size(18.0))
                .push(text(fl!("spaces-index-independent")).size(12.0))
                .push(
                    Row::new()
                        .spacing(8)
                        .push(control(
                            fl!("spaces-pick-file"),
                            Message::PickTag(false),
                            active && usable,
                        ))
                        .push(control(
                            fl!("spaces-pick-folder"),
                            Message::PickTag(true),
                            active && usable,
                        )),
                )
                .push(input(
                    fl!("spaces-source-path"),
                    &self.tag_path,
                    Message::TagPath,
                ))
                .push(control(
                    fl!("spaces-tag"),
                    Message::Tag,
                    active && usable && self.tag_path.starts_with('/'),
                ));
            for tag in &view.tags {
                content = content
                    .push(text(format!("{} · {:?} · {}", tag.path, tag.kind, tag.id)).size(12.0))
                    .push(
                        Row::new()
                            .spacing(8)
                            .push(control(
                                fl!("spaces-untag"),
                                Message::RemoveTag(tag.id.clone()),
                                retained && usable,
                            ))
                            .push(control(
                                fl!("spaces-register-index"),
                                Message::RegisterIndex(tag.id.clone()),
                                active && usable,
                            )),
                    );
            }
            content = content
                .push(control(
                    fl!("spaces-index"),
                    Message::Index,
                    active && usable,
                ))
                .push(input(
                    fl!("spaces-search-query"),
                    &self.query,
                    Message::Query,
                ))
                .push(control(
                    fl!("spaces-search"),
                    Message::Search,
                    active && usable && !self.query.trim().is_empty(),
                ))
                .push(input(
                    fl!("spaces-exact-source"),
                    &self.source_path,
                    Message::SourcePath,
                ))
                .push(control(
                    fl!("spaces-read-source"),
                    Message::ReadSource,
                    active && usable && self.source_path.starts_with('/'),
                ));
            if let Some(index) = &self.index {
                content = content
                    .push(
                        text(format!(
                            "{} · configured={} · {} sources / {} chunks · {}",
                            index.embedding.model,
                            index.embedding.configured,
                            index.corpus.indexed_sources,
                            index.corpus.indexed_chunks,
                            index.corpus.state
                        ))
                        .size(12.0),
                    )
                    .push(text(index.embedding.detail.as_deref().unwrap_or("")).size(12.0))
                    .push(text(&index.store).size(11.0))
                    .push(text(&index.formats).size(11.0));
            }
            if let Some(result) = &self.search {
                content = content.push(text(&result.coverage).size(12.0));
                for hit in &result.hits {
                    content = content
                        .push(
                            text(format!(
                                "{} · {:.3} · indexed {}",
                                hit.path, hit.score, hit.indexed_at_ms
                            ))
                            .size(12.0),
                        )
                        .push(text(&hit.snippet).size(12.0))
                        .push(control(
                            fl!("spaces-read-source"),
                            Message::ReadHit(hit.path.clone(), hit.revision.clone()),
                            active && usable,
                        ));
                }
            }
            if let Some(page) = &self.source {
                content = content
                    .push(
                        text(format!(
                            "{} · revision {} · {}/{} characters",
                            page.path, page.revision, page.offset, page.total_chars
                        ))
                        .size(11.0),
                    )
                    .push(text(&page.content).size(12.0))
                    .push(control(
                        fl!("spaces-next-page"),
                        Message::NextSource,
                        active && usable && page.next_offset.is_some(),
                    ));
            }
            content = content
                .push(text(fl!("activities")).size(18.0))
                .push(input(
                    fl!("spaces-activity-id"),
                    &self.activity,
                    Message::Activity,
                ))
                .push(control(
                    fl!("spaces-attach-activity"),
                    Message::AttachActivity,
                    active && usable && !self.activity.is_empty(),
                ));
            for id in &view.activity_ids {
                content = content.push(text(id).size(12.0)).push(control(
                    fl!("spaces-detach-activity"),
                    Message::DetachActivity(id.clone()),
                    usable,
                ));
            }
            content = content
                .push(text(fl!("spaces-temporary-title")).size(18.0))
                .push(text(fl!("spaces-temporary-description")).size(12.0));
            for source in self.records.iter().filter(|source| {
                source.id != workspace.id && source.state == WorkspaceState::Active
            }) {
                content = content.push(control(
                    format!("{} · {}", fl!("spaces-inspect-selection"), source.name),
                    Message::InspectOrigin(source.id.clone()),
                    active && usable,
                ));
            }
            if let Some(source) = &self.origin {
                content = content
                    .push(text(format!("Source Workspace {}", source.workspace.id)).size(12.0));
                for note in &source.notes {
                    content = content.push(
                        text(format!(
                            "Note {} · {:?} · {}",
                            note.id, note.source, note.title
                        ))
                        .size(12.0),
                    );
                }
                for tag in &source.tags {
                    content = content.push(
                        text(format!("Tag {} · {:?} · {}", tag.id, tag.kind, tag.path)).size(12.0),
                    );
                }
            }
            content = content
                .push(input(
                    fl!("spaces-temporary-json"),
                    &self.link_draft,
                    Message::LinkDraft,
                ))
                .push(control(
                    fl!("spaces-allow-selection"),
                    Message::AllowLink,
                    active && usable,
                ));
            for link in &workspace.read_links {
                content = content
                    .push(
                        text(format!(
                            "{} · from {} · {:?} · expires {}",
                            link.id, link.source_workspace_id, link.target, link.expires_at
                        ))
                        .size(12.0),
                    )
                    .push(control(
                        fl!("spaces-revoke-link"),
                        Message::RevokeLink(link.id.clone()),
                        active && usable,
                    ));
                for note_id in &link.note_ids {
                    content = content.push(control(
                        format!("{} · {}", fl!("spaces-preview-note"), note_id),
                        Message::PreviewNote(link.id.clone(), note_id.clone()),
                        active && usable,
                    ));
                }
                for source in &link.sources {
                    content = content.push(control(
                        format!("{} · {}", fl!("spaces-preview-source"), source.path),
                        Message::PreviewSource(link.id.clone(), source.path.clone()),
                        active && usable,
                    ));
                }
            }
            if let Some(preview) = &self.note_preview {
                content = content
                    .push(
                        text(format!(
                            "{} · {} · {:?} · revision {}",
                            fl!("spaces-imported-note"),
                            preview.source_workspace_id,
                            preview.note.source,
                            preview.note.revision
                        ))
                        .size(12.0),
                    )
                    .push(text(preview.note.title.clone()))
                    .push(text(preview.note.content.clone()))
                    .push(text(preview.evidence.clone()).size(12.0));
            }
        }
        if let Some(workspace) = &self.delete {
            content = content
                .push(
                    text(format!(
                        "{}: {} · revision {}",
                        fl!("spaces-delete-confirm"),
                        workspace.name,
                        workspace.revision
                    ))
                    .size(16.0),
                )
                .push(text(fl!("spaces-delete-evidence")).size(12.0))
                .push(
                    Row::new()
                        .spacing(8)
                        .push(control(
                            fl!("spaces-confirm-delete"),
                            Message::ConfirmDelete,
                            true,
                        ))
                        .push(control(fl!("spaces-keep"), Message::CancelDelete, true)),
                );
        }
        scrollable(content.padding(20)).into()
    }
}
