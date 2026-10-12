use crate::{Message as AppMessage, bridge, fl};
use cos_agent_protocol::{
    BridgeEndpoint, MemoryEntry, MemoryForgetRequest, MemoryInspectRequest, MemoryInventory,
    MemoryKind, MemoryReadRequest, MemoryReference, MemoryUpdateRequest,
};
use cosmic::iced::widget::text_editor;
use cosmic::{
    Element,
    app::Task,
    widget::{self, Column, Row, button, scrollable, text},
};

#[derive(Clone, Debug)]
pub enum Message {
    Show,
    Close,
    Refresh,
    Kind(MemoryKind),
    Query(String),
    Open(MemoryReference),
    Listed(u64, Result<MemoryInventory, String>),
    Read(u64, Result<MemoryEntry, String>),
    New,
    Correction,
    Edit,
    Name(String),
    Content(text_editor::Action),
    Save,
    Saved(u64, Result<MemoryEntry, String>),
    AskForget,
    Keep,
    Forget,
    Forgotten(u64, Result<serde_json::Value, String>),
}

struct Editor {
    name: String,
    revision: String,
    content: text_editor::Content,
}
#[derive(Default)]
pub struct MemoryCenter {
    visible: bool,
    generation: u64,
    kind: MemoryKind,
    query: String,
    inventory: MemoryInventory,
    selected: Option<MemoryEntry>,
    editor: Option<Editor>,
    confirm: bool,
    busy: bool,
    error: Option<String>,
}

fn task<F, R>(
    future: F,
    map: impl Fn(Result<R, String>) -> Message + Send + 'static,
) -> Task<AppMessage>
where
    F: std::future::Future<Output = anyhow::Result<R>> + Send + 'static,
    R: Send + 'static,
{
    Task::perform(
        async move { future.await.map_err(|error| format!("{error:#}")) },
        move |result| cosmic::Action::App(AppMessage::Memory(map(result))),
    )
}

