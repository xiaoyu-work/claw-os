//! The broker route registry.
//!
//! One table is the whole route surface. A row ties together the wire
//! command name, the typed request body that name decodes into, the
//! access class that may reach it, whether it mutates, the concurrency
//! and time budget it runs under, and the handler it dispatches to.
//!
//! There is no way to add a route without declaring all of them: the
//! `routes!` macro generates the [`Command`] enum, [`ROUTES`] and the
//! name lookup from the same rows, so a route that is not in this table
//! does not exist on the wire, and a row that omits a field does not
//! compile. [`crate::audit_policy`] is checked against this table by a
//! unit test, so a new route cannot reach a durable sink unclassified
//! either.
//!
//! Access classes are the historical allowlist, unchanged:
//! `context.update` is root-only and everything else is reachable by an
//! authenticated non-root peer. Reaching a route is not the same as
//! being allowed to act: the route still derives identity, session and
//! capability from the peer the kernel named.

use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

use serde::de::DeserializeOwned;
use serde::Serialize;
use serde_json::{json, Value};

use super::client_identity::ClientIdentity;
use super::protocol::BrokerError;
use super::state::DaemonState;
use super::wire::bounded::MAX_WAIT_MS;
use super::wire::requests as body;
use super::wire::Fault;
use super::{
    accessibility, app_sessions, audio, backup, bluetooth, camera, clipboard, config_editor,
    containers, context, context_events, crash, credentials, desktop, display, event_center,
    firewall, hardware, location, memory, network, packages, permissions, power, printer,
    scheduler, security, snapshots, storage, system_journal, systemd, tasks, transactions,
    usb_guard, users,
};

/// Who may reach a route at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Access {
    /// Any authenticated peer, root included.
    User,
    /// Root only.
    Root,
}

/// Whether a route changes privileged state.
///
/// Mutations are the ones a replayed frame must not repeat, so they are
/// the ones the broker's bounded duplicate check covers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Query,
    Mutation,
}

/// How long a route may run before the broker stops waiting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Deadline {
    /// Safe to drop at this point: the route reads, or its writes are
    /// already committed at every await it can be cancelled on.
    Interruptible(Duration),
    /// Never dropped. Cancelling could leave a privileged mutation half
    /// applied — a package part-installed, a unit part-restored — so the
    /// route bounds itself with its own lock and subprocess timeouts and
    /// the broker only bounds how many may run at once.
    Uninterruptible,
}

/// Concurrency and time a route is allowed to consume.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Budget {
    /// Requests of this route that may run at the same time.
    pub max_in_flight: u32,
    pub deadline: Deadline,
}

impl Budget {
    /// Cheap, purely in-memory reads.
    const fn fast() -> Self {
        Self {
            max_in_flight: 64,
            deadline: Deadline::Interruptible(Duration::from_secs(10)),
        }
    }

    /// Reads that touch a store or the filesystem.
    const fn query() -> Self {
        Self {
            max_in_flight: 32,
            deadline: Deadline::Interruptible(Duration::from_secs(120)),
        }
    }

    /// Long polls. The caller's own `timeout_ms` is already capped at
    /// [`MAX_WAIT_MS`]; this is that ceiling plus slack, and the
    /// in-flight cap is what keeps a flood of waiters bounded.
    const fn poll() -> Self {
        Self {
            max_in_flight: 32,
            deadline: Deadline::Interruptible(Duration::from_millis(MAX_WAIT_MS + 60_000)),
        }
    }

    /// Privileged state changes.
    const fn mutation() -> Self {
        Self {
            max_in_flight: 8,
            deadline: Deadline::Uninterruptible,
        }
    }

    /// Privileged state changes that drive a long external tool —
    /// package managers, backup engines, snapshot and storage work.
    const fn heavy() -> Self {
        Self {
            max_in_flight: 4,
            deadline: Deadline::Uninterruptible,
        }
    }

