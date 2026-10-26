//! The service: one task owns the radio backend and the lease, and the
//! D-Bus object hands it the calls. Everything that ends a lease (release,
//! expiry, the owner leaving the bus, the link going) goes through that
//! task, so they can't race.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::StreamExt;
use owly_radio::radio::{Backend, Capabilities, ErrorKind, Link, LinkState, PeerInfo};
use tokio::sync::{mpsc, oneshot};
use tokio::time::Instant;
use zbus::fdo::{DBusProxy, NameOwnerChangedStream};
use zbus::message::Header;
use zbus::names::{BusName, OwnedUniqueName};
use zbus::object_server::{InterfaceRef, SignalEmitter};
use zbus::zvariant::{OwnedValue, Value};

use crate::api::{self, ACQUIRE, LinkInfo, NAME, PATH};

/// How owlyd runs.
pub struct Options {
    /// The bus to serve on; the system bus when None.
    pub bus: Option<String>,
    /// Ask polkit whether a caller other than root may acquire the radio.
    /// Off only for a fake radio.
    pub polkit: bool,
}

#[derive(Debug, zbus::DBusError)]
#[zbus(prefix = "io.github.t4t5.Owly.Error")]
pub enum ApiError {
    #[zbus(error)]
    ZBus(zbus::Error),
    /// Another lease holds the radio.
    Busy(String),
    /// Not now; trying again later may work.
    NotNow(String),
    /// No radio to bring up, or it failed.
    NoRadio(String),
    /// The lease named isn't the caller's current one.
    StaleLease(String),
    NotAuthorized(String),
}

impl From<zbus::fdo::Error> for ApiError {
    fn from(e: zbus::fdo::Error) -> Self {
        ApiError::ZBus(e.into())
    }
}

/// What the properties read, kept by the task.
#[derive(Debug, Clone)]
struct Shared {
    state: &'static str,
    lease: u64,
    link: Option<LinkInfo>,
    peers: Vec<PeerInfo>,
    /// The last lease to end, and why.
    last_end: (u64, String),
}

type Reply<T> = oneshot::Sender<Result<T, ApiError>>;

/// The probe, which runs apart from the backend.
type Prober = Arc<dyn Fn() -> Capabilities + Send + Sync>;

enum Cmd {
    Acquire {
        owner: OwnedUniqueName,
        duration: u32,
        /// The lease outlives its owner on the bus.
        detached: bool,
        reply: Reply<(u64, LinkInfo)>,
    },
    Release {
        owner: OwnedUniqueName,
        /// The caller may release a detached lease.
        root: bool,
        lease: u64,
        reply: Reply<()>,
    },
    Shutdown,
}

/// The D-Bus object.
struct Radio {
    tx: mpsc::Sender<Cmd>,
    shared: Arc<Mutex<Shared>>,
    prober: Prober,
    polkit: bool,
}

#[zbus::interface(name = "io.github.t4t5.Owly.Radio1")]
impl Radio {
    async fn probe(&self) -> Result<String, ApiError> {
        let prober = self.prober.clone();
        let caps = tokio::task::spawn_blocking(move || prober())
            .await
            .map_err(|e| ApiError::NoRadio(e.to_string()))?;
        Ok(serde_json::to_string(&caps).expect("capabilities serialize"))
    }

    async fn acquire(
        &self,
        duration: u32,
        #[zbus(header)] header: Header<'_>,
        #[zbus(connection)] conn: &zbus::Connection,
    ) -> Result<(u64, HashMap<String, OwnedValue>), ApiError> {
        let owner = sender(&header)?;
        if self.polkit && !authorized(conn, &owner).await? {
            return Err(ApiError::NotAuthorized(
                "Not allowed to turn on AirDrop's radio from here.".into(),
            ));
        }
        self.acquire_as(owner, duration, false).await
    }

