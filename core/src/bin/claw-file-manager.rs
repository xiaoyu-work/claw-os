//! Same-owner desktop intents, never App authority or a native executable bypass.

fn main() {
    if let Err(refusal) =
        cos::update::runtime::enforce_startup(cos::update::runtime::Scope::CompiledEpoch)
    {
        eprintln!("claw-file-manager: {}", refusal.message);
        std::process::exit(1);
    }
    #[cfg(target_os = "linux")]
    let result = desktop::run();
    #[cfg(not(target_os = "linux"))]
    let result: Result<(), &str> = Err("the desktop intent transport requires Linux");
    if let Err(error) = result {
        eprintln!("claw-file-manager: {error}");
        std::process::exit(1);
    }
}

#[cfg(target_os = "linux")]
mod desktop {
    use std::ffi::OsString;
    use std::io;
    use std::os::unix::process::CommandExt;
    use std::path::PathBuf;
    use std::process::{ExitStatus, Stdio};
    use std::sync::Arc;
    use std::time::Duration;

    use futures_util::StreamExt;
    use tokio::process::{Child, Command};
    use tokio::signal::unix::{signal, SignalKind};
    use tokio::sync::{mpsc, watch, OwnedSemaphorePermit, Semaphore};
    use tokio::task::JoinSet;
    use zbus::fdo::{self, DBusProxy, RequestNameFlags, RequestNameReply};
    use zbus::message::Header;
    use zbus::{connection::Builder, Connection, MessageStream};

    const SERVICE: &str = "org.freedesktop.FileManager1";
    const OBJECT_PATH: &str = "/org/freedesktop/FileManager1";
    const COS: &str = "/usr/local/bin/cos";
    const MAX_PENDING: usize = 8;
    const MAX_URIS: usize = 128;
    const MAX_URI_BYTES: usize = 4096;
    const MAX_ENCODED_BYTES: usize = 64 * 1024;
    const BUS_TIMEOUT: Duration = Duration::from_secs(3);