    /// Launch authority. Bounded harder than a general mutation because
    /// each call can file approval requests.
    const fn launch() -> Self {
        Self {
            max_in_flight: 16,
            deadline: Deadline::Uninterruptible,
        }
    }
}

/// Everything a handler is given.
pub struct RouteCall<'a> {
    pub state: &'a DaemonState,
    pub client: &'a ClientIdentity,
    /// The canonical object rebuilt from the route's typed body. Every
    /// key in it was declared and validated by
    /// [`super::wire::requests`].
    pub params: Value,
}

pub type RouteFuture<'a> = Pin<Box<dyn Future<Output = Result<Value, BrokerError>> + Send + 'a>>;
pub type RouteHandler = for<'a> fn(RouteCall<'a>) -> RouteFuture<'a>;
pub type RouteDecoder = fn(Value) -> Result<Value, Fault>;

pub struct Route {
    pub command: Command,
    /// The wire name. Also the key [`crate::audit_policy`] is written
    /// against, and the only version of the name that reaches a record.
    pub name: &'static str,
    pub access: Access,
    pub kind: Kind,
    pub budget: Budget,
    pub decode: RouteDecoder,
    pub handler: RouteHandler,
}

impl Route {
    /// Whether this peer's access class may reach the route.
    ///
    /// Identity comes from the credentials the kernel attached to the
    /// request message; nothing in the request selects it.
    pub fn authorize(&self, client: &ClientIdentity) -> Result<(), Fault> {
        let uid = client.uid.ok_or(Fault::MissingCredentials)?;
        match self.access {
            Access::User => Ok(()),
            Access::Root if uid == 0 => Ok(()),
            Access::Root => Err(Fault::NotAuthorized),
        }
    }

    /// The audit policy this route's fields are projected through.
    pub fn audit_policy(&self) -> Option<&'static crate::audit_policy::CommandPolicy> {
        crate::audit_policy::command_policy(self.name)
    }
}

/// Decode a route's parameters into its declared body, then rebuild the
/// canonical object the handler reads.
///
/// The round trip is the point: only fields the body type declared
/// survive it, so a handler cannot reach a value the boundary did not
/// validate, and `deny_unknown_fields` has already refused anything
/// else. The `serde` error is deliberately not carried out of here —
/// it quotes the offending value, which may be a credential.
fn decode_body<T: DeserializeOwned + Serialize>(params: Value) -> Result<Value, Fault> {
    let params = if params.is_null() {
        Value::Object(serde_json::Map::new())
    } else {
        params
    };
    let typed = serde_json::from_value::<T>(params).map_err(|error| {
        tracing::debug!(
            category = ?error.classify(),
            "clawd refused route parameters"
        );
        Fault::InvalidParams
    })?;
    serde_json::to_value(typed).map_err(|_| Fault::InvalidParams)
}

macro_rules! routes {
    (
        $(
            $variant:ident {
                name: $name:literal,
                access: $access:expr,
                kind: $kind:expr,
                budget: $budget:expr,
                body: $body:ty,
                run: |$call:ident| $run:expr,
            }
        )*
    ) => {
        /// Every route `clawd` serves, as an enum an in-repo client can
        /// name at compile time.
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        pub enum Command {
            $( $variant, )*
        }

        impl Command {
            pub const ALL: &'static [Command] = &[ $( Command::$variant, )* ];

            pub fn as_str(self) -> &'static str {
                match self {
                    $( Command::$variant => $name, )*
                }
            }

            /// Resolve a wire name. `None` is the only outcome for a
            /// name this daemon does not route, so an unknown command
            /// fails closed before authorization.
            pub fn parse(value: &str) -> Option<Self> {
                match value {
                    $( $name => Some(Command::$variant), )*
                    _ => None,
                }
            }

            pub fn route(self) -> &'static Route {
                &ROUTES[self as usize]
            }
        }

        pub static ROUTES: &[Route] = &[
            $(
                Route {
                    command: Command::$variant,
                    name: $name,
                    access: $access,
                    kind: $kind,
                    budget: $budget,
                    decode: {
                        fn decode(params: Value) -> Result<Value, Fault> {
                            decode_body::<$body>(params)
                        }
                        decode
                    },
                    handler: {
                        fn handler<'a>($call: RouteCall<'a>) -> RouteFuture<'a> {
                            Box::pin(async move { $run })
                        }
                        handler
                    },
                },
            )*
        ];
    };
}