    async fn acquire_detached(
        &self,
        duration: u32,
        #[zbus(header)] header: Header<'_>,
        #[zbus(connection)] conn: &zbus::Connection,
    ) -> Result<(u64, HashMap<String, OwnedValue>), ApiError> {
        let owner = sender(&header)?;
        if !self.root(conn, &owner).await? {
            return Err(ApiError::NotAuthorized(
                "Only root may hold AirDrop's radio past its own exit.".into(),
            ));
        }
        self.acquire_as(owner, duration, true).await
    }

    async fn release(
        &self,
        lease: u64,
        #[zbus(header)] header: Header<'_>,
        #[zbus(connection)] conn: &zbus::Connection,
    ) -> Result<(), ApiError> {
        let owner = sender(&header)?;
        let root = self.root(conn, &owner).await?;
        let (reply, rx) = oneshot::channel();
        self.send(Cmd::Release {
            owner,
            root,
            lease,
            reply,
        })
        .await?;
        rx.await.map_err(gone)?
    }

    #[zbus(signal, name = "StateChanged")]
    async fn radio_state(
        emitter: &SignalEmitter<'_>,
        lease: u64,
        state: &str,
        reason: &str,
    ) -> zbus::Result<()>;

    #[zbus(property, name = "State")]
    async fn current_state(&self) -> String {
        self.shared.lock().unwrap().state.to_string()
    }

    #[zbus(property)]
    async fn lease(&self) -> u64 {
        self.shared.lock().unwrap().lease
    }

    #[zbus(property)]
    async fn link(&self) -> HashMap<String, OwnedValue> {
        self.shared
            .lock()
            .unwrap()
            .link
            .as_ref()
            .map(LinkInfo::to_dict)
            .unwrap_or_default()
    }

    #[zbus(property)]
    async fn last_end(&self) -> (u64, String) {
        self.shared.lock().unwrap().last_end.clone()
    }

    #[zbus(property)]
    async fn peers(&self) -> Vec<HashMap<String, OwnedValue>> {
        self.shared
            .lock()
            .unwrap()
            .peers
            .iter()
            .map(|p| {
                let mut d = HashMap::new();
                d.insert("mac".to_string(), owned(Value::from(p.mac.to_string())));
                if let Some(rssi) = p.rssi {
                    d.insert("rssi".to_string(), owned(Value::from(i16::from(rssi))));
                }
                d
            })
            .collect()
    }
}

impl Radio {
    async fn acquire_as(
        &self,
        owner: OwnedUniqueName,
        duration: u32,
        detached: bool,
    ) -> Result<(u64, HashMap<String, OwnedValue>), ApiError> {
        let (reply, rx) = oneshot::channel();
        self.send(Cmd::Acquire {
            owner,
            duration,
            detached,
            reply,
        })
        .await?;
        let (lease, link) = rx.await.map_err(gone)??;
        Ok((lease, link.to_dict()))
    }

    /// Whether the caller is root, or, with polkit off for a fake radio,
    /// trusted as if it were.
    async fn root(&self, conn: &zbus::Connection, who: &OwnedUniqueName) -> Result<bool, ApiError> {
        Ok(!self.polkit || is_root(conn, who).await?)
    }

    async fn send(&self, cmd: Cmd) -> Result<(), ApiError> {
        self.tx
            .send(cmd)
            .await
            .map_err(|_| ApiError::NoRadio("owlyd is shutting down.".into()))
    }
}

fn owned(v: Value<'_>) -> OwnedValue {
    v.try_to_owned().expect("no file descriptors")
}

fn gone(_: oneshot::error::RecvError) -> ApiError {
    ApiError::NoRadio("owlyd is shutting down.".into())
}

fn sender(header: &Header<'_>) -> Result<OwnedUniqueName, ApiError> {
    header
        .sender()
        .map(|s| s.to_owned().into())
        .ok_or_else(|| ApiError::NotAuthorized("A caller with no bus name.".into()))
}

