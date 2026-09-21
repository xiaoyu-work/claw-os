use std::collections::{HashMap, HashSet, VecDeque};

use super::backend::{ApprovalRequest, BackendInfo, Conversation, Job};

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
    Succeeded,
    Failed,
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
    pub scroll: u16,
    pub usage_input: u64,
    pub usage_output: u64,
    pub usage_cached: u64,
    pub should_quit: bool,
    active_assistant: Option<usize>,
    provider_had_text: bool,
    tool_entries: HashMap<String, usize>,
    seen_approvals: HashSet<String>,
    transcript_bytes: usize,
}

impl App {
    pub fn new(info: BackendInfo, conversation: Conversation) -> Self {
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
            scroll: 0,
            usage_input: 0,
            usage_output: 0,
            usage_cached: 0,
            should_quit: false,
            active_assistant: None,
            provider_had_text: false,
            tool_entries: HashMap::new(),
            seen_approvals: HashSet::new(),
            transcript_bytes: 0,
        };
        app.load_history();
        app
    }

    pub fn replace_conversation(&mut self, conversation: Conversation) {
        self.conversation = conversation;
        self.entries.clear();
        self.transcript_bytes = 0;
        self.tool_entries.clear();
        self.pending_approvals.clear();
        self.seen_approvals.clear();
        self.active_assistant = None;
        self.provider_had_text = false;
        self.scroll = 0;
        self.status = RunStatus::Ready;
        self.active_task = None;
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

    pub fn tool_finished(&mut self, id: &str, name: &str, success: bool) {
        self.tool_started(id, name);
        if let Some(index) = self.tool_entries.get(id).copied() {
            if let EntryKind::Tool { status, .. } = &mut self.entries[index].kind {
                *status = if success {
                    ToolStatus::Succeeded
                } else {
                    ToolStatus::Failed
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
    }

    pub fn insert_text(&mut self, value: &str) {
        for value in value
            .chars()
            .filter(|value| !value.is_control() || *value == '\t')
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
    }

    pub fn delete(&mut self) {
        if self.cursor >= self.input.chars().count() {
            return;
        }
        let start = byte_index(&self.input, self.cursor);
        let end = byte_index(&self.input, self.cursor + 1);
        self.input.replace_range(start..end, "");
    }

    pub fn move_left(&mut self) {
        self.cursor = self.cursor.saturating_sub(1);
    }

    pub fn move_right(&mut self) {
        self.cursor = (self.cursor + 1).min(self.input.chars().count());
    }

    pub fn history_previous(&mut self) {
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

    pub fn history_next(&mut self) {
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
        let Some(completion) = super::commands::completion(&self.input) else {
            return;
        };
        self.input = completion;
        self.cursor = self.input.chars().count();
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

fn byte_index(value: &str, character_index: usize) -> usize {
    value
        .char_indices()
        .nth(character_index)
        .map_or(value.len(), |(index, _)| index)
}
