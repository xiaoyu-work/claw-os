use std::collections::{HashMap, HashSet, VecDeque};
use std::time::{Duration, Instant};

use super::backend::{
    ApprovalRequest, BackendInfo, Conversation, ConversationSummary, Job, TaskSummary,
};

const MAX_TRANSCRIPT_ENTRIES: usize = 2_048;
const MAX_TRANSCRIPT_BYTES: usize = 2 * 1024 * 1024;
pub(super) const MAX_PROMPT_BYTES: usize = 64 * 1024;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum EntryKind {
    User,
    Assistant,
    Reasoning,
    Tool { id: String, status: ToolStatus },
    System,
    Error,
    Approval { id: String, status: ApprovalStatus },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ToolStatus {
    Running,
    Succeeded { duration_ms: Option<u64> },
    Failed { duration_ms: Option<u64> },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ApprovalStatus {
    Pending,
    Approved,
    Denied,
    Closed,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct Entry {
    pub kind: EntryKind,
    pub text: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum RunStatus {
    Ready,
    Working,
    WaitingApproval,
    Cancelling,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum PickerKind {
    Models,
    Sessions,
    Tasks,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct PickerItem {
    pub label: String,
    pub detail: String,
    pub value: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct Picker {
    pub kind: PickerKind,
    pub title: &'static str,
    pub items: Vec<PickerItem>,
    pub query: String,
    pub selected: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum PickerSelection {
    Model(String),
    Session(String),
    Task(String),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum ConfirmationAction {
    Archive,
    Rewind(u32),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct Confirmation {
    pub title: String,
    pub body: String,
    pub confirm_label: String,
    pub action: ConfirmationAction,
}

pub(super) struct App {
    pub info: BackendInfo,
    pub conversation: Conversation,
    pub entries: Vec<Entry>,
    pub input: String,
    pub cursor: usize,
    pub status: RunStatus,
    pub active_task: Option<String>,
    pub selected_model: String,
    pub queued_prompts: VecDeque<String>,
    pub pending_approvals: VecDeque<ApprovalRequest>,
    pub input_history: Vec<String>,
    pub history_index: Option<usize>,
    pub command_selection: usize,
    pub picker: Option<Picker>,
    pub confirmation: Option<Confirmation>,
    pub task_detail: Option<Job>,
    pub task_detail_scroll: u16,
    pub scroll: u16,
    pub usage_input: u64,
    pub usage_output: u64,
    pub usage_cached: u64,
    pub should_quit: bool,
    pub frame: u64,
    task_started_at: Option<Instant>,
    active_assistant: Option<usize>,
    provider_had_text: bool,
    tool_entries: HashMap<String, usize>,
    seen_approvals: HashSet<String>,
    transcript_bytes: usize,
}

impl App {
    pub fn new(info: BackendInfo, mut conversation: Conversation) -> Self {
        conversation.title = clean_text(&conversation.title).replace('\n', " ");
        let selected_model = info.model.clone();
        let mut app = Self {
            info,
            conversation,
            entries: Vec::new(),
            input: String::new(),
            cursor: 0,
            status: RunStatus::Ready,
            active_task: None,
            selected_model,
            queued_prompts: VecDeque::new(),
            pending_approvals: VecDeque::new(),
            input_history: Vec::new(),
            history_index: None,
            command_selection: 0,
            picker: None,
            confirmation: None,
            task_detail: None,
            task_detail_scroll: 0,
            scroll: 0,
            usage_input: 0,
            usage_output: 0,
            usage_cached: 0,
            should_quit: false,
            frame: 0,
            task_started_at: None,
            active_assistant: None,
            provider_had_text: false,
            tool_entries: HashMap::new(),
            seen_approvals: HashSet::new(),
            transcript_bytes: 0,
        };
        app.load_history();
        app
    }

    pub fn replace_conversation(&mut self, mut conversation: Conversation) {
        conversation.title = clean_text(&conversation.title).replace('\n', " ");
        self.conversation = conversation;
        self.entries.clear();
        self.transcript_bytes = 0;
        self.tool_entries.clear();
        self.pending_approvals.clear();
        self.seen_approvals.clear();
        self.active_assistant = None;
        self.provider_had_text = false;
        self.scroll = 0;
        self.command_selection = 0;
        self.picker = None;
        self.confirmation = None;
        self.task_detail = None;
        self.task_detail_scroll = 0;
        self.status = RunStatus::Ready;
        self.active_task = None;
        self.task_started_at = None;
        self.queued_prompts.clear();
        self.usage_input = 0;
        self.usage_output = 0;
        self.usage_cached = 0;
        self.load_history();
    }

    fn load_history(&mut self) {
        if self.conversation.history_truncated {
            self.push_system(
                "Only the most recent bounded conversation history is shown in this terminal.",
            );
        }
        if self.conversation.messages.is_empty() {
            self.push_system("New Claw conversation. Type /help for terminal commands.");
            return;
        }
        for message in self.conversation.messages.clone() {
            match message.role.as_str() {
                "user" => self.push_entry(EntryKind::User, clean_text(&message.text)),
                "assistant" => self.push_entry(EntryKind::Assistant, clean_text(&message.text)),
                _ => {}
            }
        }
    }

    pub fn begin_task(&mut self, job: &Job) {
        self.active_task = Some(job.id.clone());
        self.status = RunStatus::Working;
        self.task_started_at = Some(Instant::now());
        self.active_assistant = None;
        self.provider_had_text = false;
        self.push_entry(EntryKind::User, clean_text(&job.prompt));
        self.scroll = 0;
    }

    pub fn finish_task(&mut self, job: &Job) {
        if self.active_task.as_deref() != Some(job.id.as_str()) {
            self.push_error("Claw returned a terminal result for another task.");
            return;
        }
        if self.active_assistant.is_none() {
            if let Some(response) = job
                .response
                .as_deref()
                .filter(|response| !response.is_empty())
            {
                self.push_entry(EntryKind::Assistant, clean_text(response));
            }
        }
        match job.status.as_str() {
            "ok" => {}
            "cancelled" => self.push_system("Task cancelled."),
            "error" => self.push_error(job.error.as_deref().unwrap_or("Claw task failed.")),
            status => self.push_error(&format!("Task ended with unexpected status {status}.")),
        }
        self.active_task = None;
        self.status = RunStatus::Ready;
        self.task_started_at = None;
        self.active_assistant = None;
        self.provider_had_text = false;
        for entry in &mut self.entries {
            if let EntryKind::Approval { status, .. } = &mut entry.kind {
                if *status == ApprovalStatus::Pending {
                    *status = ApprovalStatus::Closed;
                }
            }
        }
        self.pending_approvals.clear();
        if self
            .task_detail
            .as_ref()
            .is_some_and(|task| task.id == job.id)
        {
            self.open_task_detail(job.clone());
        }
    }

    pub fn push_assistant_delta(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        self.provider_had_text = true;
        let text = clean_text(text);
        if let Some(index) = self.active_assistant {
            self.entries[index].text.push_str(&text);
            self.transcript_bytes = self.transcript_bytes.saturating_add(text.len());
        } else {
            self.push_entry(EntryKind::Assistant, text);
            self.active_assistant = self.entries.len().checked_sub(1);
        }
        self.trim_transcript();
        self.scroll = 0;
    }

    pub fn provider_had_text(&self) -> bool {
        self.provider_had_text
    }

    pub fn finish_provider(&mut self) {
        self.active_assistant = None;
        self.provider_had_text = false;
    }

    pub fn push_reasoning(&mut self, summary: &[String]) {
        if summary.is_empty() {
            return;
        }
        self.push_entry(
            EntryKind::Reasoning,
            summary
                .iter()
                .map(|line| clean_text(line))
                .collect::<Vec<_>>()
                .join("\n"),
        );
    }

    pub fn tool_started(&mut self, id: &str, name: &str) {
        if let Some(index) = self.tool_entries.get(id).copied() {
            self.entries[index].text = clean_text(name);
            return;
        }
        self.push_entry(
            EntryKind::Tool {
                id: id.to_string(),
                status: ToolStatus::Running,
            },
            clean_text(name),
        );
        if let Some(index) = self.entries.len().checked_sub(1) {
            self.tool_entries.insert(id.to_string(), index);
        }
    }

    pub fn tool_finished(&mut self, id: &str, name: &str, success: bool, duration_ms: Option<u64>) {
        self.tool_started(id, name);
        if let Some(index) = self.tool_entries.get(id).copied() {
            if let EntryKind::Tool { status, .. } = &mut self.entries[index].kind {
                *status = if success {
                    ToolStatus::Succeeded { duration_ms }
                } else {
                    ToolStatus::Failed { duration_ms }
                };
            }
        }
    }

    pub fn add_approvals(&mut self, approvals: Vec<ApprovalRequest>) {
        for approval in approvals {
            if !self.seen_approvals.insert(approval.id.clone()) {
                continue;
            }
            self.push_entry(
                EntryKind::Approval {
                    id: approval.id.clone(),
                    status: ApprovalStatus::Pending,
                },
                format!(
                    "{} on {}\n{}",
                    clean_text(&approval.verb),
                    clean_text(&approval.scope.to_string()),
                    clean_text(&approval.reason)
                ),
            );
            self.pending_approvals.push_back(approval);
        }
        if !self.pending_approvals.is_empty() {
            self.status = RunStatus::WaitingApproval;
            self.picker = None;
        }
    }

    pub fn current_approval(&self) -> Option<&ApprovalRequest> {
        self.pending_approvals.front()
    }

    pub fn resolve_approval(&mut self, id: &str, approved: bool) {
        self.pending_approvals.retain(|approval| approval.id != id);
        for entry in &mut self.entries {
            if let EntryKind::Approval {
                id: entry_id,
                status,
            } = &mut entry.kind
            {
                if entry_id == id {
                    *status = if approved {
                        ApprovalStatus::Approved
                    } else {
                        ApprovalStatus::Denied
                    };
                }
            }
        }
        self.status = if !self.pending_approvals.is_empty() {
            RunStatus::WaitingApproval
        } else if self.active_task.is_some() {
            RunStatus::Working
        } else {
            RunStatus::Ready
        };
    }

    pub fn push_system(&mut self, text: &str) {
        self.push_entry(EntryKind::System, clean_text(text));
    }

    pub fn push_error(&mut self, text: &str) {
        self.push_entry(EntryKind::Error, clean_text(text));
    }

    pub fn queue_prompt(&mut self, prompt: String) {
        self.push_system(&format!(
            "Queued for the current task: {}",
            clean_text(&prompt)
        ));
        self.queued_prompts.push_back(prompt);
    }

    pub fn take_input(&mut self) -> String {
        let input = self.input.trim().to_string();
        self.input.clear();
        self.cursor = 0;
        self.history_index = None;
        self.command_selection = 0;
        if !input.is_empty() {
            self.input_history.push(input.clone());
            if self.input_history.len() > 100 {
                self.input_history.remove(0);
            }
        }
        input
    }

    pub fn insert_char(&mut self, value: char) {
        if self.input.len().saturating_add(value.len_utf8()) > MAX_PROMPT_BYTES {
            return;
        }
        let index = byte_index(&self.input, self.cursor);
        self.input.insert(index, value);
        self.cursor += 1;
        self.command_selection = 0;
    }

    pub fn insert_text(&mut self, value: &str) {
        for value in value
            .chars()
            .filter(|value| !value.is_control() || matches!(*value, '\n' | '\t'))
        {
            self.insert_char(value);
        }
    }

    pub fn backspace(&mut self) {
        if self.cursor == 0 {
            return;
        }
        let start = byte_index(&self.input, self.cursor - 1);
        let end = byte_index(&self.input, self.cursor);
        self.input.replace_range(start..end, "");
        self.cursor -= 1;
        self.command_selection = 0;
    }

    pub fn delete(&mut self) {
        if self.cursor >= self.input.chars().count() {
            return;
        }
        let start = byte_index(&self.input, self.cursor);
        let end = byte_index(&self.input, self.cursor + 1);
        self.input.replace_range(start..end, "");
        self.command_selection = 0;
    }

    pub fn move_left(&mut self) {
        self.cursor = self.cursor.saturating_sub(1);
    }

    pub fn move_right(&mut self) {
        self.cursor = (self.cursor + 1).min(self.input.chars().count());
    }

    pub fn move_up(&mut self) {
        if move_vertical(&self.input, &mut self.cursor, true) {
            return;
        }
        self.history_previous();
    }

    pub fn move_down(&mut self) {
        if move_vertical(&self.input, &mut self.cursor, false) {
            return;
        }
        self.history_next();
    }

    fn history_previous(&mut self) {
        if self.input_history.is_empty() {
            return;
        }
        let index = self
            .history_index
            .map_or(self.input_history.len() - 1, |index| {
                index.saturating_sub(1)
            });
        self.history_index = Some(index);
        self.input = self.input_history[index].clone();
        self.cursor = self.input.chars().count();
    }

    fn history_next(&mut self) {
        let Some(index) = self.history_index else {
            return;
        };
        if index + 1 >= self.input_history.len() {
            self.history_index = None;
            self.input.clear();
        } else {
            self.history_index = Some(index + 1);
            self.input = self.input_history[index + 1].clone();
        }
        self.cursor = self.input.chars().count();
    }

    pub fn clear_transcript(&mut self) {
        self.entries.clear();
        self.transcript_bytes = 0;
        self.tool_entries.clear();
        self.push_system(
            "Transcript view cleared. Canonical conversation history was not deleted.",
        );
    }

    pub fn complete_command(&mut self) {
        let Some(completion) = super::commands::completion(&self.input, self.command_selection)
        else {
            return;
        };
        self.input = completion;
        self.cursor = self.input.chars().count();
    }

    pub fn command_palette_active(&self) -> bool {
        self.picker.is_none() && !super::commands::suggestions(&self.input).is_empty()
    }

    pub fn move_command_selection(&mut self, down: bool) {
        let count = super::commands::suggestions(&self.input).len();
        if count == 0 {
            self.command_selection = 0;
        } else if down {
            self.command_selection = (self.command_selection + 1) % count;
        } else {
            self.command_selection = self.command_selection.checked_sub(1).unwrap_or(count - 1);
        }
    }

    pub fn open_model_picker(&mut self) {
        let selected = self
            .info
            .models
            .iter()
            .position(|model| model == &self.selected_model)
            .unwrap_or(0);
        self.picker = Some(Picker {
            kind: PickerKind::Models,
            title: "Select model",
            items: self
                .info
                .models
                .iter()
                .map(|model| PickerItem {
                    label: model.clone(),
                    detail: if model == &self.selected_model {
                        "current".into()
                    } else {
                        self.info.provider.clone()
                    },
                    value: model.clone(),
                })
                .collect(),
            query: String::new(),
            selected,
        });
    }

    pub fn open_session_picker(&mut self, conversations: Vec<ConversationSummary>) {
        let current_id = self.conversation.id.clone();
        self.picker = Some(Picker {
            kind: PickerKind::Sessions,
            title: "Resume conversation",
            items: conversations
                .into_iter()
                .map(|conversation| PickerItem {
                    label: clean_text(&conversation.title),
                    detail: format!(
                        "{}{}{}",
                        conversation.id,
                        if conversation.id == current_id {
                            " [current]"
                        } else {
                            ""
                        },
                        if conversation.archived {
                            " [archived]"
                        } else {
                            ""
                        }
                    ),
                    value: conversation.id,
                })
                .collect(),
            query: String::new(),
            selected: 0,
        });
    }

    pub fn open_task_picker(&mut self, tasks: Vec<TaskSummary>) {
        self.task_detail = None;
        self.picker = Some(Picker {
            kind: PickerKind::Tasks,
            title: "Durable tasks",
            items: tasks
                .into_iter()
                .map(|task| {
                    let mut flags = Vec::new();
                    if task.waiting_on > 0 {
                        flags.push(format!("waiting:{}", task.waiting_on));
                    }
                    if task.cancel_requested {
                        flags.push("cancel requested".into());
                    }
                    if task.activity_id.is_some() {
                        flags.push("activity".into());
                    }
                    if task.error.is_some() {
                        flags.push("error".into());
                    }
                    let flags = if flags.is_empty() {
                        String::new()
                    } else {
                        format!(" | {}", flags.join(", "))
                    };
                    PickerItem {
                        label: bounded_clean_text(&task.title, 120).replace('\n', " "),
                        detail: format!(
                            "{} | {} | {}{}",
                            task.status,
                            task.created_at,
                            task.session_id.as_deref().unwrap_or("no session"),
                            flags
                        ),
                        value: task.id,
                    }
                })
                .collect(),
            query: String::new(),
            selected: 0,
        });
    }

    pub fn open_task_detail(&mut self, mut job: Job) {
        job.prompt = bounded_clean_text(&job.prompt, 8_192);
        job.response = job
            .response
            .as_deref()
            .map(|value| bounded_clean_text(value, 32_768));
        job.error = job
            .error
            .as_deref()
            .map(|value| bounded_clean_text(value, 8_192));
        self.picker = None;
        self.task_detail = Some(job);
        self.task_detail_scroll = 0;
    }

    pub fn close_task_detail(&mut self) {
        self.task_detail = None;
        self.task_detail_scroll = 0;
    }

    pub fn picker_insert(&mut self, value: char) {
        if let Some(picker) = &mut self.picker {
            if !value.is_control() && picker.query.len() < 256 {
                picker.query.push(value);
                picker.selected = 0;
            }
        }
    }

    pub fn picker_backspace(&mut self) {
        if let Some(picker) = &mut self.picker {
            picker.query.pop();
            picker.selected = 0;
        }
    }

    pub fn picker_move(&mut self, down: bool) {
        let count = self.picker_visible_indices().len();
        let Some(picker) = &mut self.picker else {
            return;
        };
        if count == 0 {
            picker.selected = 0;
        } else if down {
            picker.selected = (picker.selected + 1) % count;
        } else {
            picker.selected = picker.selected.checked_sub(1).unwrap_or(count - 1);
        }
    }

    pub fn close_picker(&mut self) {
        self.picker = None;
    }

    pub fn confirm_archive(&mut self) {
        self.confirmation = Some(Confirmation {
            title: "Archive conversation?".into(),
            body: "The conversation will leave the default session list. Canonical history, task evidence and admitted effects remain. It can be restored with /unarchive.".into(),
            confirm_label: "Archive".into(),
            action: ConfirmationAction::Archive,
        });
    }

    pub fn confirm_rewind(&mut self, user_turns: u32) {
        self.confirmation = Some(Confirmation {
            title: format!("Rewind {user_turns} user turn(s)?"),
            body: "Only retained conversation replay changes. Audit history, task evidence, files, processes and other admitted effects are not rolled back.".into(),
            confirm_label: "Rewind replay".into(),
            action: ConfirmationAction::Rewind(user_turns),
        });
    }

    pub fn close_confirmation(&mut self) {
        self.confirmation = None;
    }

    pub fn take_confirmation(&mut self) -> Option<ConfirmationAction> {
        self.confirmation
            .take()
            .map(|confirmation| confirmation.action)
    }

    pub fn picker_visible_indices(&self) -> Vec<usize> {
        let Some(picker) = &self.picker else {
            return Vec::new();
        };
        let query = picker.query.to_lowercase();
        picker
            .items
            .iter()
            .enumerate()
            .filter(|(_, item)| {
                query.is_empty()
                    || item.label.to_lowercase().contains(&query)
                    || item.detail.to_lowercase().contains(&query)
            })
            .map(|(index, _)| index)
            .collect()
    }

    pub fn take_picker_selection(&mut self) -> Option<PickerSelection> {
        let indices = self.picker_visible_indices();
        if indices.is_empty() {
            return None;
        }
        let picker = self.picker.take()?;
        let index = *indices.get(picker.selected.min(indices.len().saturating_sub(1)))?;
        let item = picker.items.get(index)?;
        Some(match picker.kind {
            PickerKind::Models => PickerSelection::Model(item.value.clone()),
            PickerKind::Sessions => PickerSelection::Session(item.value.clone()),
            PickerKind::Tasks => PickerSelection::Task(item.value.clone()),
        })
    }

    pub fn tick(&mut self) {
        self.frame = self.frame.wrapping_add(1);
    }

    pub fn task_elapsed(&self) -> Option<Duration> {
        self.task_started_at.map(|started| started.elapsed())
    }

    fn push_entry(&mut self, kind: EntryKind, text: String) {
        self.transcript_bytes = self.transcript_bytes.saturating_add(text.len());
        self.entries.push(Entry { kind, text });
        self.trim_transcript();
        self.scroll = 0;
    }

    fn trim_transcript(&mut self) {
        while self.entries.len() > MAX_TRANSCRIPT_ENTRIES
            || self.transcript_bytes > MAX_TRANSCRIPT_BYTES
        {
            let removed = self.entries.remove(0);
            self.transcript_bytes = self.transcript_bytes.saturating_sub(removed.text.len());
            self.active_assistant = self.active_assistant.and_then(|index| index.checked_sub(1));
            self.tool_entries.retain(|_, index| {
                if *index == 0 {
                    false
                } else {
                    *index -= 1;
                    true
                }
            });
        }
    }
}

pub(super) fn clean_text(value: &str) -> String {
    let redacted = crate::agent::safety::redact::Redactor::default_set().redact(value);
    redacted
        .chars()
        .map(|character| {
            if character == '\n' || character == '\t' || !character.is_control() {
                character
            } else {
                '\u{fffd}'
            }
        })
        .collect()
}

fn bounded_clean_text(value: &str, max_chars: usize) -> String {
    let value = clean_text(value);
    if value.chars().count() <= max_chars {
        value
    } else {
        format!(
            "{}\n[terminal view truncated]",
            value.chars().take(max_chars).collect::<String>()
        )
    }
}

fn byte_index(value: &str, character_index: usize) -> usize {
    value
        .char_indices()
        .nth(character_index)
        .map_or(value.len(), |(index, _)| index)
}

fn move_vertical(value: &str, cursor: &mut usize, up: bool) -> bool {
    let characters = value.chars().collect::<Vec<_>>();
    let cursor_value = (*cursor).min(characters.len());
    let line_start = characters[..cursor_value]
        .iter()
        .rposition(|character| *character == '\n')
        .map_or(0, |index| index + 1);
    let column = cursor_value.saturating_sub(line_start);
    if up {
        if line_start == 0 {
            return false;
        }
        let previous_end = line_start - 1;
        let previous_start = characters[..previous_end]
            .iter()
            .rposition(|character| *character == '\n')
            .map_or(0, |index| index + 1);
        *cursor = previous_start + column.min(previous_end - previous_start);
        return true;
    }
    let Some(current_end_offset) = characters[cursor_value..]
        .iter()
        .position(|character| *character == '\n')
    else {
        return false;
    };
    let next_start = cursor_value + current_end_offset + 1;
    let next_end = characters[next_start..]
        .iter()
        .position(|character| *character == '\n')
        .map_or(characters.len(), |offset| next_start + offset);
    *cursor = next_start + column.min(next_end - next_start);
    true
}