#[zbus::proxy(
    interface = "org.freedesktop.PolicyKit1.Authority",
    default_service = "org.freedesktop.PolicyKit1",
    default_path = "/org/freedesktop/PolicyKit1/Authority"
)]
trait Authority {
    fn check_authorization(
        &self,
        subject: &(&str, HashMap<&str, Value<'_>>),
        action_id: &str,
        details: HashMap<&str, &str>,
        flags: u32,
        cancellation_id: &str,
    ) -> zbus::Result<(bool, bool, HashMap<String, String>)>;
}

async fn is_root(conn: &zbus::Connection, who: &OwnedUniqueName) -> Result<bool, ApiError> {
    let uid = DBusProxy::new(conn)
        .await?
        .get_connection_unix_user(BusName::from(who.as_ref()))
        .await?;
    Ok(uid == 0)
}

/// Root may; anyone else as polkit says, which may ask them to
/// authenticate.
async fn authorized(conn: &zbus::Connection, who: &OwnedUniqueName) -> Result<bool, ApiError> {
    if is_root(conn, who).await? {
        return Ok(true);
    }
    const ALLOW_USER_INTERACTION: u32 = 1;
    let mut subject = HashMap::new();
    subject.insert("name", Value::from(who.as_str()));
    let (ok, _, _) = AuthorityProxy::new(conn)
        .await?
        .check_authorization(
            &("system-bus-name", subject),
            ACQUIRE,
            HashMap::new(),
            ALLOW_USER_INTERACTION,
            "",
        )
        .await
        .map_err(|e| {
            tracing::warn!("asking polkit: {e}");
            ApiError::NotAuthorized(format!("polkit couldn't be asked: {e}"))
        })?;
    Ok(ok)
}

/// The radio's lease while it's up.
struct Live {
    id: u64,
    owner: OwnedUniqueName,
    /// The owner leaving the bus; None for a detached lease, which root
    /// releases from any connection.
    owner_gone: Option<NameOwnerChangedStream>,
    link: Link,
    expires: Option<Instant>,
}

/// The task that owns the backend.
struct Task<B> {
    backend: B,
    conn: zbus::Connection,
    iface: InterfaceRef<Radio>,
    shared: Arc<Mutex<Shared>>,
    next: u64,
    live: Option<Live>,
}

enum Event {
    Cmd(Option<Cmd>),
    Expired,
    OwnerGone,
    Peers,
    State,
}

/// Recovers what a previous owlyd left, then serves `backend`, with `probe`
/// answering `Probe()`, until `shutdown` completes, when it takes the radio
/// down.
pub async fn serve<B: Backend>(
    mut backend: B,
    probe: impl Fn() -> Capabilities + Send + Sync + 'static,
    opts: Options,
    shutdown: impl std::future::Future<Output = ()> + Send + 'static,
) -> Result<(), String> {
    // Before any lease: a crash of owlyd itself mustn't leave Wi-Fi down.
    if let Err(e) = backend.recover().await {
        tracing::error!("undoing the last session: {e}");
        return Err(format!("Radio startup recovery failed: {e}"));
    }
    let shared = Arc::new(Mutex::new(Shared {
        state: "idle",
        lease: 0,
        link: None,
        peers: Vec::new(),
        last_end: (0, String::new()),
    }));
    let (tx, mut rx) = mpsc::channel(16);
    let radio = Radio {
        tx: tx.clone(),
        shared: shared.clone(),
        prober: Arc::new(probe),
        polkit: opts.polkit,
    };
    let conn = api::connect(opts.bus.as_deref())
        .await
        .map_err(|e| format!("connecting to the bus: {e}"))?;
    conn.object_server()
        .at(PATH, radio)
        .await
        .map_err(|e| e.to_string())?;
    conn.request_name(NAME)
        .await
        .map_err(|e| format!("taking {NAME} on the bus: {e}"))?;
    let iface = conn
        .object_server()
        .interface::<_, Radio>(PATH)
        .await
        .map_err(|e| e.to_string())?;
    tracing::info!("serving {NAME}");

    tokio::spawn(async move {
        shutdown.await;
        let _ = tx.send(Cmd::Shutdown).await;
    });

    let mut task = Task {
        backend,
        conn,
        iface,
        shared,
        next: 0,
        live: None,
    };
    loop {
        let event = match task.live.as_mut() {
            None => Event::Cmd(rx.recv().await),
            Some(live) => {
                let expiry = async {
                    match live.expires {
                        Some(at) => tokio::time::sleep_until(at).await,
                        None => std::future::pending().await,
                    }
                };
                let owner_gone = async {
                    match &mut live.owner_gone {
                        Some(s) => s.next().await,
                        None => std::future::pending().await,
                    }
                };
                tokio::select! {
                    cmd = rx.recv() => Event::Cmd(cmd),
                    () = expiry => Event::Expired,
                    Some(signal) = owner_gone => {
                        match signal.args() {
                            Ok(a) if a.new_owner().is_none() => Event::OwnerGone,
                            _ => continue,
                        }
                    }
                    Ok(()) = live.link.peers.changed() => Event::Peers,
                    Ok(()) = live.link.state.changed() => Event::State,
                }
            }
        };
        match event {
            Event::Cmd(None) | Event::Cmd(Some(Cmd::Shutdown)) => {
                task.end("stopped").await.map_err(|e| e.to_string())?;
                tracing::info!("stopped");
                return Ok(());
            }
            Event::Cmd(Some(cmd)) => task.handle(cmd).await,
            Event::Expired => task.end("expired").await.map_err(|e| e.to_string())?,
            Event::OwnerGone => task.end("owner-gone").await.map_err(|e| e.to_string())?,
            Event::Peers => task.peers().await,
            Event::State => {
                let lost = match &*task.live.as_ref().unwrap().link.state.borrow() {
                    LinkState::Lost(why) => Some(why.clone()),
                    LinkState::Up => None,
                };
                if let Some(why) = lost {
                    task.end(&format!("lost: {why}")).await.map_err(|e| e.to_string())?;
                }
            }
        }
    }
}

impl<B: Backend> Task<B> {
    async fn handle(&mut self, cmd: Cmd) {
        match cmd {
            Cmd::Acquire {
                owner,
                duration,
                detached,
                reply,
            } => {
                let r = self.acquire(owner, duration, detached).await;
                let _ = reply.send(r);
            }
            Cmd::Release {
                owner,
                root,
                lease,
                reply,
            } => {
                let r = match self.check(&owner, root, lease) {
                    Ok(()) => self.end("released").await,
                    Err(e) => Err(e),
                };
                let _ = reply.send(r);
            }
            Cmd::Shutdown => unreachable!("handled by the loop"),
        }
    }

