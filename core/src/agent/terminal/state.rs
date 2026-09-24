use std::collections::{HashMap, HashSet, VecDeque};
use std::time::{Duration, Instant};

use super::backend::{
    Activity, ActivityAttention, ActivityControls, ActivityDetail, ActivityEvidence,
    ActivityOperationPreview, ActivityReview, ApprovalRequest, BackendInfo, Conversation,
    ConversationJob,
    ConversationSummary, Job, NotificationItem, NotificationPage, NotificationPreferences,
    TaskSummary,
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
    Reconnecting,
    WaitingApproval,
    Cancelling,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum TerminalTheme {
    Cyan,
    Blue,
    Magenta,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum PickerKind {
    Models,
    Sessions,
    History,
    Files,
    Tasks,
    Approvals,
    Notifications,
    Activities,
    ActivityReviews,
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
    History {
        before_user_turn: u32,
        prompt: String,
    },
    File(String),
    Task(String),
    Approval(ApprovalRequest),
    Notification(NotificationItem),
    Activity(String),
    ActivityReview(String),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum NotificationPreferenceAction {
    ToggleWeb,
    ToggleDesktop,
    ToggleNtfy,
    ToggleDnd,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum ConfirmationAction {
    Archive,
    Rewind(u32),
    ActivityComplete { id: String, note: String },
    ActivityCancel { id: String },
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
    pub active_workspace: Option<String>,
    pub selected_model: String,
    pub selected_workspace: String,
    pub task_use_memory: bool,
    pub task_max_turns: Option<u32>,
    pub task_reasoning_effort: Option<String>,
    pub task_plan_only: bool,
    pub task_controls_open: bool,
    pub appearance_open: bool,
    pub agents_open: bool,
    pub agents_scroll: u16,
    pub terminal_theme: TerminalTheme,
    pub terminal_title_enabled: bool,
    pub compact_statusline: bool,
    pub vim_mode: bool,
    pub vim_insert_mode: bool,
    side_parent_id: Option<String>,
    side_conversation_id: Option<String>,
    pending_attachments: Vec<crate::agent::attachments::AttachmentInput>,
    pub queued_tasks: VecDeque<Job>,
    pub pending_approvals: VecDeque<ApprovalRequest>,
    pub input_history: Vec<String>,
    pub history_index: Option<usize>,
    pub command_selection: usize,
    pub picker: Option<Picker>,
    pub confirmation: Option<Confirmation>,
    pub task_detail: Option<Job>,
    pub task_detail_scroll: u16,
    pub approval_detail: Option<ApprovalRequest>,
    pub approval_detail_scroll: u16,
    pub notification_detail: Option<NotificationItem>,
    pub notification_detail_scroll: u16,
    pub notification_preferences: Option<NotificationPreferences>,
    pub notification_preferences_scroll: u16,
    pub activity_detail: Option<ActivityDetail>,
    pub activity_detail_scroll: u16,
    pub activity_attention: Option<ActivityAttention>,
    pub activity_attention_scroll: u16,
    pub activity_controls: Option<ActivityControls>,
    pub activity_controls_scroll: u16,
    pub activity_evidence: Option<ActivityEvidence>,
    pub activity_evidence_scroll: u16,
    pub activity_review: Option<ActivityReview>,
    pub activity_review_scroll: u16,
    pub activity_operation_preview: Option<ActivityOperationPreview>,
    pub activity_operation_preview_scroll: u16,
    pub scroll: u16,
    pub usage_input: u64,
    pub usage_output: u64,
    pub usage_cached: u64,
    pub should_quit: bool,
    raw_scrollback_requested: bool,
    pub frame: u64,
    task_started_at: Option<Instant>,
    active_assistant: Option<usize>,
    provider_had_text: bool,
    task_had_assistant_text: bool,
    tool_entries: HashMap<String, usize>,
    approval_catalog: HashMap<String, ApprovalRequest>,
    notification_catalog: HashMap<String, NotificationItem>,
    history_catalog: HashMap<String, (u32, String)>,
    seen_approvals: HashSet<String>,
    transcript_bytes: usize,
    backtrack_armed_at: Option<Instant>,
}

impl App {
    pub fn new(info: BackendInfo, mut conversation: Conversation) -> Self {
        conversation.title = clean_text(&conversation.title).replace('\n', " ");
        let selected_model = info.model.clone();
        let selected_workspace = info.home.to_string_lossy().into_owned();
        let mut app = Self {
            info,
            conversation,
            entries: Vec::new(),
            input: String::new(),
            cursor: 0,
            status: RunStatus::Ready,
            active_task: None,
            active_workspace: None,
            selected_model,
            selected_workspace,
            task_use_memory: true,
            task_max_turns: None,
            task_reasoning_effort: None,
            task_plan_only: false,
            task_controls_open: false,
            appearance_open: false,
            agents_open: false,
            agents_scroll: 0,
            terminal_theme: TerminalTheme::Cyan,
            terminal_title_enabled: false,
            compact_statusline: false,
            vim_mode: false,
            vim_insert_mode: true,
            side_parent_id: None,
            side_conversation_id: None,
            pending_attachments: Vec::new(),
            queued_tasks: VecDeque::new(),
            pending_approvals: VecDeque::new(),
            input_history: Vec::new(),
            history_index: None,
            command_selection: 0,
            picker: None,
            confirmation: None,
            task_detail: None,
            task_detail_scroll: 0,
            approval_detail: None,
            approval_detail_scroll: 0,
            notification_detail: None,
            notification_detail_scroll: 0,
            notification_preferences: None,
            notification_preferences_scroll: 0,
            activity_detail: None,
            activity_detail_scroll: 0,
            activity_attention: None,
            activity_attention_scroll: 0,
            activity_controls: None,
            activity_controls_scroll: 0,
            activity_evidence: None,
            activity_evidence_scroll: 0,
            activity_review: None,
            activity_review_scroll: 0,
            activity_operation_preview: None,
            activity_operation_preview_scroll: 0,
            scroll: 0,
            usage_input: 0,
            usage_output: 0,
            usage_cached: 0,
            should_quit: false,
            raw_scrollback_requested: false,
            frame: 0,
            task_started_at: None,
            active_assistant: None,
            provider_had_text: false,
            task_had_assistant_text: false,
            tool_entries: HashMap::new(),
            approval_catalog: HashMap::new(),
            notification_catalog: HashMap::new(),
            history_catalog: HashMap::new(),
            seen_approvals: HashSet::new(),
            transcript_bytes: 0,
            backtrack_armed_at: None,
        };
        app.load_history();
        if !app.info.provider_ready {
            app.push_system(
                "Text provider setup is incomplete. Local sessions, tasks, approvals, \
                 notifications, and Activities remain available; run `cos agent setup text` \
                 before submitting model work.",
            );
        }
        if let Some(warning) = app.info.model_catalog_warning.clone() {
            app.push_system(&format!(
                "{warning}. The configured model remains available in this terminal."
            ));
        }
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
        self.task_had_assistant_text = false;
        self.scroll = 0;
        self.command_selection = 0;
        self.picker = None;
        self.confirmation = None;
        self.task_detail = None;
        self.task_detail_scroll = 0;
        self.approval_detail = None;
        self.approval_detail_scroll = 0;
        self.approval_catalog.clear();
        self.notification_detail = None;
        self.notification_detail_scroll = 0;
        self.notification_preferences = None;
        self.notification_preferences_scroll = 0;
        self.notification_catalog.clear();
        self.history_catalog.clear();
        self.activity_detail = None;
        self.activity_detail_scroll = 0;
        self.activity_attention = None;
        self.activity_attention_scroll = 0;
        self.activity_controls = None;
        self.activity_controls_scroll = 0;
        self.activity_evidence = None;
        self.activity_evidence_scroll = 0;
        self.activity_review = None;
        self.activity_review_scroll = 0;
        self.task_controls_open = false;
        self.appearance_open = false;
        self.agents_open = false;
        self.agents_scroll = 0;
        self.activity_operation_preview = None;
        self.activity_operation_preview_scroll = 0;
        self.status = RunStatus::Ready;
        self.active_task = None;
        self.active_workspace = None;
        self.task_started_at = None;
        self.queued_tasks.clear();
        self.usage_input = 0;
        self.usage_output = 0;
        self.usage_cached = 0;
        self.pending_attachments.clear();
        self.backtrack_armed_at = None;
        self.raw_scrollback_requested = false;
        self.clear_side_conversation();
        self.load_history();
    }

    fn load_history(&mut self) {
        if self.conversation.history_truncated {
            self.push_system(
                "Only the most recent bounded conversation history is shown in this terminal.",
            );
        }
        if self.conversation.jobs_truncated {
            self.push_system(
                "Only the most recent bounded durable-task metadata is available for this conversation.",
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
        self.active_workspace = job.workspace.clone();
        self.status = RunStatus::Working;
        self.task_started_at = Some(Instant::now());
        self.active_assistant = None;
        self.provider_had_text = false;
        self.task_had_assistant_text = false;
        self.push_entry(EntryKind::User, clean_text(&job.prompt));
        self.scroll = 0;
    }

    pub fn resume_task(&mut self, job: &Job, queued_successors: usize) {
        self.active_task = Some(job.id.clone());
        self.active_workspace = job.workspace.clone();
        self.status = RunStatus::Working;
        self.task_started_at = Some(Instant::now());
        self.active_assistant = None;
        self.provider_had_text = false;
        self.task_had_assistant_text = false;
        self.push_system(&format!(
            "Reattached durable task {}{}.",
            job.id,
            if queued_successors == 0 {
                String::new()
            } else {
                format!(" with {queued_successors} queued successor(s)")
            }
        ));
        self.scroll = 0;
    }

    pub fn restore_queued_task(&mut self, job: Job) {
        self.queued_tasks.push_back(job);
    }

    pub fn restore_selected_model(&mut self, model: Option<&str>) {
        let Some(model) = model else {
            return;
        };
        if self.info.models.iter().any(|candidate| candidate == model) {
            self.selected_model = model.to_string();
        } else {
            self.push_system(&format!(
                "The conversation's last requested model {model} is no longer available; \
                 future tasks will use {}.",
                self.selected_model
            ));
        }
    }

    pub fn take_conversation_jobs(&mut self) -> Vec<ConversationJob> {
        std::mem::take(&mut self.conversation.jobs)
    }

    pub fn finish_task(&mut self, job: &Job) {
        if self.active_task.as_deref() != Some(job.id.as_str()) {
            self.push_error("Claw returned a terminal result for another task.");
            return;
        }
        if !self.task_had_assistant_text {
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
        self.active_workspace = None;
        self.status = RunStatus::Ready;
        self.task_started_at = None;
        self.active_assistant = None;
        self.provider_had_text = false;
        self.task_had_assistant_text = false;
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
        self.task_had_assistant_text = true;
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
            self.task_detail = None;
            self.approval_detail = None;
            self.notification_detail = None;
            self.notification_preferences = None;
            self.activity_detail = None;
            self.activity_attention = None;
            self.activity_controls = None;
            self.activity_evidence = None;
            self.activity_operation_preview = None;
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

    pub fn queue_task(&mut self, job: Job) {
        self.push_system(&format!(
            "Queued durable task {} after {}: {}",
            job.id,
            job.after_task_id.as_deref().unwrap_or("current queue"),
            clean_text(&job.prompt)
        ));
        self.queued_tasks.push_back(job);
    }

    pub fn set_workspace(&mut self, workspace: String) {
        self.selected_workspace = workspace;
        self.push_system(&format!(
            "Future tasks will use workspace {}. Workspace selection grants no capability.",
            self.selected_workspace
        ));
    }

    pub fn set_task_defaults(&mut self, use_memory: bool, max_turns: Option<u32>) {
        self.task_use_memory = use_memory;
        self.task_max_turns = max_turns;
    }

    pub fn open_task_controls(&mut self) {
        self.picker = None;
        self.task_controls_open = true;
    }

    pub fn close_task_controls(&mut self) {
        self.task_controls_open = false;
    }

    pub fn toggle_task_memory(&mut self) {
        self.task_use_memory = !self.task_use_memory;
    }

    pub fn cycle_task_max_turns(&mut self) {
        self.task_max_turns = match self.task_max_turns {
            None => Some(8),
            Some(8) => Some(16),
            Some(16) => Some(32),
            Some(_) => None,
        };
    }

    pub fn cycle_task_reasoning_effort(&mut self) {
        if self.info.provider != "copilot" {
            self.push_error(
                "Per-task reasoning effort currently requires the Copilot Responses provider.",
            );
            return;
        }

        self.task_reasoning_effort = match self.task_reasoning_effort.as_deref() {
            None => Some("minimal".into()),
            Some("minimal") => Some("low".into()),
            Some("low") => Some("medium".into()),
            Some("medium") => Some("high".into()),
            Some("high") => Some("xhigh".into()),
            Some(_) => None,
        };
    }

    pub fn toggle_task_plan_mode(&mut self) {
        self.task_plan_only = !self.task_plan_only;
    }

    pub fn open_appearance(&mut self) {
        self.picker = None;
        self.appearance_open = true;
    }

    pub fn close_appearance(&mut self) {
        self.appearance_open = false;
    }

    pub fn open_agents(&mut self) {
        self.picker = None;
        self.agents_open = true;
        self.agents_scroll = 0;
    }

    pub fn close_agents(&mut self) {
        self.agents_open = false;
        self.agents_scroll = 0;
    }

    pub fn delegate_summaries(&self) -> Vec<(String, &'static str)> {
        self.entries
            .iter()
            .filter_map(|entry| match &entry.kind {
                EntryKind::Tool { id, status } if entry.text == "cos_delegate" => Some((
                    id.clone(),
                    match status {
                        ToolStatus::Running => "running",
                        ToolStatus::Succeeded { .. } => "completed",
                        ToolStatus::Failed { .. } => "failed",
                    },
                )),
                _ => None,
            })
            .collect()
    }

    pub fn cycle_theme(&mut self) {
        self.terminal_theme = match self.terminal_theme {
            TerminalTheme::Cyan => TerminalTheme::Blue,
            TerminalTheme::Blue => TerminalTheme::Magenta,
            TerminalTheme::Magenta => TerminalTheme::Cyan,
        };
    }

    pub fn toggle_terminal_title(&mut self) {
        self.terminal_title_enabled = !self.terminal_title_enabled;
    }

    pub fn toggle_statusline(&mut self) {
        self.compact_statusline = !self.compact_statusline;
    }

    pub fn toggle_vim_mode(&mut self) {
        self.vim_mode = !self.vim_mode;
        self.vim_insert_mode = !self.vim_mode;
    }

    pub fn enter_vim_insert(&mut self) {
        self.vim_insert_mode = true;
    }

    pub fn leave_vim_insert(&mut self) {
        self.vim_insert_mode = false;
    }

    pub fn keymap_name(&self) -> &'static str {
        if self.vim_mode {
            "vim"
        } else {
            "emacs"
        }
    }

    pub fn theme_name(&self) -> &'static str {
        match self.terminal_theme {
            TerminalTheme::Cyan => "cyan",
            TerminalTheme::Blue => "blue",
            TerminalTheme::Magenta => "magenta",
        }
    }

    pub fn terminal_title(&self) -> Option<String> {
        self.terminal_title_enabled
            .then(|| format!("Claw - {}", self.conversation.title))
    }

    pub fn begin_side_conversation(&mut self, parent_id: String, side_id: String) {
        self.side_parent_id = Some(parent_id);
        self.side_conversation_id = Some(side_id);
    }

    pub fn side_conversation(&self) -> Option<(&str, &str)> {
        Some((
            self.side_parent_id.as_deref()?,
            self.side_conversation_id.as_deref()?,
        ))
    }

    pub fn clear_side_conversation(&mut self) {
        self.side_parent_id = None;
        self.side_conversation_id = None;
    }

    pub fn in_side_conversation(&self) -> bool {
        self.side_parent_id.is_some()
    }

    pub fn restore_reasoning_effort(&mut self, effort: Option<&str>) {
        self.task_reasoning_effort = effort.map(str::to_string);
    }

    pub fn restore_plan_mode(&mut self, plan_only: bool) {
        self.task_plan_only = plan_only;
    }

    pub fn pending_attachments(&self) -> &[crate::agent::attachments::AttachmentInput] {
        &self.pending_attachments
    }

    pub fn pending_attachment_count(&self) -> usize {
        self.pending_attachments.len()
    }

    pub fn add_attachment(
        &mut self,
        attachment: crate::agent::attachments::AttachmentInput,
    ) -> Result<(), String> {
        let mut combined = self.pending_attachments.clone();
        combined.push(attachment.clone());
        crate::agent::attachments::normalize(combined)?;
        self.pending_attachments.push(attachment);
        self.push_system(&format!(
            "Attached image {} for the next submitted task.",
            self.pending_attachments
                .last()
                .map(|attachment| attachment.name.as_str())
                .unwrap_or("image")
        ));
        Ok(())
    }

    pub fn clear_attachments(&mut self) {
        let count = self.pending_attachments.len();
        self.pending_attachments.clear();
        self.push_system(&format!("Cleared {count} pending image attachment(s)."));
    }

    pub fn consume_attachments(&mut self) {
        self.pending_attachments.clear();
    }

    pub fn describe_attachments(&mut self) {
        if self.pending_attachments.is_empty() {
            self.push_system("No images are attached to the next task.");
            return;
        }

        let names = self
            .pending_attachments
            .iter()
            .map(|attachment| attachment.name.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        self.push_system(&format!(
            "{} image attachment(s) pending: {names}",
            self.pending_attachments.len()
        ));
    }

    pub fn last_assistant_text(&self) -> Option<&str> {
        self.entries.iter().rev().find_map(|entry| {
            matches!(&entry.kind, EntryKind::Assistant).then_some(entry.text.as_str())
        })
    }

    pub fn export_markdown(&self) -> Result<String, String> {
        let mut output = String::from("# Claw conversation\n\n");
        let mut messages = 0usize;
        for entry in &self.entries {
            let heading = match &entry.kind {
                EntryKind::User => "User",
                EntryKind::Assistant => "Assistant",
                _ => continue,
            };
            output.push_str("## ");
            output.push_str(heading);
            output.push_str("\n\n");
            output.push_str(&entry.text);
            output.push_str("\n\n");
            messages += 1;
        }
        if messages == 0 {
            return Err("There are no user or assistant messages to export.".into());
        }
        Ok(output)
    }

    pub fn request_raw_scrollback(&mut self) {
        self.raw_scrollback_requested = true;
    }

    pub fn take_raw_scrollback(&mut self) -> Option<String> {
        if !std::mem::take(&mut self.raw_scrollback_requested) {
            return None;
        }
        let mut output = String::new();
        for entry in &self.entries {
            let label = match &entry.kind {
                EntryKind::User => "user",
                EntryKind::Assistant => "assistant",
                EntryKind::Reasoning => "reasoning",
                EntryKind::Tool { .. } => "tool",
                EntryKind::System => "system",
                EntryKind::Error => "error",
                EntryKind::Approval { .. } => "approval",
            };
            output.push('[');
            output.push_str(label);
            output.push_str("] ");
            output.push_str(&entry.text);
            output.push_str("\n\n");
        }
        Some(output)
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
        self.backtrack_armed_at = None;
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

    pub fn open_history_picker(&mut self) {
        self.history_catalog.clear();
        let mut items = Vec::new();
        let mut user_turn = 0u32;
        for message in &self.conversation.messages {
            if message.role != "user" {
                continue;
            }
            let value = format!("user-turn-{user_turn}");
            let prompt = bounded_clean_text(&message.text, MAX_PROMPT_BYTES);
            let label = prompt
                .lines()
                .next()
                .map(|line| bounded_clean_text(line, 120))
                .unwrap_or_default();
            self.history_catalog
                .insert(value.clone(), (user_turn, prompt));
            items.push(PickerItem {
                label,
                detail: format!("turn {} - fork before and edit", user_turn + 1),
                value,
            });
            user_turn = user_turn.saturating_add(1);
        }
        if items.is_empty() {
            self.push_system("This conversation has no retained user turns to backtrack.");
            return;
        }
        self.picker = Some(Picker {
            kind: PickerKind::History,
            title: "Backtrack conversation",
            items,
            query: String::new(),
            selected: 0,
        });
    }

    pub fn open_file_picker(&mut self, files: super::commands::WorkspaceFiles) {
        if files.paths.is_empty() {
            self.push_system("The selected workspace has no files available for mention.");
            return;
        }
        if files.truncated {
            self.push_system("File mention picker is limited to the first 512 bounded entries.");
        }
        self.picker = Some(Picker {
            kind: PickerKind::Files,
            title: "Mention workspace file",
            items: files
                .paths
                .into_iter()
                .map(|path| PickerItem {
                    label: path.clone(),
                    detail: "path only - no file read or capability".into(),
                    value: path,
                })
                .collect(),
            query: String::new(),
            selected: 0,
        });
    }

    pub fn insert_file_mention(&mut self, path: &str) {
        self.insert_text(&format!("@{path} "));
    }

    pub fn handle_idle_escape(&mut self) {
        if !self.input.is_empty() {
            self.input.clear();
            self.cursor = 0;
            self.backtrack_armed_at = None;
            return;
        }
        if self
            .backtrack_armed_at
            .is_some_and(|armed| armed.elapsed() <= Duration::from_millis(1_500))
        {
            self.backtrack_armed_at = None;
            self.open_history_picker();
        } else {
            self.backtrack_armed_at = Some(Instant::now());
        }
    }

    pub fn backtrack_armed(&self) -> bool {
        self.backtrack_armed_at.is_some()
    }

    pub fn disarm_backtrack(&mut self) {
        self.backtrack_armed_at = None;
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
                    if let Some(workspace) = task.workspace.as_deref() {
                        flags.push(format!("cwd:{workspace}"));
                    }
                    if task.after_task_id.is_some() {
                        flags.push("sequenced".into());
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
        self.approval_detail = None;
        self.notification_detail = None;
        self.notification_preferences = None;
        self.activity_detail = None;
        self.activity_attention = None;
        self.activity_controls = None;
        self.task_detail = Some(job);
        self.task_detail_scroll = 0;
    }

    pub fn close_task_detail(&mut self) {
        self.task_detail = None;
        self.task_detail_scroll = 0;
    }

    pub fn open_approval_picker(&mut self, approvals: Vec<ApprovalRequest>) {
        self.task_detail = None;
        self.approval_detail = None;
        self.approval_catalog = approvals
            .iter()
            .map(|approval| (approval.id.clone(), approval.clone()))
            .collect();
        self.picker = Some(Picker {
            kind: PickerKind::Approvals,
            title: "Approval center",
            items: approvals
                .into_iter()
                .map(|approval| {
                    let subject = approval
                        .requester
                        .as_deref()
                        .unwrap_or(approval.session.as_str());
                    PickerItem {
                        label: format!(
                            "{} {}",
                            approval.status.to_uppercase(),
                            bounded_clean_text(&approval.verb, 80)
                        ),
                        detail: format!(
                            "{} | {} | {}",
                            approval.risk.as_deref().unwrap_or("unclassified"),
                            approval.requested_at,
                            bounded_clean_text(subject, 120).replace('\n', " ")
                        ),
                        value: approval.id,
                    }
                })
                .collect(),
            query: String::new(),
            selected: 0,
        });
    }

    pub fn open_approval_detail(&mut self, mut approval: ApprovalRequest) {
        approval.verb = bounded_clean_text(&approval.verb, 256).replace('\n', " ");
        approval.reason = bounded_clean_text(&approval.reason, 8_192);
        approval.session = bounded_clean_text(&approval.session, 512).replace('\n', " ");
        approval.requester = approval
            .requester
            .as_deref()
            .map(|value| bounded_clean_text(value, 512).replace('\n', " "));
        approval.note = approval
            .note
            .as_deref()
            .map(|value| bounded_clean_text(value, 2_048));
        self.picker = None;
        self.task_detail = None;
        self.notification_detail = None;
        self.notification_preferences = None;
        self.activity_detail = None;
        self.activity_attention = None;
        self.activity_controls = None;
        self.approval_detail = Some(approval);
        self.approval_detail_scroll = 0;
    }

    pub fn close_approval_detail(&mut self) {
        self.approval_detail = None;
        self.approval_detail_scroll = 0;
    }

    pub fn open_notification_picker(&mut self, page: NotificationPage) {
        self.task_detail = None;
        self.approval_detail = None;
        self.notification_detail = None;
        self.notification_preferences = None;
        self.notification_catalog = page
            .notifications
            .iter()
            .map(|notification| (notification.id.clone(), notification.clone()))
            .collect();
        self.push_system(&format!(
            "Notification Inbox: {} unread in this bounded view.",
            page.unread
        ));
        self.picker = Some(Picker {
            kind: PickerKind::Notifications,
            title: "Notification Inbox",
            items: page
                .notifications
                .into_iter()
                .map(|notification| PickerItem {
                    label: format!(
                        "{} {} {}",
                        notification.state.to_uppercase(),
                        notification.severity.to_uppercase(),
                        bounded_clean_text(&notification.title, 120).replace('\n', " ")
                    ),
                    detail: format!(
                        "{} | {} | {} | x{}",
                        notification.source,
                        notification.kind,
                        notification.updated_at_ms,
                        notification.occurrences
                    ),
                    value: notification.id,
                })
                .collect(),
            query: String::new(),
            selected: 0,
        });
    }

    pub fn open_notification_detail(&mut self, mut notification: NotificationItem) {
        notification.title = bounded_clean_text(&notification.title, 512).replace('\n', " ");
        notification.body = bounded_clean_text(&notification.body, 16_384);
        notification.source = bounded_clean_text(&notification.source, 256).replace('\n', " ");
        notification.kind = bounded_clean_text(&notification.kind, 256).replace('\n', " ");
        for action in &mut notification.actions {
            action.label = bounded_clean_text(&action.label, 256).replace('\n', " ");
            action.uri = bounded_clean_text(&action.uri, 2_048).replace('\n', " ");
        }
        self.picker = None;
        self.task_detail = None;
        self.approval_detail = None;
        self.notification_preferences = None;
        self.activity_detail = None;
        self.activity_attention = None;
        self.activity_controls = None;
        self.notification_detail = Some(notification);
        self.notification_detail_scroll = 0;
    }

    pub fn close_notification_detail(&mut self) {
        self.notification_detail = None;
        self.notification_detail_scroll = 0;
    }

    pub fn open_notification_preferences(&mut self, preferences: NotificationPreferences) {
        self.picker = None;
        self.task_detail = None;
        self.approval_detail = None;
        self.notification_detail = None;
        self.activity_detail = None;
        self.activity_attention = None;
        self.activity_controls = None;
        self.notification_preferences = Some(preferences);
        self.notification_preferences_scroll = 0;
    }

    pub fn close_notification_preferences(&mut self) {
        self.notification_preferences = None;
        self.notification_preferences_scroll = 0;
    }

    pub fn open_activity_picker(&mut self, activities: Vec<Activity>) {
        self.activity_detail = None;
        self.activity_attention = None;
        self.activity_controls = None;
        self.picker = Some(Picker {
            kind: PickerKind::Activities,
            title: "Activities",
            items: activities
                .into_iter()
                .map(|activity| PickerItem {
                    label: format!(
                        "{} {}",
                        activity.state.to_uppercase(),
                        bounded_clean_text(&activity.title, 120).replace('\n', " ")
                    ),
                    detail: format!(
                        "{} | {}",
                        bounded_clean_text(&activity.goal, 160).replace('\n', " "),
                        activity.updated_at
                    ),
                    value: activity.id,
                })
                .collect(),
            query: String::new(),
            selected: 0,
        });
    }

    pub fn open_activity_review_picker(&mut self, activities: Vec<Activity>) {
        self.activity_detail = None;
        self.activity_attention = None;
        self.activity_controls = None;
        self.activity_evidence = None;
        self.activity_review = None;
        self.picker = Some(Picker {
            kind: PickerKind::ActivityReviews,
            title: "Review staged file plans",
            items: activities
                .into_iter()
                .map(|activity| PickerItem {
                    label: format!(
                        "{} {}",
                        activity.state.to_uppercase(),
                        bounded_clean_text(&activity.title, 120).replace('\n', " ")
                    ),
                    detail: format!(
                        "{} | {}",
                        bounded_clean_text(&activity.goal, 160).replace('\n', " "),
                        activity.updated_at
                    ),
                    value: activity.id,
                })
                .collect(),
            query: String::new(),
            selected: 0,
        });
    }

    pub fn open_activity_detail(&mut self, mut detail: ActivityDetail) {
        sanitize_activity(&mut detail.activity);
        for job in &mut detail.jobs {
            job.title = bounded_clean_text(&job.title, 512).replace('\n', " ");
            job.response = job
                .response
                .as_deref()
                .map(|value| bounded_clean_text(value, 4_096));
            job.error = job
                .error
                .as_deref()
                .map(|value| bounded_clean_text(value, 2_048));
        }
        self.picker = None;
        self.task_detail = None;
        self.approval_detail = None;
        self.notification_detail = None;
        self.notification_preferences = None;
        self.activity_attention = None;
        self.activity_controls = None;
        self.activity_review = None;
        self.activity_detail = Some(detail);
        self.activity_detail_scroll = 0;
    }

    pub fn update_activity(&mut self, mut activity: Activity) {
        sanitize_activity(&mut activity);
        if let Some(detail) = &mut self.activity_detail {
            if detail.activity.id == activity.id {
                detail.activity = activity;
            }
        }
    }

    pub fn close_activity_detail(&mut self) {
        self.activity_detail = None;
        self.activity_detail_scroll = 0;
    }

    pub fn open_activity_attention(&mut self, mut attention: ActivityAttention) {
        attention.presentation = bounded_clean_text(&attention.presentation, 256 * 1024);
        self.picker = None;
        self.task_detail = None;
        self.approval_detail = None;
        self.notification_detail = None;
        self.notification_preferences = None;
        self.activity_detail = None;
        self.activity_controls = None;
        self.activity_attention = Some(attention);
        self.activity_attention_scroll = 0;
    }

    pub fn close_activity_attention(&mut self) {
        self.activity_attention = None;
        self.activity_attention_scroll = 0;
    }

    pub fn open_activity_controls(&mut self, controls: ActivityControls) {
        self.picker = None;
        self.task_detail = None;
        self.approval_detail = None;
        self.notification_detail = None;
        self.notification_preferences = None;
        self.activity_detail = None;
        self.activity_attention = None;
        self.activity_controls = Some(controls);
        self.activity_controls_scroll = 0;
    }

    pub fn close_activity_controls(&mut self) {
        self.activity_controls = None;
        self.activity_controls_scroll = 0;
    }

    pub fn open_activity_evidence(&mut self, mut evidence: ActivityEvidence) {
        evidence.presentation = bounded_clean_text(&evidence.presentation, 1024 * 1024);
        self.picker = None;
        self.task_detail = None;
        self.approval_detail = None;
        self.notification_detail = None;
        self.notification_preferences = None;
        self.activity_detail = None;
        self.activity_attention = None;
        self.activity_controls = None;
        self.activity_operation_preview = None;
        self.activity_review = None;
        self.activity_evidence = Some(evidence);
        self.activity_evidence_scroll = 0;
    }

    pub fn close_activity_evidence(&mut self) {
        self.activity_evidence = None;
        self.activity_evidence_scroll = 0;
    }

    pub fn open_activity_review(&mut self, mut review: ActivityReview) {
        review.presentation = bounded_clean_text(&review.presentation, 1024 * 1024);
        self.picker = None;
        self.task_detail = None;
        self.approval_detail = None;
        self.notification_detail = None;
        self.notification_preferences = None;
        self.activity_detail = None;
        self.activity_attention = None;
        self.activity_controls = None;
        self.activity_evidence = None;
        self.activity_operation_preview = None;
        self.activity_review = Some(review);
        self.activity_review_scroll = 0;
    }

    pub fn close_activity_review(&mut self) {
        self.activity_review = None;
        self.activity_review_scroll = 0;
    }

    pub fn open_activity_operation_preview(&mut self, mut preview: ActivityOperationPreview) {
        preview.presentation = bounded_clean_text(&preview.presentation, 256 * 1024);
        self.picker = None;
        self.task_detail = None;
        self.approval_detail = None;
        self.notification_detail = None;
        self.notification_preferences = None;
        self.activity_detail = None;
        self.activity_attention = None;
        self.activity_controls = None;
        self.activity_evidence = None;
        self.activity_operation_preview = Some(preview);
        self.activity_operation_preview_scroll = 0;
    }

    pub fn close_activity_operation_preview(&mut self) {
        self.activity_operation_preview = None;
        self.activity_operation_preview_scroll = 0;
    }

    pub fn confirm_activity_complete(&mut self, id: String, note: String) {
        self.confirmation = Some(Confirmation {
            title: "Complete Activity goal?".into(),
            body: format!(
                "This explicitly records goal completion with note: {}. A successful Job alone never proves completion, and admitted effects are not changed.",
                bounded_clean_text(&note, 512).replace('\n', " ")
            ),
            confirm_label: "Complete Activity".into(),
            action: ConfirmationAction::ActivityComplete { id, note },
        });
    }

    pub fn confirm_activity_cancel(&mut self, id: String) {
        self.confirmation = Some(Confirmation {
            title: "Cancel Activity goal?".into(),
            body: "This ends the goal without claiming success. It does not cancel an in-flight task or undo admitted effects.".into(),
            confirm_label: "Cancel Activity".into(),
            action: ConfirmationAction::ActivityCancel { id },
        });
    }

    pub fn prefill_input(&mut self, value: String) {
        self.input = value;
        self.cursor = self.input.chars().count();
        self.command_selection = 0;
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
            PickerKind::History => {
                let (before_user_turn, prompt) =
                    self.history_catalog.remove(&item.value)?;
                PickerSelection::History {
                    before_user_turn,
                    prompt,
                }
            }
            PickerKind::Files => PickerSelection::File(item.value.clone()),
            PickerKind::Tasks => PickerSelection::Task(item.value.clone()),
            PickerKind::Approvals => {
                PickerSelection::Approval(self.approval_catalog.get(&item.value)?.clone())
            }
            PickerKind::Notifications => {
                PickerSelection::Notification(self.notification_catalog.get(&item.value)?.clone())
            }
            PickerKind::Activities => PickerSelection::Activity(item.value.clone()),
            PickerKind::ActivityReviews => {
                PickerSelection::ActivityReview(item.value.clone())
            }
        })
    }

    pub fn tick(&mut self) {
        self.frame = self.frame.wrapping_add(1);
        if self
            .backtrack_armed_at
            .is_some_and(|armed| armed.elapsed() > Duration::from_millis(1_500))
        {
            self.backtrack_armed_at = None;
        }
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

fn sanitize_activity(activity: &mut Activity) {
    activity.title = bounded_clean_text(&activity.title, 512).replace('\n', " ");
    activity.goal = bounded_clean_text(&activity.goal, 16_384);
    activity.completion_criteria = bounded_clean_text(&activity.completion_criteria, 8_192);
    activity.boundaries = bounded_clean_text(&activity.boundaries, 8_192);
    activity.completion_note = activity
        .completion_note
        .as_deref()
        .map(|value| bounded_clean_text(value, 8_192));
    for resource in &mut activity.resources {
        resource.label = bounded_clean_text(&resource.label, 512).replace('\n', " ");
        resource.reference = bounded_clean_text(&resource.reference, 4_096).replace('\n', " ");
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
