//! OS panel descriptor transport. The managed App owns rendering and preferences.

fn main() {
    if let Err(refusal) =
        yu::update::runtime::enforce_startup(yu::update::runtime::Scope::CompiledEpoch)
    {
        eprintln!("claw-notification-panel: {}", refusal.message);
        std::process::exit(1);
    }
    #[cfg(target_os = "linux")]
    let result = panel::run();
    #[cfg(not(target_os = "linux"))]
    let result: Result<(), &str> = Err("notification presentation requires Linux");
    if let Err(error) = result {
        eprintln!("claw-notification-panel: {error}");
        std::process::exit(1);
    }
}

#[cfg(target_os = "linux")]
mod panel {
    use std::collections::{BTreeMap, HashSet};
    use std::os::fd::{AsFd, FromRawFd, OwnedFd};
    use std::sync::Arc;
    use std::time::Duration;

    use claw_notification_presentation::{Card, Preferences, PRESENTER_FD_ENV};
    use serde::Deserialize;
    use serde_json::{json, Value};
    use tokio::sync::{Mutex, OwnedSemaphorePermit, Semaphore};
    use zbus::{connection::Builder, interface, object_server::SignalEmitter, Connection, Guid};

    const CALL_TIMEOUT: Duration = Duration::from_secs(3);
    const SOCKET_PATH: &str = "/com/yuos/NotificationsSocket";
    const CLOSED_PATH: &str = "/org/freedesktop/Notifications";

    #[derive(Deserialize)]
    struct Snapshot {
        generation: String,
        revision: u64,
        unchanged: bool,
        preferences: Preferences,
        cards: Vec<String>,
        more: bool,
    }

    #[derive(Default)]
    struct View {
        generation: Option<String>,
        revision: Option<u64>,
        cards: BTreeMap<u32, String>,
        preferences: Option<Preferences>,
        roots: BTreeMap<u32, u32>,
        next_handle: u32,
    }

    async fn broker(params: Value) -> Result<Value, String> {
        let request = yu::clawd::protocol::Request::new(
            yu::clawd::routes::Command::parse("notification.presentation.control")
                .ok_or("Root notification panel route is unavailable")?,
            params,
        );
        let response = tokio::time::timeout(
            CALL_TIMEOUT,
            yu::clawd::client::request(yu::clawd::config::socket_path(), request),
        )
        .await
        .map_err(|_| "notification panel broker timed out")?
        .map_err(|error| error.to_string())?;
        if response.ok {
            response
                .result
                .ok_or_else(|| "notification panel reply is missing".into())
        } else {
            Err(response
                .error
                .map(|error| error.message)
                .unwrap_or_else(|| "notification presenter is unavailable".into()))
        }
    }

    fn error(error: impl ToString) -> zbus::fdo::Error {
        zbus::fdo::Error::Failed(error.to_string())
    }

    fn peer_closed(error: &zbus::Error) -> bool {
        // zbus reports an authenticated Unix peer's EOF as BrokenPipe.
        matches!(error, zbus::Error::InputOutput(error) if error.kind() == std::io::ErrorKind::BrokenPipe)
    }

    #[derive(Clone)]
    struct Presentation {
        view: Arc<Mutex<View>>,
    }

    impl Presentation {
        async fn generation(&self) -> zbus::fdo::Result<String> {
            self.view
                .lock()
                .await
                .generation
                .clone()
                .ok_or_else(|| error("no authorized native notification presenter is ready"))
        }

        async fn action(&self, id: u32, action: Option<&str>) -> zbus::fdo::Result<()> {
            claw_notification_presentation::validate_handle(id).map_err(error)?;
            let generation = {
                let view = self.view.lock().await;
                let card = view
                    .cards
                    .get(&id)
                    .ok_or_else(|| error("notification presentation handle is stale"))?;
                let card = Card::from_json(card).map_err(error)?;
                if action.is_some_and(|action| card.activation.as_deref() != Some(action)) {
                    return Err(zbus::fdo::Error::InvalidArgs(
                        "notification action is not declared".into(),
                    ));
                }
                view.generation
                    .clone()
                    .ok_or_else(|| error("native notification presenter is unavailable"))?
            };
            let request = if let Some(action) = action {
                json!({"action":"activate","generation":generation,"handle":self.root_handle(id).await?,"action_id":action})
            } else {
                json!({"action":"dismiss","generation":generation,"handle":self.root_handle(id).await?})
            };
            broker(request).await.map_err(error)?;
            Ok(())
        }