    /// The lease is the current one, and `owner`'s, or detached and
    /// `owner` root.
    fn check(&self, owner: &OwnedUniqueName, root: bool, lease: u64) -> Result<(), ApiError> {
        match &self.live {
            Some(l) if l.id == lease && (&l.owner == owner || l.owner_gone.is_none() && root) => {
                Ok(())
            }
            _ => Err(ApiError::StaleLease(format!(
                "Lease {lease} isn't held by this caller."
            ))),
        }
    }

    async fn acquire(
        &mut self,
        owner: OwnedUniqueName,
        duration: u32,
        detached: bool,
    ) -> Result<(u64, LinkInfo), ApiError> {
        if let Some(l) = &self.live {
            return Err(ApiError::Busy(format!(
                "AirDrop's radio is in use (lease {}).",
                l.id
            )));
        }
        // Watched from before the radio comes up, so an owner that leaves
        // meanwhile isn't missed.
        let dbus = DBusProxy::new(&self.conn).await?;
        let owner_gone = if detached {
            None
        } else {
            Some(
                dbus.receive_name_owner_changed_with_args(&[(0, owner.as_str())])
                    .await?,
            )
        };
        self.next += 1;
        let id = self.next;
        self.set_state("starting", 0, id, "acquire").await;
        let link = match self.backend.up().await {
            Ok(link) => link,
            Err(e) => {
                tracing::warn!("the radio didn't come up: {e}");
                self.set_state("idle", 0, id, &format!("failed: {e}")).await;
                return Err(match e.kind {
                    ErrorKind::NotNow => ApiError::NotNow(e.message),
                    ErrorKind::NoRadio => ApiError::NoRadio(e.message),
                });
            }
        };
        let info = LinkInfo {
            interface: link.name.clone(),
            index: link.index,
            mac: link.mac.to_string(),
            address: link.addr.to_string(),
            announce: link.announce,
        };
        self.live = Some(Live {
            id,
            owner: owner.clone(),
            owner_gone,
            link,
            expires: (duration > 0).then(|| Instant::now() + Duration::from_secs(duration.into())),
        });
        if !detached
            && !dbus
                .name_has_owner(BusName::from(owner.as_ref()))
                .await
                .unwrap_or(false)
        {
            self.end("owner-gone").await?;
            return Err(ApiError::NoRadio("The caller left.".into()));
        }
        tracing::info!(lease = id, owner = %owner, detached, "{} up at {}", info.interface, info.address);
        self.set_link(Some(info.clone())).await;
        self.set_state("up", id, id, "acquired").await;
        self.peers().await;
        Ok((id, info))
    }

