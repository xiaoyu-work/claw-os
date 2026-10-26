use crate::{Message as AppMessage, bridge, fl};
use cos_agent_protocol::{BridgeEndpoint, nearby::*};
use cosmic::{
    Element,
    app::Task,
    widget::{self, Column, Row, button, scrollable, text},
};

#[derive(Debug, Clone)]
pub enum Message {
    Show,
    Close,
    Refresh,
    Poll,
    PickFile,
    ClearFiles,
    ToggleDisruption,
    Seconds(String),
    Peer(String),
    Open(bool),
    Send,
    Action(NearbyAction),
    FileChosen(u64, Result<Option<String>, String>),
    Fetched(u64, Result<Box<NearbyView>, String>),
}

pub struct Nearby {
    visible: bool,
    generation: u64,
    busy: bool,
    error: Option<String>,
    view: Option<NearbyView>,
    seconds: String,
    disruption: bool,
    files: Vec<String>,
    peer: Option<String>,
}
impl Default for Nearby {
    fn default() -> Self {
        Self {
            visible: false,
            generation: 0,
            busy: false,
            error: None,
            view: None,
            seconds: "300".into(),
            disruption: false,
            files: Vec::new(),
            peer: None,
        }
    }
}
impl Nearby {
    pub fn show(&mut self) {
        self.visible = true;
    }
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
            Message::Fetched(generation, result) if generation == self.generation => {
                self.busy = false;
                match result {
                    Ok(view) => {
                        if self
                            .view
                            .as_ref()
                            .is_none_or(|old| old.probe.mode != view.probe.mode)
                            || view.session.as_ref().is_some_and(|session| {
                                matches!(
                                    session.phase,
                                    SessionPhase::Ended
                                        | SessionPhase::Failed
                                        | SessionPhase::Interrupted
                                )
                            })
                        {
                            self.disruption = false;
                        }
                        if self
                            .view
                            .as_ref()
                            .and_then(|old| old.session.as_ref())
                            .map(|s| &s.id)
                            != view.session.as_ref().map(|s| &s.id)
                        {
                            self.peer = None;
                        }
                        self.view = Some(*view);
                        self.error = None;
                    }
                    Err(error) => self.error = Some(error),
                }
                return Task::none();
            }
            Message::FileChosen(generation, result) if generation == self.generation => {
                self.busy = false;
                match result {
                    Ok(Some(path))
                        if self.files.len() < MAX_FILES && !self.files.contains(&path) =>
                    {
                        self.files.push(path)
                    }
                    Ok(_) => {}
                    Err(error) => self.error = Some(error),
                }
                return Task::none();
            }
            Message::Seconds(value) if !self.busy => {
                self.seconds = value;
                return Task::none();
            }
            Message::ToggleDisruption if !self.busy => {
                self.disruption = !self.disruption;
                return Task::none();
            }
            Message::ClearFiles if !self.busy => {
                self.files.clear();
                return Task::none();
            }
            Message::Peer(id) if !self.busy => {
                self.peer = Some(id);
                return Task::none();
            }
            Message::Poll if !self.visible || self.busy || self.error.is_some() => {
                return Task::none();
            }
            Message::PickFile if !self.busy => {
                if self.files.len() >= MAX_FILES {
                    self.error = Some(fl!("nearby-limits"));
                    return Task::none();
                }
                self.busy = true;
                self.generation = self.generation.wrapping_add(1);
                let generation = self.generation;
                return Task::perform(
                    async {
                        match cosmic::dialog::file_chooser::open::Dialog::new()
                            .title(fl!("nearby-pick"))
                            .open_file()
                            .await
                        {
                            Ok(response) => response
                                .url()
                                .to_file_path()
                                .map_err(|_| fl!("nearby-file-error"))?
                                .into_os_string()
                                .into_string()
                                .map(Some)
                                .map_err(|_| fl!("nearby-file-error")),
                            Err(cosmic::dialog::file_chooser::Error::Cancelled) => Ok(None),
                            Err(error) => Err(error.to_string()),
                        }
                    },
                    move |result| {
                        cosmic::Action::App(AppMessage::Nearby(Message::FileChosen(
                            generation, result,
                        )))
                    },
                );
            }
            Message::Refresh
            | Message::Poll
            | Message::Open(_)
            | Message::Send
            | Message::Action(_)
                if !self.busy => {}
            _ => return Task::none(),
        }
        let action = match message {
            Message::Refresh | Message::Poll => None,
            Message::Open(receive) => {
                let Some(view) = &self.view else {
                    return Task::none();
                };
                let Some(mode) = view.probe.mode.filter(|_| view.probe.available) else {
                    self.error = Some(fl!("nearby-unavailable"));
                    return Task::none();
                };
                if mode.disrupts_wifi() && !self.disruption {
                    self.error = Some(fl!("nearby-disruption"));
                    return Task::none();
                }
                let seconds = match self.seconds.parse::<u32>() {
                    Ok(seconds) => seconds,
                    Err(_) => {
                        self.error = Some(fl!("nearby-duration"));
                        return Task::none();
                    }
                };
                Some(NearbyAction::Open {
                    duration_secs: seconds,
                    receive,
                    expected_mode: mode,
                    allow_disruption: self.disruption,
                })
            }
            Message::Send => {
                let Some(session) = self.view.as_ref().and_then(|view| view.session.as_ref())
                else {
                    return Task::none();
                };
                let Some(peer) = &self.peer else {
                    return Task::none();
                };
                Some(NearbyAction::Send {
                    session_id: session.id.clone(),
                    peer_id: peer.clone(),
                    paths: self.files.clone(),
                    uploads: Vec::new(),
                })
            }
            Message::Action(action) => Some(action),
            _ => return Task::none(),
        };
        if let Some(action) = &action
            && let Err(error) = action.validate()
        {
            self.error = Some(error);
            return Task::none();
        }
        let Some(endpoint) = endpoint else {
            self.error = Some(fl!("bridge-offline"));
            return Task::none();
        };
        self.busy = true;
        self.error = None;
        self.generation = self.generation.wrapping_add(1);
        let generation = self.generation;
        Task::perform(
            async move {
                bridge::nearby_request(endpoint, action)
                    .await
                    .map(Box::new)
                    .map_err(|error| format!("{error:#}"))
            },
            move |result| {
                cosmic::Action::App(AppMessage::Nearby(Message::Fetched(generation, result)))
            },
        )
    }

    pub fn view(&self) -> Element<'_, AppMessage> {
        let control = |label: String, message: Message, enabled: bool| {
            let button = button::text(label);
            if enabled && !self.busy {
                button.on_press(AppMessage::Nearby(message))
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
                    .push(text(fl!("nearby-title")).size(22.0))
                    .push(control(fl!("nearby-refresh"), Message::Refresh, true))
                    .push(
                        button::text(fl!("nearby-close"))
                            .on_press(AppMessage::Nearby(Message::Close)),
                    ),
            )
            .push(text(fl!("nearby-description")).size(13.0))
            .push(text(fl!("nearby-caveat")).size(12.0));
        if let Some(error) = &self.error {
            content = content
                .push(text(error))
                .push(text(fl!("nearby-uncertain")).size(12.0));
        }
        if let Some(view) = &self.view {
            content = content
                .push(text(if view.probe.available {
                    fl!("nearby-available")
                } else {
                    fl!("nearby-unavailable")
                }))
                .push(text(format!(
                    "{:?} · {}",
                    view.probe.mode,
                    view.probe.driver.as_deref().unwrap_or("-")
                )));
            for diagnostic in &view.probe.diagnostics {
                content = content.push(text(diagnostic).size(12.0));
            }
            if view.probe.mode.is_some_and(RadioMode::disrupts_wifi) {
                content = content.push(text(fl!("nearby-disruption")).size(13.0));
            }
            let active = view.session.as_ref().is_some_and(|session| {
                matches!(
                    session.phase,
                    SessionPhase::Starting | SessionPhase::Ready | SessionPhase::Stopping
                )
            });
            content = content
                .push(control(
                    if self.disruption {
                        fl!("nearby-disruption-yes")
                    } else {
                        fl!("nearby-disruption-no")
                    },
                    Message::ToggleDisruption,
                    !active,
                ))
                .push(
                    widget::text_input(fl!("nearby-duration"), &self.seconds)
                        .on_input(|value| AppMessage::Nearby(Message::Seconds(value))),
                );
            let can_open = usable
                && view.probe.available
                && !active
                && view
                    .probe
                    .mode
                    .is_some_and(|mode| !mode.disrupts_wifi() || self.disruption);
            content = content
                .push(
                    Row::new()
                        .spacing(8)
                        .push(control(fl!("nearby-open"), Message::Open(false), can_open))
                        .push(control(
                            fl!("nearby-receive"),
                            Message::Open(true),
                            can_open,
                        )),
                )
                .push(text(fl!("nearby-files")).size(18.0))
                .push(text(fl!("nearby-limits")).size(12.0))
                .push(
                    Row::new()
                        .spacing(8)
                        .push(control(fl!("nearby-pick"), Message::PickFile, true))
                        .push(control(fl!("nearby-clear"), Message::ClearFiles, true)),
                );
            for file in &self.files {
                content = content.push(text(file).size(12.0));
            }
            if let Some(session) = &view.session {
                content = content.push(text(format!(
                    "{:?} · {}",
                    session.phase, session.expires_at
                )));
                if let Some(message) = &session.message {
                    content = content.push(text(message).size(12.0));
                }
                content = content
                    .push(control(
                        fl!("nearby-stop"),
                        Message::Action(NearbyAction::Stop {
                            session_id: session.id.clone(),
                        }),
                        active && session.phase != SessionPhase::Stopping,
                    ))
                    .push(text(fl!("nearby-peers")).size(18.0))
                    .push(control(
                        fl!("nearby-discover"),
                        Message::Action(NearbyAction::Discover {
                            session_id: session.id.clone(),
                        }),
                        usable && session.phase == SessionPhase::Ready && !session.discovering,
                    ));
                for peer in &session.peers {
                    content = content.push(control(
                        format!(
                            "{} {} · {}",
                            if self.peer.as_ref() == Some(&peer.id) {
                                ">"
                            } else {
                                ""
                            },
                            peer.name,
                            peer.id
                        ),
                        Message::Peer(peer.id.clone()),
                        usable,
                    ));
                }
                content = content
                    .push(control(
                        fl!("nearby-send"),
                        Message::Send,
                        usable
                            && session.phase == SessionPhase::Ready
                            && !self.files.is_empty()
                            && self
                                .peer
                                .as_ref()
                                .is_some_and(|id| session.peers.iter().any(|peer| &peer.id == id)),
                    ))
                    .push(text(fl!("nearby-offers")).size(18.0));
                for offer in &session.offers {
                    content = content.push(text(format!(
                        "{} · {:?} · {}",
                        offer.sender, offer.phase, offer.expires_at
                    )));
                    for file in &offer.files {
                        content = content.push(text(&file.name).size(12.0));
                    }
                    let pending = offer.phase == TransferPhase::Offered
                        && session.phase == SessionPhase::Ready;
                    content = content.push(
                        Row::new()
                            .spacing(8)
                            .push(control(
                                fl!("nearby-accept"),
                                Message::Action(NearbyAction::Decide {
                                    session_id: session.id.clone(),
                                    offer_id: offer.id.clone(),
                                    accept: true,
                                }),
                                usable && pending,
                            ))
                            .push(control(
                                fl!("nearby-decline"),
                                Message::Action(NearbyAction::Decide {
                                    session_id: session.id.clone(),
                                    offer_id: offer.id.clone(),
                                    accept: false,
                                }),
                                pending,
                            )),
                    );
                }
                content = content.push(text(fl!("nearby-transfers")).size(18.0));
                for transfer in &session.transfers {
                    content = content.push(text(format!(
                        "{} · {:?} · {} / {}",
                        transfer.peer,
                        transfer.phase,
                        transfer.bytes,
                        transfer
                            .total_bytes
                            .map(|bytes| bytes.to_string())
                            .unwrap_or_else(|| fl!("nearby-progress-unknown"))
                    )));
                    for file in &transfer.files {
                        content = content.push(text(&file.name).size(12.0));
                    }
                    for path in &transfer.received_paths {
                        content = content.push(text(path).size(12.0));
                    }
                    if let Some(message) = &transfer.message {
                        content = content.push(text(message).size(12.0));
                    }
                    if !transfer.incoming && !transfer.phase.terminal() {
                        content = content.push(control(
                            fl!("nearby-cancel"),
                            Message::Action(NearbyAction::Cancel {
                                session_id: session.id.clone(),
                                transfer_id: transfer.id.clone(),
                            }),
                            session.phase == SessionPhase::Ready,
                        ));
                    }
                }
            }
            content = content
                .push(text(fl!("nearby-inbox")).size(14.0))
                .push(text(&view.inbox).size(12.0));
        }
        scrollable(widget::container(content).padding(20)).into()
    }
}

#[cfg(test)]
mod tests {
    include!(concat!(env!("CARGO_MANIFEST_DIR"), "/test/unit/nearby.rs"));
}