impl serde::Serialize for Command {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> serde::Deserialize<'de> for Command {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        Command::parse(&raw).ok_or_else(|| serde::de::Error::custom("unknown clawd command"))
    }
}

impl std::fmt::Display for Command {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

routes! {
    // -----------------------------------------------------------------
    // Daemon
    // -----------------------------------------------------------------
    DaemonHealth {
        name: "daemon.health",
        access: Access::User,
        kind: Kind::Query,
        budget: Budget::fast(),
        body: body::NoBody,
        run: |c| Ok(json!({
            "status": "ok",
            "daemon": "clawd",
            "started_at": c.state.started_at(),
            "uptime_ms": c.state.uptime_millis(),
        })),
    }
    DaemonStatus {
        name: "daemon.status",
        access: Access::User,
        kind: Kind::Query,
        budget: Budget::query(),
        body: body::NoBody,
        run: |c| Ok(json!({
            "status": "running",
            "daemon": "clawd",
            "started_at": c.state.started_at(),
            "uptime_ms": c.state.uptime_millis(),
            "tasks": tasks::counts(c.client)?,
            "context": context::snapshot_for_client(c.state, c.client)?,
            "transactions": transactions::list(c.state, c.client)?,
        })),
    }

    // -----------------------------------------------------------------
    // Agent tasks
    // -----------------------------------------------------------------
    TaskSubmit {
        name: "task.submit",
        access: Access::User,
        kind: Kind::Mutation,
        budget: Budget::mutation(),
        body: body::TaskSubmit,
        run: |c| tasks::submit(c.params, c.client).await.map_err(BrokerError::from),
    }
    TaskList {
        name: "task.list",
        access: Access::User,
        kind: Kind::Query,
        budget: Budget::query(),
        body: body::TaskList,
        run: |c| tasks::list(c.params, c.client).map_err(BrokerError::from),
    }
    TaskGet {
        name: "task.get",
        access: Access::User,
        kind: Kind::Query,
        budget: Budget::query(),
        body: body::TaskId,
        run: |c| tasks::get(c.params, c.client).map_err(BrokerError::from),
    }
    TaskStatus {
        name: "task.status",
        access: Access::User,
        kind: Kind::Query,
        budget: Budget::query(),
        body: body::TaskId,
        run: |c| tasks::get(c.params, c.client).map_err(BrokerError::from),
    }
    TaskCancel {
        name: "task.cancel",
        access: Access::User,
        kind: Kind::Mutation,
        budget: Budget::mutation(),
        body: body::TaskId,
        run: |c| tasks::cancel(c.params, c.client).map_err(BrokerError::from),
    }
    TaskStream {
        name: "task.stream",
        access: Access::User,
        kind: Kind::Query,
        budget: Budget::poll(),
        body: body::TaskWait,
        run: |c| tasks::result(c.params, c.client).await.map_err(BrokerError::from),
    }
    TaskResult {
        name: "task.result",
        access: Access::User,
        kind: Kind::Query,
        budget: Budget::poll(),
        body: body::TaskWait,
        run: |c| tasks::result(c.params, c.client).await.map_err(BrokerError::from),
    }
    TaskCount {
        name: "task.count",
        access: Access::User,
        kind: Kind::Query,
        budget: Budget::fast(),
        body: body::NoBody,
        run: |c| tasks::counts(c.client).map_err(BrokerError::from),
    }

    // -----------------------------------------------------------------
    // Memory, context and journals
    // -----------------------------------------------------------------
    MemoryHistory {
        name: "memory.history",
        access: Access::User,
        kind: Kind::Query,
        budget: Budget::query(),
        body: body::MemoryHistory,
        run: |c| memory::history(c.params, c.client).map_err(BrokerError::from),
    }
    MemorySessions {
        name: "memory.sessions",
        access: Access::User,
        kind: Kind::Query,
        budget: Budget::query(),
        body: body::MemorySessions,
        run: |c| memory::sessions(c.params, c.client).map_err(BrokerError::from),
    }
    ContextSnapshot {
        name: "context.snapshot",
        access: Access::User,
        kind: Kind::Query,
        budget: Budget::query(),
        body: body::NoBody,
        run: |c| context::snapshot_for_client(c.state, c.client).map_err(BrokerError::from),
    }
    ContextSources {
        name: "context.sources",
        access: Access::User,
        kind: Kind::Query,
        budget: Budget::query(),
        body: body::NoBody,
        run: |c| context::sources_for_client(c.state, c.client).map_err(BrokerError::from),
    }
    ContextUpdate {
        name: "context.update",
        access: Access::Root,
        kind: Kind::Mutation,
        budget: Budget::mutation(),
        body: body::ContextUpdate,
        run: |c| context::update(c.state, c.params).map_err(BrokerError::from),
    }
    ContextEventAppend {
        name: "context.event.append",
        access: Access::User,
        kind: Kind::Mutation,
        budget: Budget::mutation(),
        body: body::ContextEventAppend,
        run: |c| context_events::append(c.params, c.client).map_err(BrokerError::from),
    }
    ContextEventQuery {
        name: "context.event.query",
        access: Access::User,
        kind: Kind::Query,
        budget: Budget::query(),
        body: body::ContextEventQuery,
        run: |c| context_events::query_for_client(c.params, c.client).map_err(BrokerError::from),
    }
    SystemOperations {
        name: "system.operations",
        access: Access::User,
        kind: Kind::Query,
        budget: Budget::query(),
        body: body::SystemOperations,
        run: |c| system_journal::query_for_client(c.params, c.client).map_err(BrokerError::from),
    }

    // -----------------------------------------------------------------
    // Permissions
    // -----------------------------------------------------------------
    PermissionPending {
        name: "permission.pending",
        access: Access::User,
        kind: Kind::Query,
        budget: Budget::query(),
        body: body::PermissionList,
        run: |c| permissions::pending(c.params, c.client).map_err(BrokerError::from),
    }
    PermissionRecent {
        name: "permission.recent",
        access: Access::User,
        kind: Kind::Query,
        budget: Budget::query(),
        body: body::PermissionList,
        run: |c| permissions::recent(c.params, c.client).map_err(BrokerError::from),
    }
    PermissionStatus {
        name: "permission.status",
        access: Access::User,
        kind: Kind::Query,
        budget: Budget::query(),
        body: body::PermissionStatus,
        run: |c| permissions::status(c.params, c.client).map_err(BrokerError::from),
    }
    PermissionRequest {
        name: "permission.request",
        access: Access::User,
        kind: Kind::Mutation,
        budget: Budget::mutation(),
        body: body::PermissionRequest,
        run: |c| permissions::request(c.params, c.client).map_err(BrokerError::from),
    }
    PermissionDecide {
        name: "permission.decide",
        access: Access::User,
        kind: Kind::Mutation,
        budget: Budget::mutation(),
        body: body::PermissionDecide,
        run: |c| permissions::decide(c.params, c.client).map_err(BrokerError::from),
    }

    // -----------------------------------------------------------------
    // Transactions
    // -----------------------------------------------------------------
    TransactionBegin {
        name: "transaction.begin",
        access: Access::User,
        kind: Kind::Mutation,
        budget: Budget::mutation(),
        body: body::TransactionBegin,
        run: |c| transactions::begin(c.state, c.params, c.client).map_err(BrokerError::from),
    }
    TransactionList {
        name: "transaction.list",
        access: Access::User,
        kind: Kind::Query,
        budget: Budget::query(),
        body: body::NoBody,
        run: |c| transactions::list(c.state, c.client).map_err(BrokerError::from),
    }
    TransactionCommit {
        name: "transaction.commit",
        access: Access::User,
        kind: Kind::Mutation,
        budget: Budget::mutation(),
        body: body::TransactionId,
        run: |c| transactions::commit(c.state, c.params, c.client).map_err(BrokerError::from),
    }
    TransactionRollback {
        name: "transaction.rollback",
        access: Access::User,
        kind: Kind::Mutation,
        budget: Budget::mutation(),
        body: body::TransactionId,
        run: |c| {
            transactions::rollback(c.state, c.params, c.client)
                .await
                .map_err(BrokerError::from)
        },
    }

    // -----------------------------------------------------------------
    // App / MCP session authority
    // -----------------------------------------------------------------
    AppSessionRegister {
        name: "app_session.register",
        access: Access::User,
        kind: Kind::Mutation,
        budget: Budget::launch(),
        body: body::AppSessionRegister,
        run: |c| app_sessions::register(c.params, c.client).await,
    }
    AppSessionRegisterNative {
        name: "app_session.register_native",
        access: Access::User,
        kind: Kind::Mutation,
        budget: Budget::launch(),
        body: body::AppSessionRegisterNative,
        run: |c| {
            app_sessions::register_native(c.params, c.client)
                .await
                .map_err(BrokerError::from)
        },
    }
    McpSessionRegister {
        name: "mcp_session.register",
        access: Access::User,
        kind: Kind::Mutation,
        budget: Budget::launch(),
        body: body::McpSessionRegister,
        run: |c| {
            app_sessions::register_mcp(c.params, c.client)
                .await
                .map_err(BrokerError::from)
        },
    }
    AppSessionBind {
        name: "app_session.bind",
        access: Access::User,
        kind: Kind::Mutation,
        budget: Budget::launch(),
        body: body::AppSessionBind,
        run: |c| {
            app_sessions::bind(c.params, c.client)
                .await
                .map_err(BrokerError::from)
        },
    }
    AppSessionSetTransient {
        name: "app_session.set_transient",
        access: Access::User,
        kind: Kind::Mutation,
        budget: Budget::launch(),
        body: body::AppSessionSetTransient,
        run: |c| {
            app_sessions::set_transient(c.params, c.client)
                .await
                .map_err(BrokerError::from)
        },
    }
    AppSessionDeregister {
        name: "app_session.deregister",
        access: Access::User,
        kind: Kind::Mutation,
        budget: Budget::launch(),
        body: body::AppSessionDeregister,
        run: |c| {
            app_sessions::deregister(c.params, c.client)
                .await
                .map_err(BrokerError::from)
        },
    }

    // -----------------------------------------------------------------
    // Scheduler authority
    // -----------------------------------------------------------------
    SchedulerRun {
        name: "scheduler.run",
        access: Access::User,
        kind: Kind::Mutation,
        budget: Budget::mutation(),
        body: body::SchedulerRun,
        run: |c| scheduler::run(c.params, c.client).await,
    }

    // -----------------------------------------------------------------
    // Credentials
    // -----------------------------------------------------------------
    CredentialOauthRefresh {
        name: "credential.oauth-refresh",
        access: Access::User,
        kind: Kind::Mutation,
        budget: Budget::mutation(),
        body: body::CredentialOauthRefresh,
        run: |c| {
            credentials::oauth_refresh(c.params, c.client)
                .await
                .map_err(BrokerError::from)
        },
    }

    // -----------------------------------------------------------------
    // System services
    // -----------------------------------------------------------------
    SystemAudioControl {
        name: "system.audio.control",
        access: Access::User,
        kind: Kind::Mutation,
        budget: Budget::mutation(),
        body: body::AudioControl,
        run: |c| audio::control(c.params, c.client).await.map_err(BrokerError::from),
    }
    SystemAccessibilityControl {
        name: "system.accessibility.control",
        access: Access::User,
        kind: Kind::Mutation,
        budget: Budget::mutation(),
        body: body::AccessibilityControl,
        run: |c| {
            accessibility::control(c.params, c.client)
                .await
                .map_err(BrokerError::from)
        },
    }
    SystemBackupControl {
        name: "system.backup.control",
        access: Access::User,
        kind: Kind::Mutation,
        budget: Budget::heavy(),
        body: body::BackupControl,
        run: |c| backup::control(c.params, c.client).await.map_err(BrokerError::from),
    }
    SystemBluetoothControl {
        name: "system.bluetooth.control",
        access: Access::User,
        kind: Kind::Mutation,
        budget: Budget::mutation(),
        body: body::BluetoothControl,
        run: |c| {
            bluetooth::control(c.params, c.client)
                .await
                .map_err(BrokerError::from)
        },
    }
    SystemCameraControl {
        name: "system.camera.control",
        access: Access::User,
        kind: Kind::Mutation,
        budget: Budget::mutation(),
        body: body::CameraControl,
        run: |c| camera::control(c.params, c.client).await.map_err(BrokerError::from),
    }
    SystemClipboardControl {
        name: "system.clipboard.control",
        access: Access::User,
        kind: Kind::Mutation,
        budget: Budget::mutation(),
        body: body::ClipboardControl,
        run: |c| {
            clipboard::control(c.params, c.client)
                .await
                .map_err(BrokerError::from)
        },
    }
    SystemContainerControl {
        name: "system.container.control",
        access: Access::User,
        kind: Kind::Mutation,
        budget: Budget::heavy(),
        body: body::ContainerControl,
        run: |c| {
            containers::control(c.params, c.client)
                .await
                .map_err(BrokerError::from)
        },
    }
    SystemConfigControl {
        name: "system.config.control",
        access: Access::User,
        kind: Kind::Mutation,
        budget: Budget::mutation(),
        body: body::ConfigControl,
        run: |c| {
            config_editor::control(c.params, c.client)
                .await
                .map_err(BrokerError::from)
        },
    }
    SystemCrashInspect {
        name: "system.crash.inspect",
        access: Access::User,
        kind: Kind::Query,
        budget: Budget::query(),
        body: body::CrashInspect,
        run: |c| crash::inspect(c.params, c.client).await.map_err(BrokerError::from),
    }
    SystemDesktopControl {
        name: "system.desktop.control",
        access: Access::User,
        kind: Kind::Mutation,
        budget: Budget::mutation(),
        body: body::DesktopControl,
        run: |c| desktop::control(c.params, c.client).await.map_err(BrokerError::from),
    }
    SystemDisplayControl {
        name: "system.display.control",
        access: Access::User,
        kind: Kind::Mutation,
        budget: Budget::mutation(),
        body: body::DisplayControl,
        run: |c| display::control(c.params, c.client).await.map_err(BrokerError::from),
    }
    SystemEventsControl {
        name: "system.events.control",
        access: Access::User,
        kind: Kind::Mutation,
        budget: Budget::mutation(),
        body: body::EventsControl,
        run: |c| {
            event_center::control(c.params, c.client)
                .await
                .map_err(BrokerError::from)
        },
    }
    SystemFirewallControl {
        name: "system.firewall.control",
        access: Access::User,
        kind: Kind::Mutation,
        budget: Budget::mutation(),
        body: body::FirewallControl,
        run: |c| firewall::control(c.params, c.client).await.map_err(BrokerError::from),
    }
    SystemHardwareInspect {
        name: "system.hardware.inspect",
        access: Access::User,
        kind: Kind::Query,
        budget: Budget::query(),
        body: body::SessionAction,
        run: |c| hardware::inspect(c.params, c.client).await.map_err(BrokerError::from),
    }
    SystemLocationQuery {
        name: "system.location.query",
        access: Access::User,
        kind: Kind::Query,
        budget: Budget::query(),
        body: body::LocationQuery,
        run: |c| location::query(c.params, c.client).await.map_err(BrokerError::from),
    }
    SystemNetworkControl {
        name: "system.network.control",
        access: Access::User,
        kind: Kind::Mutation,
        budget: Budget::mutation(),
        body: body::NetworkControl,
        run: |c| network::control(c.params, c.client).await.map_err(BrokerError::from),
    }
    SystemPackageInstall {
        name: "system.package.install",
        access: Access::User,
        kind: Kind::Mutation,
        budget: Budget::heavy(),
        body: body::PackageInstall,
        run: |c| packages::install(c.params, c.client).await.map_err(BrokerError::from),
    }
    SystemPackageControl {
        name: "system.package.control",
        access: Access::User,
        kind: Kind::Mutation,
        budget: Budget::heavy(),
        body: body::PackageControl,
        run: |c| packages::control(c.params, c.client).await.map_err(BrokerError::from),
    }
    SystemPackageRestore {
        name: "system.package.restore",
        access: Access::User,
        kind: Kind::Mutation,
        budget: Budget::heavy(),
        body: body::PackageRestore,
        run: |c| packages::restore(c.params, c.client).await.map_err(BrokerError::from),
    }
    SystemPowerControl {
        name: "system.power.control",
        access: Access::User,
        kind: Kind::Mutation,
        budget: Budget::mutation(),
        body: body::PowerControl,
        run: |c| power::control(c.params, c.client).await.map_err(BrokerError::from),
    }
    SystemPrinterControl {
        name: "system.printer.control",
        access: Access::User,
        kind: Kind::Mutation,
        budget: Budget::mutation(),
        body: body::PrinterControl,
        run: |c| printer::control(c.params, c.client).await.map_err(BrokerError::from),
    }
    SystemSecurityInspect {
        name: "system.security.inspect",
        access: Access::User,
        kind: Kind::Query,
        budget: Budget::query(),
        body: body::SessionAction,
        run: |c| security::inspect(c.params, c.client).await.map_err(BrokerError::from),
    }
    SystemServiceControl {
        name: "system.service.control",
        access: Access::User,
        kind: Kind::Mutation,
        budget: Budget::mutation(),
        body: body::ServiceControl,
        run: |c| systemd::control(c.params, c.client).await.map_err(BrokerError::from),
    }
    SystemServiceRestore {
        name: "system.service.restore",
        access: Access::User,
        kind: Kind::Mutation,
        budget: Budget::mutation(),
        body: body::ServiceRestore,
        run: |c| systemd::restore(c.params, c.client).await.map_err(BrokerError::from),
    }
    SystemSnapshotControl {
        name: "system.snapshot.control",
        access: Access::User,
        kind: Kind::Mutation,
        budget: Budget::heavy(),
        body: body::SnapshotControl,
        run: |c| snapshots::control(c.params, c.client).await.map_err(BrokerError::from),
    }
    SystemStorageControl {
        name: "system.storage.control",
        access: Access::User,
        kind: Kind::Mutation,
        budget: Budget::heavy(),
        body: body::StorageControl,
        run: |c| storage::control(c.params, c.client).await.map_err(BrokerError::from),
    }
    SystemUsbControl {
        name: "system.usb.control",
        access: Access::User,
        kind: Kind::Mutation,
        budget: Budget::mutation(),
        body: body::UsbControl,
        run: |c| usb_guard::control(c.params, c.client).await.map_err(BrokerError::from),
    }
    SystemUsersControl {
        name: "system.users.control",
        access: Access::User,
        kind: Kind::Mutation,
        budget: Budget::mutation(),
        body: body::UsersControl,
        run: |c| users::control(c.params, c.client).await.map_err(BrokerError::from),
    }
}

/// Route names any authenticated peer may reach.
pub fn user_commands() -> impl Iterator<Item = &'static str> {
    ROUTES
        .iter()
        .filter(|route| route.access == Access::User)
        .map(|route| route.name)
}

/// Route names only root may reach.
pub fn root_commands() -> impl Iterator<Item = &'static str> {
    ROUTES
        .iter()
        .filter(|route| route.access == Access::Root)
        .map(|route| route.name)
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/clawd/routes.rs"
    ));
}