        async fn root_handle(&self, id: u32) -> zbus::fdo::Result<u32> {
            self.view
                .lock()
                .await
                .roots
                .get(&id)
                .copied()
                .ok_or_else(|| error("notification presentation handle is stale"))
        }
    }

    #[interface(name = "com.yuos.NotificationPresentation1")]
    impl Presentation {
        fn version(&self) -> u32 {
            claw_notification_presentation::VERSION
        }

        async fn preferences(&self) -> zbus::fdo::Result<String> {
            let preferences = self
                .view
                .lock()
                .await
                .preferences
                .clone()
                .ok_or_else(|| error("native notification preferences are not ready"))?;
            preferences.to_json().map_err(error)
        }

        async fn set_preferences(&self, payload: &str) -> zbus::fdo::Result<String> {
            let preferences = Preferences::from_json(payload)
                .map_err(|failure| zbus::fdo::Error::InvalidArgs(failure.to_string()))?;
            let value = broker(
                json!({"action":"set_preferences","generation":self.generation().await?,
                "preferences":preferences}),
            )
            .await
            .map_err(error)?;
            let preferences: Preferences =
                serde_json::from_value(value["preferences"].clone()).map_err(error)?;
            preferences.to_json().map_err(error)
        }

        async fn dismiss(&self, id: u32) -> zbus::fdo::Result<()> {
            self.action(id, None).await
        }

        async fn invoke_action(&self, id: u32, action: &str) -> zbus::fdo::Result<()> {
            claw_notification_presentation::validate_action(id, action).map_err(error)?;
            self.action(id, Some(action)).await
        }