    fn configured_app(
        mut arguments: impl Iterator<Item = OsString>,
    ) -> Result<String, &'static str> {
        let Some(option) = arguments.next() else {
            return Ok("cosmic-files".into());
        };
        let app = arguments.next();
        if option != "--app" || arguments.next().is_some() {
            return Err("usage: claw-file-manager [--app canonical-app-id]");
        }
        let app = app
            .and_then(|value| value.into_string().ok())
            .ok_or("expected a canonical App ID")?;
        let mut bytes = app.bytes();
        if !matches!(bytes.next(), Some(first) if first.is_ascii_lowercase())
            || !bytes.all(|byte| {
                byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'_' | b'-')
            })
        {
            return Err("expected a canonical App ID");
        }
        if matches!(
            app.as_str(),
            "stdio" | "lint" | "tool" | "install" | "create" | "consent"
        ) {
            return Err("App routing preference cannot select a cos CLI management namespace");
        }
        Ok(app)
    }

    fn validate_intent(uris: &[String], startup_id: &str) -> fdo::Result<()> {
        let invalid = || fdo::Error::InvalidArgs("invalid or oversized desktop URI intent".into());
        if uris.len() > MAX_URIS
            || startup_id.len() > MAX_URI_BYTES
            || startup_id.chars().any(char::is_control)
        {
            return Err(invalid());
        }
        for uri in uris {
            if uri.len() > MAX_URI_BYTES
                || uri.chars().any(char::is_control)
                || url::Url::parse(uri).is_err()
            {
                return Err(invalid());
            }
        }
        if serde_json::to_vec(uris).map_err(|_| invalid())?.len() > MAX_ENCODED_BYTES {
            return Err(invalid());
        }
        Ok(())
    }

    struct Intent {
        uris: Vec<String>,
        _slot: OwnedSemaphorePermit,
    }

    struct FileManager {
        owner_uid: u32,
        pending: mpsc::Sender<Intent>,
        slots: Arc<Semaphore>,
    }

    impl FileManager {
        fn new(owner_uid: u32) -> (Self, mpsc::Receiver<Intent>) {
            let (pending, receiver) = mpsc::channel(MAX_PENDING);
            (
                Self {
                    owner_uid,
                    pending,
                    slots: Arc::new(Semaphore::new(MAX_PENDING)),
                },
                receiver,
            )
        }

        async fn accept(
            &self,
            uris: Vec<String>,
            startup_id: &str,
            connection: &Connection,
            header: Header<'_>,
        ) -> fdo::Result<()> {
            let denied = || fdo::Error::AccessDenied("desktop intent owner mismatch".into());
            let sender = header.sender().ok_or_else(denied)?;
            let caller_uid = tokio::time::timeout(BUS_TIMEOUT, async {
                let bus = DBusProxy::new(connection).await.map_err(|_| denied())?;
                bus.get_connection_unix_user(sender.clone().into())
                    .await
                    .map_err(|_| denied())
            })
            .await
            .map_err(|_| denied())??;
            if caller_uid != self.owner_uid {
                return Err(denied());
            }
            validate_intent(&uris, startup_id)?;
            // StartupID remains ignored presentation data, never a session/owner selector.
            let slot = self.slots.clone().try_acquire_owned().map_err(|_| {
                fdo::Error::LimitsExceeded("too many pending desktop GUI launches".into())
            })?;
            self.pending
                .try_send(Intent { uris, _slot: slot })
                .map_err(|error| match error {
                    mpsc::error::TrySendError::Full(_) => {
                        fdo::Error::LimitsExceeded("too many pending desktop GUI launches".into())
                    }
                    mpsc::error::TrySendError::Closed(_) => {
                        fdo::Error::Failed("desktop intent transport is stopping".into())
                    }
                })
        }
    }

    #[zbus::interface(name = "org.freedesktop.FileManager1")]
    impl FileManager {
        async fn show_folders(
            &self,
            uris: Vec<String>,
            startup_id: &str,
            #[zbus(connection)] connection: &Connection,
            #[zbus(header)] header: Header<'_>,
        ) -> fdo::Result<()> {
            self.accept(uris, startup_id, connection, header).await
        }

        async fn show_items(
            &self,
            uris: Vec<String>,
            startup_id: &str,
            #[zbus(connection)] connection: &Connection,
            #[zbus(header)] header: Header<'_>,
        ) -> fdo::Result<()> {
            self.accept(uris, startup_id, connection, header).await
        }

        async fn show_item_properties(
            &self,
            uris: Vec<String>,
            startup_id: &str,
            #[zbus(connection)] connection: &Connection,
            #[zbus(header)] header: Header<'_>,
        ) -> fdo::Result<()> {
            self.accept(uris, startup_id, connection, header).await
        }
    }

    struct Launcher {
        executable: PathBuf,
        app: String,
        environment: Vec<(OsString, OsString)>,
    }

    impl Launcher {
        fn installed(app: String) -> Self {
            Self {
                executable: PathBuf::from(COS),
                app,
                environment: launcher_environment(std::env::vars_os()),
            }
        }

        #[cfg(test)]
        fn fixture(
            executable: PathBuf,
            app: String,
            environment: Vec<(OsString, OsString)>,
        ) -> Self {
            Self {
                executable,
                app,
                environment: launcher_environment(environment),
            }
        }

        fn spawn(&self, uris: &[String]) -> io::Result<Child> {
            let mut command = Command::new(&self.executable);
            command
                .args(["app", &self.app, "--gui"])
                .args(uris)
                .current_dir("/")
                .env_clear()
                .env("PATH", "/usr/local/bin:/usr/bin:/bin")
                .envs(self.environment.iter().cloned())
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .kill_on_drop(true);
            let parent = std::process::id() as libc::pid_t;
            // A killed facade must not leave an owner CLI retaining a GUI registration.
            unsafe {
                command.as_std_mut().pre_exec(move || {
                    if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) != 0 {
                        return Err(io::Error::last_os_error());
                    }
                    if libc::getppid() != parent {
                        return Err(io::Error::from_raw_os_error(libc::ESRCH));
                    }
                    Ok(())
                });
            }
            command.spawn()
        }
    }

    fn launcher_environment(
        values: impl IntoIterator<Item = (OsString, OsString)>,
    ) -> Vec<(OsString, OsString)> {
        values
            .into_iter()
            .filter(|(key, _)| {
                matches!(
                    key.to_str(),
                    Some(
                        "HOME"
                            | "USER"
                            | "LOGNAME"
                            | "LANG"
                            | "LANGUAGE"
                            | "LC_ALL"
                            | "LC_CTYPE"
                            | "LC_MESSAGES"
                            | "LC_COLLATE"
                            | "LC_NUMERIC"
                            | "LC_MONETARY"
                            | "LC_TIME"
                            | "LC_PAPER"
                            | "LC_NAME"
                            | "LC_ADDRESS"
                            | "LC_TELEPHONE"
                            | "LC_MEASUREMENT"
                            | "LC_IDENTIFICATION"
                            | "TZ"
                            | "XDG_CONFIG_HOME"
                            | "XDG_DATA_HOME"
                            | "XDG_CACHE_HOME"
                            | "XDG_RUNTIME_DIR"
                            | "DBUS_SESSION_BUS_ADDRESS"
                            | "WAYLAND_DISPLAY"
                            | "DISPLAY"
                            | "XAUTHORITY"
                    )
                )
            })
            .collect()
    }

    async fn wait_launcher(
        mut child: Child,
        mut stop: watch::Receiver<bool>,
        _slot: OwnedSemaphorePermit,
    ) -> io::Result<ExitStatus> {
        tokio::select! {
            result = child.wait() => result,
            _ = async { let _ = stop.wait_for(|stopping| *stopping).await; } => {
                let _ = child.start_kill();
                child.wait().await
            }
        }
    }

    fn report(result: Result<io::Result<ExitStatus>, tokio::task::JoinError>) {
        match result {
            Ok(Ok(status)) if status.success() => {}
            Ok(Ok(status)) => {
                eprintln!("claw-file-manager: Root GUI launcher ended without success ({status})");
            }
            _ => eprintln!("claw-file-manager: could not confirm Root GUI launcher exit"),
        }
    }

    async fn dispatch(
        mut pending: mpsc::Receiver<Intent>,
        launcher: Launcher,
        mut stop: watch::Receiver<bool>,
    ) {
        let mut children = JoinSet::new();
        let (cancel, cancelled) = watch::channel(false);
        loop {
            tokio::select! {
                biased;
                _ = stop.wait_for(|stopping| *stopping) => break,
                result = children.join_next(), if !children.is_empty() => {
                    if let Some(result) = result {
                        report(result);
                    }
                }
                intent = pending.recv() => {
                    let Some(intent) = intent else { break };
                    match launcher.spawn(&intent.uris) {
                        Ok(child) => {
                            children.spawn(wait_launcher(child, cancelled.clone(), intent._slot));
                        }
                        Err(_) => eprintln!("claw-file-manager: could not start Root GUI launcher"),
                    }
                }
            }
        }
        pending.close();
        cancel.send_replace(true);
        while pending.try_recv().is_ok() {
            eprintln!("claw-file-manager: queued GUI intent cancelled during shutdown");
        }
        while let Some(result) = children.join_next().await {
            report(result);
        }
    }

    async fn register(
        builder: Builder<'_>,
        interface: FileManager,
    ) -> Result<Connection, &'static str> {
        let connection = builder
            .max_queued(MAX_PENDING)
            .serve_at(OBJECT_PATH, interface)
            .map_err(|_| "could not register the desktop intent interface")?
            .build()
            .await
            .map_err(|_| "could not connect to the desktop session bus")?;
        if connection
            .request_name_with_flags(SERVICE, RequestNameFlags::DoNotQueue.into())
            .await
            .map_err(|_| "FileManager1 is unavailable or already owned")?
            != RequestNameReply::PrimaryOwner
        {
            return Err("FileManager1 is already owned");
        }
        Ok(connection)
    }

    async fn serve(owner_uid: u32, app: String, parent: libc::pid_t) -> Result<(), &'static str> {
        let mut terminate =
            signal(SignalKind::terminate()).map_err(|_| "could not watch desktop shutdown")?;
        let mut interrupt =
            signal(SignalKind::interrupt()).map_err(|_| "could not watch desktop interruption")?;
        if unsafe { libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM) } != 0
            || unsafe { libc::getppid() } != parent
        {
            return Err("could not retain the desktop session parent lifetime");
        }
        let (interface, pending) = FileManager::new(owner_uid);
        let connection = tokio::time::timeout(
            BUS_TIMEOUT,
            register(
                Builder::session().map_err(|_| "no desktop session bus is available")?,
                interface,
            ),
        )
        .await
        .map_err(|_| "desktop session bus registration timed out")??;
        let (stop, stopping) = watch::channel(false);
        let mut dispatcher = tokio::spawn(dispatch(pending, Launcher::installed(app), stopping));
        let mut messages = MessageStream::from(&connection);
        let disconnected = async { while let Some(Ok(_)) = messages.next().await {} };
        let mut failed = false;
        let finished = tokio::select! {
            _ = terminate.recv() => None,
            _ = interrupt.recv() => None,
            _ = disconnected => {
                failed = true;
                None
            }
            result = &mut dispatcher => {
                failed = true;
                Some(result)
            }
        };
        stop.send_replace(true);
        let result = match finished {
            Some(result) => result,
            None => dispatcher.await,
        };
        drop(messages);
        if !matches!(
            tokio::time::timeout(BUS_TIMEOUT, connection.close()).await,
            Ok(Ok(()))
        ) {
            eprintln!("claw-file-manager: desktop bus close was not confirmed");
            failed = true;
        }
        if failed || result.is_err() {
            return Err("desktop intent transport stopped unexpectedly");
        }
        Ok(())
    }

    pub(super) fn run() -> Result<(), &'static str> {
        let uid = unsafe { libc::getuid() };
        if uid == 0 || unsafe { libc::geteuid() } != uid {
            return Err("desktop intents require an unprivileged login owner");
        }
        let parent = unsafe { libc::getppid() };
        if parent <= 1 {
            return Err("desktop intents require a live desktop session parent");
        }
        let app = configured_app(std::env::args_os().skip(1))?;
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|_| "could not initialize the desktop intent transport")?
            .block_on(serve(uid, app, parent))
    }

    #[cfg(test)]
    mod tests {
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/test/unit/bin/claw-file-manager.rs"
        ));
    }
}
