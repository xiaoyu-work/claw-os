//! Human Workspace center. Owned requests never borrow live renderer state.

use super::{
    backend::Backend,
    requests::Request,
    state::{App, Confirmation, ConfirmationAction},
    InputAction,
};
use clawd_client::{workspaces::*, Command};
use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::{
    layout::Rect,
    text::Line,
    widgets::{Block, Borders, Clear, Paragraph, Wrap},
    Frame,
};
use serde_json::{json, Value};
use std::sync::Arc;

#[derive(Default)]
pub(super) struct Center {
    pub open: bool,
    pub generation: u64,
    pub selected: Option<String>,
    pub records: Vec<Workspace>,
    pub view: Option<WorkspaceView>,
    pub output: Option<String>,
    pub error: Option<String>,
    pub scroll: u16,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Action {
    Refresh,
    Select(String),
    Conversation(Option<String>),
    Control(WorkspaceAction),
    Read(WorkspaceRead),
    Index(String),
}

pub(super) fn command(app: &mut App, rest: &str) -> Result<Action, String> {
    let (verb, tail) = rest.split_once(char::is_whitespace).unwrap_or((rest, ""));
    let tail = tail.trim();
    if matches!(verb, "" | "list") {
        return Ok(Action::Refresh);
    }
    if verb == "select" {
        validate_id(tail)?;
        return Ok(Action::Select(tail.into()));
    }
    if verb == "create" {
        let action = WorkspaceAction::Create {
            draft: WorkspaceDraft {
                name: tail.into(),
                description: String::new(),
                context: String::new(),
            },
        };
        action.validate()?;
        return Ok(Action::Control(action));
    }
    if verb == "global" {
        return Ok(Action::Conversation(None));
    }
    let view = app
        .spaces
        .view
        .as_ref()
        .ok_or("Open /workspace and select a Workspace first")?;
    let id = view.workspace.id.clone();
    let expected_revision = view.workspace.revision;
    let action=match verb {
        "chat" => return Ok(Action::Conversation(Some(id))),
        "context"|"rename"|"description" => {
            let mut draft=WorkspaceDraft{name:view.workspace.name.clone(),description:view.workspace.description.clone(),context:view.workspace.context.clone()};
            match verb {"rename"=>draft.name=tail.into(),"description"=>draft.description=tail.into(),_=>draft.context=tail.into()}
            WorkspaceAction::Update{id,expected_revision,draft}
        }
        "tag"=>WorkspaceAction::Tag{id,expected_revision,path:tail.into(),kind:None},
        "untag"=>WorkspaceAction::Untag{id,expected_revision,tag_id:tail.into()},
        "note"=>{
            let (title,content)=tail.split_once('|').ok_or("Use /workspace note TITLE | CONTENT")?;
            WorkspaceAction::PutNote{id,expected_revision,note_id:None,note:NoteDraft{title:title.trim().into(),content:content.trim().into()}}
        }
        "forget-note"=>WorkspaceAction::ForgetNote{id,expected_revision,note_id:tail.into()},
        "attach"=>WorkspaceAction::BindActivity{id,expected_revision,activity_id:tail.into()},
        "detach"=>WorkspaceAction::UnbindActivity{id,expected_revision,activity_id:tail.into()},
        "archive"|"restore"=>WorkspaceAction::Archive{id,expected_revision,archived:verb=="archive"},
        "delete"=>{
            app.confirmation=Some(Confirmation{
                title:"Delete this Workspace?".into(),
                body:format!("{} (revision {expected_revision}). Shared notes/tags are removed; automatic work stops, Activities pause and exact Jobs are cancelled. Original files, conversations, receipts and audit remain. Pending cleanup is not retirement proof.",view.workspace.name),
                confirm_label:"Delete Workspace".into(),
                action:ConfirmationAction::WorkspaceDelete{id,revision:expected_revision},
            });
            return Ok(Action::Refresh);
        }
        "search"=>return Ok(Action::Read(WorkspaceRead::Search{query:tail.into(),limit:10})),
        "read"=>return Ok(Action::Read(WorkspaceRead::Source{path:tail.into(),link_id:None,offset:0,max_chars:4096,revision:None})),
        "read-note"=>return Ok(Action::Read(WorkspaceRead::Note{note_id:tail.into(),link_id:None})),
        "index" if tail.is_empty()=>return Ok(Action::Read(WorkspaceRead::Index)),
        "index"=>{validate_id(tail)?;return Ok(Action::Index(tail.into()));}
        "revoke"=>WorkspaceAction::RevokeLink{id,expected_revision,link_id:tail.into()},
        "read-link"=>{
            #[derive(serde::Deserialize)]
            #[serde(deny_unknown_fields)]
            struct Selection {source_workspace_id:String,target:ReadTarget,note_ids:Vec<String>,sources:Vec<SelectedSource>,expires_at:String}
            let selection:Selection=serde_json::from_str(tail).map_err(|e|format!("temporary selection JSON: {e}"))?;
            WorkspaceAction::ReadLink{id,expected_revision,source_workspace_id:selection.source_workspace_id,
                target:selection.target,note_ids:selection.note_ids,sources:selection.sources,expires_at:selection.expires_at}
        }
        _=>return Err("Use /workspace [create NAME|select ID|chat|global|context TEXT|tag PATH|untag ID|note TITLE | CONTENT|forget-note ID|attach ACTIVITY|detach ACTIVITY|search QUERY|read PATH|read-note ID|index [TAG]|read-link JSON|revoke ID|archive|restore|delete]".into()),
    };
    action.validate()?;
    Ok(Action::Control(action))
}

pub(super) fn request(
    app: &mut App,
    backend: Arc<dyn Backend>,
    action: Action,
) -> Result<Request, String> {
    if let Action::Conversation(space) = action {
        if app.active_task.is_some() || app.status != super::state::RunStatus::Ready {
            return Err("finish or detach the current task before switching conversations".into());
        }
        return Ok(Request::new(
            "Creating Workspace conversation",
            async move {
                backend
                    .create_conversation_in_workspace(space.as_deref())
                    .await
            },
            |app, conversation| {
                app.spaces.open = false;
                app.replace_conversation(conversation);
                Ok(())
            },
        )
        .mutation());
    }
    let view = app.spaces.view.clone();
    let selected = match &action {
        Action::Select(id) => Some(id.clone()),
        _ => app.spaces.selected.clone(),
    };
    app.spaces.selected = selected.clone();
    app.spaces.open = true;
    app.spaces.generation = app.spaces.generation.wrapping_add(1);
    let generation = app.spaces.generation;
    let mutation = matches!(action, Action::Control(_) | Action::Index(_));
    let request = Request::new(
        "Loading Workspaces",
        async move {
            match action {
                Action::Refresh => {
                    let value = backend
                        .workspace_request(Command::WorkspaceList, json!({}))
                        .await?;
                    let list: WorkspaceList =
                        serde_json::from_value(value).map_err(|e| e.to_string())?;
                    if list.schema != SCHEMA_VERSION || list.workspaces.len() > MAX_WORKSPACES {
                        return Err("invalid Workspace list".into());
                    }
                    for workspace in &list.workspaces {
                        workspace.validate()?;
                    }
                    let selected = selected.or_else(|| {
                        list.workspaces
                            .first()
                            .map(|workspace| workspace.id.clone())
                    });
                    let view = match selected.as_deref() {
                        Some(id) => Some(decode_view(
                            backend
                                .workspace_request(Command::WorkspaceGet, json!({"id":id}))
                                .await?,
                        )?),
                        None => None,
                    };
                    Ok((Some(list.workspaces), view, None))
                }
                Action::Select(id) => Ok((
                    None,
                    Some(decode_view(
                        backend
                            .workspace_request(Command::WorkspaceGet, json!({"id":id}))
                            .await?,
                    )?),
                    None,
                )),
                Action::Control(action) => {
                    let id = action.id().map(str::to_string);
                    let returned = decode_view(
                        backend
                            .workspace_request(Command::WorkspaceControl, json!({"request":action}))
                            .await?,
                    )?;
                    if id.as_ref().is_some_and(|id| *id != returned.workspace.id) {
                        return Err("Workspace acknowledgement identity mismatch".into());
                    }
                    Ok((None, Some(returned), None))
                }
                Action::Read(request) => {
                    request.validate()?;
                    let id = selected.ok_or("select a Workspace")?;
                    let value = backend
                        .workspace_request(
                            Command::WorkspaceRead,
                            json!({"id":id,"request":request}),
                        )
                        .await?;
                    let output = serde_json::to_string_pretty(&value).map_err(|e| e.to_string())?;
                    if output.len() > 128 * 1024 {
                        return Err("Workspace read exceeds its presentation budget".into());
                    }
                    Ok((None, None, Some(output)))
                }
                Action::Index(tag_id) => {
                    let view = view.ok_or("select a Workspace")?;
                    let result=backend.workspace_request(Command::WorkspaceIndexSource,json!({
                    "id":view.workspace.id,"expected_revision":view.workspace.revision,"tag_id":tag_id,
                })).await?;
                    Ok((
                        None,
                        None,
                        Some(serde_json::to_string_pretty(&result).map_err(|e| e.to_string())?),
                    ))
                }
                Action::Conversation(_) => Err("unsupported center action".into()),
            }
        },
        move |app, (records, view, output)| {
            if generation != app.spaces.generation {
                if mutation {
                    app.push_system(
                        "Workspace operation was acknowledged; refresh /workspace to inspect it.",
                    );
                }
                return Ok(());
            }
            if let Some(records) = records {
                app.spaces.records = records;
            }
            if let Some(view) = view {
                app.spaces.selected = Some(view.workspace.id.clone());
                if let Some(record) = app
                    .spaces
                    .records
                    .iter_mut()
                    .find(|record| record.id == view.workspace.id)
                {
                    *record = view.workspace.clone();
                } else if view.workspace.state != WorkspaceState::Deleted {
                    app.spaces.records.push(view.workspace.clone());
                }
                app.spaces.view = Some(view);
            }
            app.spaces.output = output;
            app.spaces.error = None;
            app.spaces.scroll = 0;
            Ok(())
        },
    );
    Ok(if mutation {
        request.mutation()
    } else {
        request
    })
}

fn decode_view(value: Value) -> Result<WorkspaceView, String> {
    let view: WorkspaceView = serde_json::from_value(value).map_err(|e| e.to_string())?;
    view.validate()?;
    Ok(view)
}

pub(super) fn key(app: &mut App, event: KeyEvent) -> Option<InputAction> {
    if !app.spaces.open
        || app.current_approval().is_some()
        || app.approval_detail.is_some()
        || app.confirmation.is_some()
    {
        return None;
    }
    if event.kind != KeyEventKind::Press
        || !event.modifiers.difference(KeyModifiers::SHIFT).is_empty()
    {
        return Some(InputAction::None);
    }
    let action = match event.code {
        KeyCode::Esc => {
            app.spaces.open = false;
            app.spaces.generation = app.spaces.generation.wrapping_add(1);
            InputAction::None
        }
        KeyCode::Up | KeyCode::PageUp => {
            app.spaces.scroll = app.spaces.scroll.saturating_sub(4);
            InputAction::None
        }
        KeyCode::Down | KeyCode::PageDown => {
            app.spaces.scroll = app.spaces.scroll.saturating_add(4);
            InputAction::None
        }
        KeyCode::Left | KeyCode::Right => {
            let len = app.spaces.records.len();
            if len == 0 {
                InputAction::None
            } else {
                let current = app
                    .spaces
                    .records
                    .iter()
                    .position(|record| Some(&record.id) == app.spaces.selected.as_ref())
                    .unwrap_or(0);
                let next = if event.code == KeyCode::Left {
                    (current + len - 1) % len
                } else {
                    (current + 1) % len
                };
                InputAction::Spaces(Action::Select(app.spaces.records[next].id.clone()))
            }
        }
        KeyCode::Char('r') => InputAction::Spaces(Action::Refresh),
        KeyCode::Enter => app.spaces.view.as_ref().map_or(InputAction::None, |view| {
            InputAction::Spaces(Action::Conversation(Some(view.workspace.id.clone())))
        }),
        KeyCode::Char('i') => InputAction::Spaces(Action::Read(WorkspaceRead::Index)),
        KeyCode::Char(ch) if matches!(ch, 'c' | 'd' | 't' | 'n' | 'b' | 's' | 'u' | 'x' | 'l') => {
            app.spaces.open = false;
            let form=match ch {'c'=>"/workspace create ",'d'=>"/workspace delete",'t'=>"/workspace tag /absolute/file-or-folder",
                'n'=>"/workspace note Title | Shared note",'b'=>"/workspace context Shared background",'s'=>"/workspace search query",
                'u'=>"/workspace untag TAG_UUID",'x'=>"/workspace attach ACTIVITY_UUID",
                _=>"/workspace read-link {\"source_workspace_id\":\"UUID\",\"target\":{\"kind\":\"conversation\",\"id\":\"ses_...\"},\"note_ids\":[\"UUID\"],\"sources\":[],\"expires_at\":\"RFC3339\"}"};
            app.prefill_input(form.into());
            InputAction::None
        }
        _ => InputAction::None,
    };
    Some(action)
}

pub(super) fn render(frame: &mut Frame<'_>, screen: Rect, app: &App) {
    if !app.spaces.open
        || app.current_approval().is_some()
        || app.approval_detail.is_some()
        || app.confirmation.is_some()
    {
        return;
    }
    let width = screen.width.saturating_sub(4).min(110);
    let height = screen.height.saturating_sub(4);
    if width < 40 || height < 8 {
        return;
    }
    let area = Rect::new(
        screen.x + (screen.width - width) / 2,
        screen.y + (screen.height - height) / 2,
        width,
        height,
    );
    let mut lines=vec![Line::raw("Left/Right selects. Enter starts a NEW conversation in that Workspace; existing histories never move."),
        Line::raw("C create · D delete · T tag · U untag · N note · B background · S search · I index readiness · X attach Activity · L temporary read"),
        Line::raw("Use /workspace index TAG_UUID to explicitly add its directory to the independent personal-file index. Tags still constrain retrieval.")];
    for record in &app.spaces.records {
        lines.push(Line::raw(format!(
            "{} {} · {:?} · {}",
            if Some(&record.id) == app.spaces.selected.as_ref() {
                ">"
            } else {
                " "
            },
            super::state::clean_text(&record.name),
            record.state,
            record.id
        )));
    }
    if let Some(view) = &app.spaces.view {
        lines.push(Line::raw(format!(
            "Workspace {} · revision {} · cleanup pending {}",
            view.workspace.id, view.workspace.revision, view.workspace.cleanup_pending
        )));
        lines.push(Line::raw(super::state::clean_text(&view.workspace.context)));
        for note in &view.notes {
            lines.push(Line::raw(format!(
                "Note {} · {:?} · {} · {}",
                note.id,
                note.source,
                super::state::clean_text(&note.title),
                super::state::clean_text(&note.content.chars().take(256).collect::<String>())
            )));
        }
        for tag in &view.tags {
            lines.push(Line::raw(format!(
                "Tag {} · {:?} · {}",
                tag.id,
                tag.kind,
                super::state::clean_text(&tag.path)
            )));
        }
        for id in &view.activity_ids {
            lines.push(Line::raw(format!(
                "Activity {id} · /activity {id} · /workspace detach {id} (paused only)"
            )));
        }
        for link in &view.workspace.read_links {
            lines.push(Line::raw(format!(
                "Temporary read {} · from {} · {:?} · expires {} · /workspace revoke {}",
                link.id, link.source_workspace_id, link.target, link.expires_at, link.id
            )));
        }
        if view.workspace.cleanup_pending {
            lines.push(Line::raw("Cleanup is pending, not checked retirement. Refresh and explicitly repeat deletion to inspect completion."));
        }
    }
    if let Some(output) = &app.spaces.output {
        lines.extend(
            super::state::clean_text(output)
                .lines()
                .map(|line| Line::raw(line.to_string())),
        );
    }
    if let Some(error) = &app.spaces.error {
        lines.push(Line::raw(format!("Refresh required: {error}")));
    }
    frame.render_widget(Clear, area);
    frame.render_widget(
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .scroll((app.spaces.scroll, 0))
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(" Workspaces · R refresh · Esc close (work continues) "),
            ),
        area,
    );
}