        #[zbus(signal)]
        async fn card(emitter: &SignalEmitter<'_>, payload: &str) -> zbus::Result<()>;
        #[zbus(signal)]
        async fn preferences_changed(
            emitter: &SignalEmitter<'_>,
            payload: &str,
        ) -> zbus::Result<()>;
        #[zbus(signal)]
        async fn failure(emitter: &SignalEmitter<'_>, message: &str) -> zbus::Result<()>;
    }

    struct Closed;
    #[interface(name = "org.freedesktop.Notifications")]
    impl Closed {
        #[zbus(signal)]
        async fn notification_closed(
            emitter: &SignalEmitter<'_>,
            id: u32,
            reason: u32,
        ) -> zbus::Result<()>;
    }

    struct Socket {
        slots: Arc<Semaphore>,
        stop: tokio::sync::watch::Sender<bool>,
    }

    #[interface(name = "com.yuos.NotificationsSocket")]
    impl Socket {
        async fn get_fd(&self) -> zbus::fdo::Result<zbus::zvariant::OwnedFd> {
            let permit = self.slots.clone().try_acquire_owned().map_err(|_| {
                zbus::fdo::Error::LimitsExceeded("too many notification panel descriptors".into())
            })?;
            let (mine, theirs) = std::os::unix::net::UnixStream::pair().map_err(error)?;
            mine.set_nonblocking(true).map_err(error)?;
            let stream = tokio::net::UnixStream::from_std(mine).map_err(error)?;
            let stop = self.stop.subscribe();
            tokio::spawn(async move {
                if let Err(failure) = serve(stream, stop, permit).await {
                    tracing::warn!(%failure, "notification panel descriptor closed");
                }
            });
            Ok(zbus::zvariant::OwnedFd::from(OwnedFd::from(theirs)))
        }
    }

    async fn snapshot(view: &View) -> Result<Option<View>, String> {
        let mut after = 0;
        let mut next = View::default();
        loop {
            let mut request = json!({"action":"snapshot","after":after});
            if after == 0 {
                if let (Some(generation), Some(revision)) = (&view.generation, view.revision) {
                    request["generation"] = json!(generation);
                    request["if_revision"] = json!(revision);
                }
            }
            let page: Snapshot = serde_json::from_value(broker(request).await?)
                .map_err(|error| error.to_string())?;
            page.preferences
                .validate()
                .map_err(|error| error.to_string())?;
            if page.unchanged {
                return Ok(None);
            }
            if let Some(generation) = &next.generation {
                if generation != &page.generation {
                    return Err(
                        "native notification presentation changed during the bounded snapshot"
                            .into(),
                    );
                }
            } else {
                next.generation = Some(page.generation);
                next.revision = Some(page.revision);
                next.preferences = Some(page.preferences);
            }
            for payload in page.cards {
                let card = Card::from_json(&payload).map_err(|error| error.to_string())?;
                if card.id <= after {
                    return Err("invalid bounded native presentation page".into());
                }
                if next.cards.len() == 200 {
                    return Ok(Some(next));
                }
                after = card.id;
                next.cards.insert(card.id, payload);
            }
            if !page.more {
                return Ok(Some(next));
            }
        }
    }

    fn project_handles(current: &View, mut next: View) -> Result<View, String> {
        let reset = current.generation != next.generation;
        let mut next_handle = current.next_handle.max(1);
        let mut cards = BTreeMap::new();
        let mut roots = BTreeMap::new();
        for (root, payload) in next.cards {
            let handle = if reset {
                None
            } else {
                current
                    .roots
                    .iter()
                    .find_map(|(handle, held)| (*held == root).then_some(*handle))
            };
            let handle = match handle {
                Some(handle) => handle,
                None => {
                    let handle = next_handle;
                    next_handle = next_handle
                        .checked_add(1)
                        .ok_or("notification descriptor handles exhausted")?;
                    handle
                }
            };
            let mut card = Card::from_json(&payload).map_err(|error| error.to_string())?;
            card.id = handle;
            cards.insert(handle, card.to_json().map_err(|error| error.to_string())?);
            roots.insert(handle, root);
        }
        next.cards = cards;
        next.roots = roots;
        next.next_handle = next_handle;
        Ok(next)
    }

    async fn publish(
        connection: &Connection,
        view: &Arc<Mutex<View>>,
        next: View,
    ) -> Result<(), String> {
        let presentation = connection
            .object_server()
            .interface::<_, Presentation>(claw_notification_presentation::OBJECT_PATH)
            .await
            .map_err(|error| error.to_string())?;
        let closed = connection
            .object_server()
            .interface::<_, Closed>(CLOSED_PATH)
            .await
            .map_err(|error| error.to_string())?;
        let mut current = view.lock().await;
        let next = project_handles(&current, next)?;
        let reset = current.generation != next.generation;
        for id in current
            .cards
            .keys()
            .filter(|id| reset || !next.cards.contains_key(id))
        {
            Closed::notification_closed(closed.signal_emitter(), *id, 3)
                .await
                .map_err(|error| error.to_string())?;
        }
        for (id, payload) in &next.cards {
            if reset || current.cards.get(id) != Some(payload) {
                Presentation::card(presentation.signal_emitter(), payload)
                    .await
                    .map_err(|error| error.to_string())?;
            }
        }
        if let Some(preferences) = &next.preferences {
            if current.preferences.as_ref() != Some(preferences) {
                Presentation::preferences_changed(
                    presentation.signal_emitter(),
                    &preferences.to_json().map_err(|error| error.to_string())?,
                )
                .await
                .map_err(|error| error.to_string())?;
            }
        }
        *current = next;
        Ok(())
    }

    async fn serve(
        stream: tokio::net::UnixStream,
        mut stop: tokio::sync::watch::Receiver<bool>,
        _permit: OwnedSemaphorePermit,
    ) -> Result<(), String> {
        let view = Arc::new(Mutex::new(View::default()));
        let connection = tokio::time::timeout(
            CALL_TIMEOUT,
            Builder::socket(stream)
                .p2p()
                .server(Guid::generate())
                .map_err(|error| error.to_string())?
                .serve_at(
                    claw_notification_presentation::OBJECT_PATH,
                    Presentation { view: view.clone() },
                )
                .map_err(|error| error.to_string())?
                .serve_at(CLOSED_PATH, Closed)
                .map_err(|error| error.to_string())?
                .build(),
        )
        .await
        .map_err(|_| "notification panel handshake timed out")?
        .map_err(|error| error.to_string())?;
        let mut tick = tokio::time::interval(Duration::from_millis(250));
        let mut failures = HashSet::new();
        let mut messages = zbus::MessageStream::from(&connection);
        use futures_util::StreamExt;
        loop {
            tokio::select! {
                _ = stop.changed() => return Ok(()),
                message = messages.next() => {
                    match message {
                        Some(Ok(_)) => continue,
                        Some(Err(error)) if peer_closed(&error) => return Ok(()),
                        Some(Err(error)) => return Err(error.to_string()),
                        None => return Ok(()),
                    }
                },
                _ = tick.tick() => {}
            }
            let cursor = {
                let current = view.lock().await;
                View {
                    generation: current.generation.clone(),
                    revision: current.revision,
                    ..View::default()
                }
            };
            let next = snapshot(&cursor).await;
            match next {
                Ok(Some(next)) => {
                    publish(&connection, &view, next).await?;
                    failures.clear();
                }
                Ok(None) => {}
                Err(failure) => {
                    publish(&connection, &view, View::default()).await?;
                    if failures.insert(failure.clone()) {
                        let interface = connection
                            .object_server()
                            .interface::<_, Presentation>(
                                claw_notification_presentation::OBJECT_PATH,
                            )
                            .await
                            .map_err(|error| error.to_string())?;
                        Presentation::failure(interface.signal_emitter(), &failure)
                            .await
                            .map_err(|error| error.to_string())?;
                    }
                    if failures.len() > 8 {
                        failures.clear();
                    }
                }
            }
        }
    }

    fn descriptor() -> Result<tokio::net::UnixStream, String> {
        let fd: i32 = std::env::var(PRESENTER_FD_ENV)
            .map_err(|_| "notification panel descriptor is missing")?
            .parse()
            .map_err(|_| "invalid notification panel descriptor")?;
        if fd < 3 {
            return Err("invalid notification panel descriptor".into());
        }
        let descriptor = unsafe { OwnedFd::from_raw_fd(fd) };
        let peer = claw_display_control::ProcessIdentity::unix_peer(descriptor.as_fd())
            .map_err(|error| error.to_string())?;
        if peer.uid() != unsafe { libc::geteuid() } {
            return Err("notification panel descriptor belongs to another owner".into());
        }
        let stream = claw_display_control::identity::unix_stream(descriptor)
            .map_err(|error| error.to_string())?;
        stream
            .set_nonblocking(true)
            .map_err(|error| error.to_string())?;
        tokio::net::UnixStream::from_std(stream).map_err(|error| error.to_string())
    }

    async fn parent_socket(stream: tokio::net::UnixStream) -> zbus::Result<()> {
        let (stop, _) = tokio::sync::watch::channel(false);
        let connection = Builder::socket(stream)
            .p2p()
            .server(Guid::generate())?
            .serve_at(
                SOCKET_PATH,
                Socket {
                    slots: Arc::new(Semaphore::new(8)),
                    stop: stop.clone(),
                },
            )?
            .build()
            .await?;
        let mut messages = zbus::MessageStream::from(&connection);
        use futures_util::StreamExt;
        let result = loop {
            match messages.next().await {
                Some(Ok(_)) => {}
                Some(Err(error)) if peer_closed(&error) => break Ok(()),
                Some(Err(error)) => break Err(error),
                None => break Ok(()),
            }
        };
        stop.send_replace(true);
        result
    }

    pub(super) fn run() -> Result<(), String> {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|error| error.to_string())?
            .block_on(async {
                parent_socket(descriptor()?)
                    .await
                    .map_err(|error| error.to_string())
            })
    }

    #[cfg(test)]
    mod tests {
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/test/unit/bin/claw_notification_panel.rs"
        ));
    }
}