impl MemoryCenter {
    pub fn is_visible(&self) -> bool {
        self.visible
    }
    pub fn hide(&mut self) {
        self.visible = false;
        self.generation = self.generation.wrapping_add(1);
        self.busy = false;
    }
    pub fn update(
        &mut self,
        message: Message,
        endpoint: Option<BridgeEndpoint>,
    ) -> Task<AppMessage> {
        match message {
            Message::Close => {
                self.hide();
                return Task::none();
            }
            Message::Show => {
                self.visible = true;
                return self.update(Message::Refresh, endpoint);
            }
            Message::Kind(kind) if !self.busy => {
                self.kind = kind;
                self.query.clear();
                return self.update(Message::Refresh, endpoint);
            }
            Message::Query(query) if !self.busy => {
                self.query = query;
                return Task::none();
            }
            Message::Listed(generation, result) if generation == self.generation => {
                self.busy = false;
                match result {
                    Ok(inventory) => {
                        self.inventory = inventory;
                        self.error = None;
                    }
                    Err(error) => {
                        self.inventory = MemoryInventory::default();
                        self.error = Some(error);
                    }
                }
                return Task::none();
            }
            Message::Read(generation, result) | Message::Saved(generation, result)
                if generation == self.generation =>
            {
                self.busy = false;
                match result {
                    Ok(entry) => {
                        self.selected = Some(entry);
                        self.editor = None;
                        self.error = None;
                    }
                    Err(error) => self.error = Some(error),
                }
                return Task::none();
            }
            Message::Forgotten(generation, result) if generation == self.generation => {
                self.busy = false;
                match result {
                    Ok(value)
                        if value["forgotten"] == true
                            && value["conversations_preserved"] == true =>
                    {
                        return self.update(Message::Refresh, endpoint);
                    }
                    Ok(_) => self.error = Some(fl!("memory-unconfirmed")),
                    Err(error) => self.error = Some(error),
                }
                return Task::none();
            }
            Message::New if !self.busy => {
                self.editor = Some(Editor {
                    name: "NEW-NOTE.md".into(),
                    revision: "new".into(),
                    content: text_editor::Content::new(),
                });
                self.error = None;
                return Task::none();
            }
            Message::Edit if !self.busy => {
                if let Some(entry) = &self.selected {
                    if let MemoryReference::Note { name } = &entry.reference {
                        if entry.editable {
                            self.editor = Some(Editor {
                                name: name.clone(),
                                revision: entry.revision.clone(),
                                content: text_editor::Content::with_text(&entry.content),
                            });
                        }
                    }
                }
                return Task::none();
            }
            Message::Correction if !self.busy => {
                if let Some(entry) = &self.selected {
                    let stamp = match std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) {
                        Ok(duration) => duration.as_millis(),
                        Err(error) => { self.error=Some(error.to_string()); return Task::none(); }
                    };
                    self.editor = Some(Editor {
                        name: format!("CORRECTION-{stamp}.md"),
                        revision: "new".into(),
                        content: text_editor::Content::with_text(&format!(
                            "User correction for {}\nOriginal revision: {}\n\n",
                            entry.source, entry.revision
                        )),
                    });
                }
                return Task::none();
            }
            Message::Name(name) if !self.busy => {
                if let Some(editor) = &mut self.editor {
                    if editor.revision == "new" {
                        editor.name = name;
                    }
                }
                return Task::none();
            }
            Message::Content(action) if !self.busy => {
                if let Some(editor) = &mut self.editor {
                    editor.content.perform(action);
                }
                return Task::none();
            }
            Message::AskForget if !self.busy => {
                self.confirm = true;
                return Task::none();
            }
            Message::Keep => {
                self.confirm = false;
                return Task::none();
            }
            Message::Refresh | Message::Open(_) | Message::Save | Message::Forget if !self.busy => {
            }
            _ => return Task::none(),
        }
        let Some(endpoint) = endpoint else {
            self.error = Some(fl!("bridge-offline"));
            return Task::none();
        };
        self.generation = self.generation.wrapping_add(1);
        let generation = self.generation;
        match message {
            Message::Refresh => {
                self.selected = None;
                self.editor = None;
                self.confirm = false;
                if self.kind == MemoryKind::Conversations && self.query.trim().is_empty() {
                    self.inventory = MemoryInventory {
                        coverage: fl!("memory-search-interactions"),
                        ..Default::default()
                    };
                    return Task::none();
                }
                self.busy = true;
                task(
                    bridge::memory_request(
                        endpoint,
                        "inspect",
                        MemoryInspectRequest {
                            kind: self.kind,
                            query: self.query.clone(),
                            limit: 50,
                        },
                    ),
                    move |result| Message::Listed(generation, result),
                )
            }
            Message::Open(reference) => {
                self.busy = true;
                self.editor = None;
                self.confirm = false;
                task(
                    bridge::memory_request(endpoint, "read", MemoryReadRequest { reference }),
                    move |result| Message::Read(generation, result),
                )
            }
            Message::Save => {
                let Some(editor) = &self.editor else {
                    return Task::none();
                };
                self.busy = true;
                task(
                    bridge::memory_request(
                        endpoint,
                        "update",
                        MemoryUpdateRequest {
                            name: editor.name.clone(),
                            revision: editor.revision.clone(),
                            content: editor.content.text(),
                        },
                    ),
                    move |result| Message::Saved(generation, result),
                )
            }
            Message::Forget => {
                let Some(entry) = &self.selected else {
                    return Task::none();
                };
                if !self.confirm || !entry.deletable {
                    return Task::none();
                }
                self.busy = true;
                task(
                    bridge::memory_request(
                        endpoint,
                        "forget",
                        MemoryForgetRequest {
                            reference: entry.reference.clone(),
                            revision: entry.revision.clone(),
                            confirm: true,
                        },
                    ),
                    move |result| Message::Forgotten(generation, result),
                )
            }
            _ => Task::none(),
        }
    }
    pub fn view(&self) -> Element<'_, AppMessage> {
        let control = |label: String, message: Message| {
            let mut button = button::text(label);
            if !self.busy {
                button = button.on_press(AppMessage::Memory(message));
            }
            button
        };
        let mut content = Column::new()
            .spacing(12)
            .push(
                Row::new()
                    .spacing(8)
                    .push(text(fl!("memory-center")).size(20.0))
                    .push(control(fl!("memory-refresh"), Message::Refresh))
                    .push(control(fl!("memory-close"), Message::Close)),
            )
            .push(text(fl!("memory-caveat")).size(12.0));
        let mut tabs = Row::new().spacing(8);
        for (kind, label) in [
            (MemoryKind::Notes, fl!("memory-notes")),
            (MemoryKind::Apps, fl!("memory-apps")),
            (MemoryKind::Conversations, fl!("memory-interactions")),
            (MemoryKind::Semantic, fl!("memory-semantic")),
        ] {
            tabs = tabs.push(control(label, Message::Kind(kind)));
        }
        content = content
            .push(tabs)
            .push(
                widget::text_input(fl!("memory-search"), &self.query)
                    .on_input(|value| AppMessage::Memory(Message::Query(value))),
            )
            .push(control(fl!("memory-search"), Message::Refresh))
            .push(text(&self.inventory.coverage).size(11.0));
        if let Some(error) = &self.error {
            content = content.push(text(error).size(12.0));
        }
        if self.inventory.has_more {
            content = content.push(text(fl!("memory-more")).size(11.0));
        }
        for entry in &self.inventory.entries {
            content = content.push(control(
                format!("{}: {}", entry.source, entry.content),
                Message::Open(entry.reference.clone()),
            ));
        }
        if self.kind == MemoryKind::Notes {
            content = content.push(control(fl!("memory-new"), Message::New));
        }
        if let Some(entry) = &self.selected {
            content = content
                .push(text(&entry.source).size(16.0))
                .push(text(&entry.provenance).size(11.0))
                .push(
                    text(format!(
                        "{}: {}",
                        fl!("memory-record-time"),
                        entry
                            .recorded_at_ms
                            .map(|time| time.to_string())
                            .unwrap_or_else(|| fl!("memory-time-unknown"))
                    ))
                    .size(10.0),
                )
                .push(text(format!("{}: {}", fl!("memory-revision"), entry.revision)).size(10.0))
                .push(text(&entry.content).size(12.0));
            if entry.editable {
                content = content.push(control(fl!("memory-edit"), Message::Edit));
            } else {
                content = content.push(control(fl!("memory-correction"), Message::Correction));
            }
            if entry.deletable {
                content = content.push(control(fl!("memory-forget"), Message::AskForget));
            }
            if self.confirm {
                content = content
                    .push(text(fl!("memory-forget-caveat")).size(11.0))
                    .push(control(fl!("memory-confirm-forget"), Message::Forget))
                    .push(control(fl!("memory-keep"), Message::Keep));
            }
        }
        if let Some(editor) = &self.editor {
            content = content
                .push(
                    widget::text_input(fl!("memory-name"), &editor.name)
                        .on_input(|value| AppMessage::Memory(Message::Name(value))),
                )
                .push(
                    widget::text_editor(&editor.content)
                        .on_action(|action| AppMessage::Memory(Message::Content(action))),
                )
                .push(control(fl!("memory-save"), Message::Save));
        }
        scrollable(content).into()
    }
}

#[cfg(test)]
mod tests {
    include!(concat!(env!("CARGO_MANIFEST_DIR"), "/test/unit/memory_center.rs"));
}
