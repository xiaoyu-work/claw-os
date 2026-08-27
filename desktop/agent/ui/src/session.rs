use std::time::{Instant, SystemTime, UNIX_EPOCH};

use cosmic::widget;

use crate::bridge::{HistoryMessage, SessionSummary, ToolCallView, ToolResultView};
use crate::fl;

const MAX_BRANCH_CONTEXT_CHARS: usize = 32 * 1024;
const MAX_BRANCH_MESSAGE_CHARS: usize = 4 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ChatRole {
    User,
    Assistant,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct ChatMessage {
    pub(crate) role: Option<ChatRole>,
    pub(crate) content: String,
    pub(crate) tool_calls: Vec<ToolCallView>,
    pub(crate) tool_results: Vec<ToolResultView>,
    pub(crate) warnings: Vec<String>,
    pub(crate) error: Option<String>,
    pub(crate) parsed_markdown: Option<Vec<widget::markdown::Item>>,
    pub(crate) in_progress: bool,
}

impl ChatMessage {
    pub(crate) fn user(content: String) -> Self {
        Self {
            role: Some(ChatRole::User),
            content,
            ..Self::default()
        }
    }

    pub(crate) fn assistant_streaming() -> Self {
        Self {
            role: Some(ChatRole::Assistant),
            in_progress: true,
            ..Self::default()
        }
    }

    pub(crate) fn role(&self) -> ChatRole {
        self.role.clone().unwrap_or(ChatRole::Assistant)
    }

    pub(crate) fn refresh_markdown(&mut self) {
        self.parsed_markdown = if self.content.trim().is_empty() {
            None
        } else {
            let items = widget::markdown::parse(&self.content).collect::<Vec<_>>();
            (!items.is_empty()).then_some(items)
        };
    }

    fn is_visibly_empty(&self) -> bool {
        self.content.trim().is_empty()
            && self.tool_calls.is_empty()
            && self.tool_results.is_empty()
            && self.warnings.is_empty()
            && self.error.is_none()
    }

    pub(crate) fn upsert_tool_call(&mut self, call: ToolCallView) {
        if !call.id.is_empty()
            && let Some(existing) = self
                .tool_calls
                .iter_mut()
                .find(|existing| existing.id == call.id)
        {
            *existing = call;
        } else {
            self.tool_calls.push(call);
        }
    }

    pub(crate) fn upsert_tool_result(&mut self, result: ToolResultView) {
        if !result.id.is_empty()
            && let Some(existing) = self
                .tool_results
                .iter_mut()
                .find(|existing| existing.id == result.id)
        {
            *existing = result;
        } else {
            self.tool_results.push(result);
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) enum HistoryState {
    #[default]
    NotLoaded,
    Loading,
    Loaded,
    Failed(String),
}

#[derive(Debug, Clone)]
pub(crate) struct LocalSession {
    pub(crate) title: String,
    started_at: Instant,
    pub(crate) messages: Vec<ChatMessage>,
    pub(crate) remote_id: Option<String>,
    pub(crate) provisional_remote_id: Option<String>,
    pub(crate) persistent_context: Option<String>,
    pub(crate) history: HistoryState,
    pub(crate) last_ts_ms: Option<i64>,
    pub(crate) message_count: i64,
}

impl LocalSession {
    fn new() -> Self {
        Self {
            title: String::new(),
            started_at: Instant::now(),
            messages: Vec::new(),
            remote_id: None,
            provisional_remote_id: None,
            persistent_context: None,
            history: HistoryState::Loaded,
            last_ts_ms: None,
            message_count: 0,
        }
    }

    fn from_summary(summary: &SessionSummary) -> Self {
        Self {
            title: summary.title.clone(),
            started_at: Instant::now(),
            messages: Vec::new(),
            remote_id: Some(summary.id.clone()),
            provisional_remote_id: None,
            persistent_context: None,
            history: HistoryState::NotLoaded,
            last_ts_ms: summary.last_ts_ms,
            message_count: summary.message_count,
        }
    }

    pub(crate) fn display_title(&self) -> String {
        if self.title.trim().is_empty() {
            fl!("new-session")
        } else {
            self.title.clone()
        }
    }

    pub(crate) fn relative_label(&self) -> String {
        if let Some(timestamp) = self.last_ts_ms {
            return relative_time_label(timestamp, now_ms());
        }
        let seconds = self.started_at.elapsed().as_secs();
        if seconds < 60 {
            fl!("just-now")
        } else if seconds < 3_600 {
            format!("{}m", seconds / 60)
        } else if seconds < 86_400 {
            format!("{}h", seconds / 3_600)
        } else {
            format!("{}d", seconds / 86_400)
        }
    }
}

#[derive(Debug)]
pub(crate) struct SessionState {
    sessions: Vec<LocalSession>,
    active: usize,
    error: Option<String>,
}

impl Default for SessionState {
    fn default() -> Self {
        Self {
            sessions: vec![LocalSession::new()],
            active: 0,
            error: None,
        }
    }
}

impl SessionState {
    pub(crate) fn active_index(&self) -> usize {
        self.active
    }

    pub(crate) fn active(&self) -> Option<&LocalSession> {
        self.sessions.get(self.active)
    }

    pub(crate) fn active_mut(&mut self) -> Option<&mut LocalSession> {
        self.sessions.get_mut(self.active)
    }

    pub(crate) fn get(&self, index: usize) -> Option<&LocalSession> {
        self.sessions.get(index)
    }

    pub(crate) fn get_mut(&mut self, index: usize) -> Option<&mut LocalSession> {
        self.sessions.get_mut(index)
    }

    pub(crate) fn iter(&self) -> impl Iterator<Item = &LocalSession> {
        self.sessions.iter()
    }

    pub(crate) fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    pub(crate) fn set_error(&mut self, error: Option<String>) {
        self.error = error;
    }

    pub(crate) fn select(&mut self, index: usize) -> bool {
        if index >= self.sessions.len() {
            return false;
        }
        self.active = index;
        true
    }

    pub(crate) fn new_session(&mut self) {
        self.sessions.push(LocalSession::new());
        self.active = self.sessions.len() - 1;
    }

    pub(crate) fn retry_session(
        &mut self,
        prefix: Vec<ChatMessage>,
        context: String,
        title: String,
    ) {
        let mut session = LocalSession::new();
        session.title = fl!("retry-title", title = title);
        session.messages = prefix;
        session.message_count = session.messages.len() as i64;
        session.persistent_context = (!context.is_empty()).then_some(context);
        self.sessions.push(session);
        self.active = self.sessions.len() - 1;
    }

    pub(crate) fn merge_remote(&mut self, summaries: Vec<SessionSummary>) {
        for summary in summaries {
            if let Some(existing) = self.sessions.iter_mut().find(|session| {
                session.remote_id.as_deref() == Some(summary.id.as_str())
                    || session.provisional_remote_id.as_deref() == Some(summary.id.as_str())
            }) {
                if existing.title.trim().is_empty() && !summary.title.trim().is_empty() {
                    existing.title = summary.title;
                }
                existing.last_ts_ms = summary.last_ts_ms.or(existing.last_ts_ms);
                existing.message_count = existing.message_count.max(summary.message_count);
                continue;
            }
            self.sessions.push(LocalSession::from_summary(&summary));
        }
    }

    pub(crate) fn apply_history(
        &mut self,
        session_id: &str,
        result: Result<Vec<HistoryMessage>, String>,
    ) {
        let Some(session) = self
            .sessions
            .iter_mut()
            .find(|session| session.remote_id.as_deref() == Some(session_id))
        else {
            return;
        };
        match result {
            Ok(rows) => {
                session.messages = rows
                    .into_iter()
                    .filter(|row| row.role != "system")
                    .map(|row| {
                        let role = if row.role == "assistant" {
                            ChatRole::Assistant
                        } else {
                            ChatRole::User
                        };
                        let mut message = ChatMessage {
                            role: Some(role.clone()),
                            content: row.text,
                            tool_calls: row.tool_calls,
                            tool_results: row.tool_results,
                            ..ChatMessage::default()
                        };
                        if role == ChatRole::Assistant {
                            message.refresh_markdown();
                        }
                        message
                    })
                    .collect();
                session.message_count = session.messages.len() as i64;
                session.history = HistoryState::Loaded;
            }
            Err(error) => session.history = HistoryState::Failed(error),
        }
    }

    pub(crate) fn history_ready(&self) -> bool {
        self.active().is_none_or(|session| {
            session.remote_id.is_none() || matches!(session.history, HistoryState::Loaded)
        })
    }

    pub(crate) fn begin_history_load(
        &mut self,
        index: usize,
        bridge_available: bool,
        offline_error: String,
    ) -> Option<String> {
        let session = self.sessions.get_mut(index)?;
        if !matches!(
            session.history,
            HistoryState::NotLoaded | HistoryState::Failed(_)
        ) {
            return None;
        }
        let Some(remote_id) = session.remote_id.clone() else {
            session.history = HistoryState::Loaded;
            return None;
        };
        if !bridge_available {
            session.history = HistoryState::Failed(offline_error);
            return None;
        }
        session.history = HistoryState::Loading;
        Some(remote_id)
    }

    pub(crate) fn reconcile_provisional(
        &mut self,
        session_index: usize,
        session_id: &str,
        result: &Result<bool, String>,
    ) {
        let Some(session) = self.sessions.get_mut(session_index) else {
            return;
        };
        if session.provisional_remote_id.as_deref() != Some(session_id) {
            return;
        }
        match result {
            Ok(true) => {
                session.remote_id = Some(session_id.to_string());
                session.provisional_remote_id = None;
                session.history = HistoryState::Loaded;
            }
            Ok(false) => {
                session.provisional_remote_id = None;
                if session.persistent_context.is_none() {
                    session.persistent_context = build_branch_context(&session.messages);
                }
            }
            Err(_) => {}
        }
    }

    pub(crate) fn begin_stream(&mut self, prompt: String) -> StreamSession {
        let index = self.active;
        let session = self
            .active_mut()
            .expect("session state always contains an active session");
        if session.title.trim().is_empty() {
            session.title = title_from_prompt(&prompt);
        }
        let remote_id = session.remote_id.clone();
        let persistent_context = session
            .persistent_context
            .as_deref()
            .map(str::trim)
            .filter(|context| !context.is_empty())
            .map(ToOwned::to_owned);
        session.messages.push(ChatMessage::user(prompt));
        session.messages.push(ChatMessage::assistant_streaming());
        session.message_count = session.messages.len() as i64;
        StreamSession {
            index,
            remote_id,
            persistent_context,
        }
    }

    pub(crate) fn streaming_assistant_mut(
        &mut self,
        session_index: usize,
    ) -> Option<&mut ChatMessage> {
        let message = self.sessions.get_mut(session_index)?.messages.last_mut()?;
        (message.role() == ChatRole::Assistant).then_some(message)
    }

    pub(crate) fn capture_remote(&mut self, session_index: usize, id: Option<&str>) {
        let Some(id) = id.filter(|id| !id.is_empty()) else {
            return;
        };
        let Some(session) = self.sessions.get_mut(session_index) else {
            return;
        };
        session.remote_id = Some(id.to_string());
        session.provisional_remote_id = None;
        session.persistent_context = None;
        session.history = HistoryState::Loaded;
    }

    pub(crate) fn capture_provisional(&mut self, session_index: usize, id: Option<&str>) {
        let Some(id) = id.filter(|id| !id.is_empty()) else {
            return;
        };
        if let Some(session) = self.sessions.get_mut(session_index)
            && session.remote_id.is_none()
        {
            session.provisional_remote_id = Some(id.to_string());
        }
    }

    pub(crate) fn finalize_stream(
        &mut self,
        session_index: usize,
        fallback: Option<String>,
        interrupted: bool,
    ) {
        let Some(session) = self.sessions.get_mut(session_index) else {
            return;
        };
        let Some(message) = session.messages.last_mut() else {
            return;
        };
        if message.role() != ChatRole::Assistant {
            return;
        }
        if message.content.trim().is_empty() {
            if let Some(answer) = fallback.filter(|answer| !answer.trim().is_empty()) {
                message.content = answer;
            } else if interrupted {
                message.content = fl!("interrupted");
            }
        }
        if interrupted
            && !message.content.trim().is_empty()
            && message.content != fl!("interrupted")
            && !message.warnings.contains(&fl!("interrupted"))
        {
            message.warnings.push(fl!("interrupted"));
        }
        message.in_progress = false;
        message.refresh_markdown();
        if message.is_visibly_empty() {
            session.messages.pop();
        }
        session.message_count = session.messages.len() as i64;
        session.last_ts_ms = Some(now_ms());
    }

    pub(crate) fn retry_branch(
        &self,
        assistant_index: usize,
    ) -> Option<(Vec<ChatMessage>, String, String, String)> {
        let session = self.active()?;
        let user_index = session
            .messages
            .get(..assistant_index)
            .unwrap_or(&session.messages)
            .iter()
            .rposition(|message| {
                message.role() == ChatRole::User && !message.content.trim().is_empty()
            })?;
        let prompt = session.messages[user_index].content.clone();
        let prefix = session.messages[..user_index].to_vec();
        let context = build_branch_context(&prefix).unwrap_or_default();
        Some((prefix, prompt, context, session.display_title()))
    }
}

pub(crate) struct StreamSession {
    pub(crate) index: usize,
    pub(crate) remote_id: Option<String>,
    pub(crate) persistent_context: Option<String>,
}

pub(crate) fn build_branch_context(messages: &[ChatMessage]) -> Option<String> {
    let mut chunks = Vec::new();
    let mut used = 0usize;
    for message in messages.iter().rev() {
        if message.content.trim().is_empty() {
            continue;
        }
        let role = match message.role() {
            ChatRole::User => "User",
            ChatRole::Assistant => "Assistant",
        };
        let chunk = format!(
            "{role}: {}",
            clip_preview(&message.content, MAX_BRANCH_MESSAGE_CHARS)
        );
        let chars = chunk.chars().count() + 1;
        if used + chars > MAX_BRANCH_CONTEXT_CHARS {
            break;
        }
        used += chars;
        chunks.push(chunk);
    }
    if chunks.is_empty() {
        return None;
    }
    chunks.reverse();
    Some(chunks.join("\n"))
}

pub(crate) fn relative_time_label(timestamp_ms: i64, current_ms: i64) -> String {
    let seconds = current_ms.saturating_sub(timestamp_ms).max(0) / 1_000;
    if seconds < 60 {
        fl!("just-now")
    } else if seconds < 3_600 {
        let count: i64 = seconds / 60;
        fl!("minutes-ago", count = count)
    } else if seconds < 86_400 {
        let count: i64 = seconds / 3_600;
        fl!("hours-ago", count = count)
    } else {
        let count: i64 = seconds / 86_400;
        fl!("days-ago", count = count)
    }
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(i64::MAX as u128) as i64
}

fn clip_preview(input: &str, max_chars: usize) -> String {
    let trimmed = input.trim();
    if trimmed.chars().count() <= max_chars {
        return trimmed.to_string();
    }
    let mut output = trimmed.chars().take(max_chars).collect::<String>();
    output.push_str(" …");
    output
}

fn title_from_prompt(prompt: &str) -> String {
    let first_line = prompt.lines().next().unwrap_or_default().trim();
    let mut title = first_line.chars().take(40).collect::<String>();
    if first_line.chars().count() > 40 {
        title.push('…');
    }
    if title.is_empty() {
        fl!("new-session")
    } else {
        title
    }
}

#[cfg(test)]
mod tests {
    include!(concat!(env!("CARGO_MANIFEST_DIR"), "/test/unit/session.rs"));
}