    /// Ends the lease, if there is one, and gives Wi-Fi back.
    async fn end(&mut self, reason: &str) -> Result<(), ApiError> {
        let Some(live) = self.live.take() else {
            return Ok(());
        };
        let id = live.id;
        tracing::info!(lease = id, "ending the lease: {reason}");
        self.set_state("stopping", id, id, reason).await;
        drop(live);
        if let Err(e) = self.backend.down().await {
            tracing::error!("taking the radio down: {e}");
            self.set_state("recovery-failed", 0, id, &e.to_string()).await;
            return Err(ApiError::NoRadio(format!(
                "Radio recovery failed; inspect the network before retrying: {e}"
            )));
        }
        self.shared.lock().unwrap().peers.clear();
        let _ = self
            .iface
            .get()
            .await
            .peers_changed(self.iface.signal_emitter())
            .await;
        self.set_link(None).await;
        self.shared.lock().unwrap().last_end = (id, reason.to_string());
        let _ = self
            .iface
            .get()
            .await
            .last_end_changed(self.iface.signal_emitter())
            .await;
        self.set_state("idle", 0, id, reason).await;
        Ok(())
    }

    /// Sets the `State` and `Lease` properties, and signals the change for
    /// lease `about`.
    async fn set_state(&self, state: &'static str, lease: u64, about: u64, reason: &str) {
        let changed_lease = {
            let mut s = self.shared.lock().unwrap();
            s.state = state;
            let changed = s.lease != lease;
            s.lease = lease;
            changed
        };
        let emitter = self.iface.signal_emitter();
        let iface = self.iface.get().await;
        let _ = iface.state_changed(emitter).await;
        if changed_lease {
            let _ = iface.lease_changed(emitter).await;
        }
        let _ = Radio::radio_state(emitter, about, state, reason).await;
    }

    /// Sets the `Link` property: the up link, for clients other than the
    /// lease's owner, such as `owly send` beside `owly radio up`.
    async fn set_link(&self, link: Option<LinkInfo>) {
        self.shared.lock().unwrap().link = link;
        let _ = self
            .iface
            .get()
            .await
            .link_changed(self.iface.signal_emitter())
            .await;
    }

    async fn peers(&mut self) {
        let Some(live) = &mut self.live else { return };
        let now = live.link.peers.borrow_and_update().clone();
        self.shared.lock().unwrap().peers = now;
        let _ = self
            .iface
            .get()
            .await
            .peers_changed(self.iface.signal_emitter())
            .await;
    }
}
