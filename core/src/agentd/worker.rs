//! Worker-side entry point for `claw-agentd`.
//!
//! By the time this code runs the process has already been stripped of
//! privilege by [`super::spawn`]: it is the task owner, has no
//! supplementary groups, carries `PR_SET_NO_NEW_PRIVS`, leads its own
//! session and process group, and holds no descriptor from the broker
//! except the job channel on fd 3. The worker re-checks all of that
//! before touching the assignment, and refuses to run if anything is
//! still privileged — a failed drop must never silently become a root
//! agent loop.
//!
//! It then runs exactly one task and exits. Everything the agent loop
//! produces — stream deltas, tool progress, runtime audit, permission
//! mediation and the final outcome — leaves through the channel; the
//! worker never opens the job queue, the consent store, the audit log or
//! the broker socket.
//!
//! ## Why the channel gets its own thread
//!
//! `caps::require` is synchronous and is called from inside tool
//! execution, so the approval gateway has to block the calling thread
//! while it waits for the broker's answer. Channel I/O therefore runs on
//! a dedicated thread with its own current-thread runtime: the agent
//! runtime can block every one of its own worker threads without ever
//! stopping the reader that delivers the reply, which is what would
//! otherwise deadlock.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::mpsc::{sync_channel, Receiver, SyncSender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::Value;
use tokio::io::{AsyncWriteExt, BufReader};
use tokio::sync::mpsc::{self, UnboundedSender};

use crate::agent::llm::accumulate::StreamSink;
use crate::agent::llm::StreamEvent;
use crate::agent::llm::ToolCall;
use crate::agent::runtime::hooks::{
    self, Hook, HookContext, HookOutcome, ToolDecision, ToolResultSummary, TurnSummary,
};
use crate::agent::runtime::progress::ProgressSink;
use crate::agent::service::{FinishOutcome, JobExecution};
use crate::audit_policy;
use crate::caps::approval_gateway::{ApprovalGateway, PendingApproval};
use crate::caps::{Scope, Verb};

use super::protocol::{
    self, ApprovalAsk, ApprovalReply, Assignment, BrokerFrame, FrameReader, ProgressRecord,
    RuntimeAuditRecord, WorkerFrame, WorkerHello, WorkerOutcome,
};

const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(15);
const INTERRUPT_RETRY: Duration = Duration::from_millis(100);
/// The broker answers permission mediation locally, so this only has to
/// cover scheduling jitter. It also bounds what a stalled or killed
/// supervisor can cost a tool call.
const APPROVAL_TIMEOUT: Duration = Duration::from_secs(15);
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(60);
const IO_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(10);

/// `claw-agentd` process entry point. Never returns.
pub fn main(args: &[String]) -> ! {
    if args.iter().any(|arg| arg == "-h" || arg == "--help") {
        println!(
            "\
claw-agentd — Claw OS unprivileged agent worker

This binary is spawned by clawd with a private job channel on fd 3 and
runs exactly one agent task as the task owner. It is not intended to be
started by hand.

Usage:
  claw-agentd --worker
"
        );
        std::process::exit(0);
    }
    match run() {
        Ok(()) => std::process::exit(0),
        Err(error) => {
            eprintln!("claw-agentd: {error}");
            std::process::exit(1);
        }
    }
}

fn run() -> Result<(), String> {
    crate::storage::set_private_umask();
    let channel = adopt_channel()?;
    let identity = LocalIdentity::read()?;

    let io = ChannelIo::start(channel, identity.clone())?;
    let assignment = io.handshake()?;
    let task_id = assignment.job.id.clone();

    crate::caps::approval_gateway::install(Arc::new(ChannelApprovalGateway {
        task_id: task_id.clone(),
        state: io.state.clone(),
    }));

    // Routed tool paths use `block_in_place`, which needs the
    // multi-thread scheduler — the same requirement the in-process
    // worker loop had. The extra threads give tool calls room to block
    // on permission mediation without starving each other.
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(4)
        .enable_all()
        .build()
        .map_err(|error| format!("tokio runtime: {error}"))?;

    let cancelled = io.state.cancelled.clone();
    let tx = io.state.tx.clone();
    let outcome = runtime.block_on(execute(assignment, tx.clone(), cancelled.clone()));
    let outcome = if cancelled.load(Ordering::SeqCst) {
        WorkerOutcome::Cancelled
    } else {
        outcome
    };
    let _ = tx.send(WorkerFrame::Result {
        task_id,
        outcome: Box::new(outcome),
    });
    io.finish();
    Ok(())
}

/// Take ownership of the descriptor the broker dup'd onto fd 3. The
/// environment hint must agree with what we find, so a hand-started
/// worker fails instead of talking to whatever happens to be open.
fn adopt_channel() -> Result<std::os::unix::net::UnixStream, String> {
    use std::os::unix::io::FromRawFd;

    let declared = std::env::var(protocol::CHANNEL_FD_ENV)
        .map_err(|_| {
            format!(
                "claw-agentd must be started by clawd; {} is not set",
                protocol::CHANNEL_FD_ENV
            )
        })?
        .trim()
        .parse::<i32>()
        .map_err(|error| format!("invalid {}: {error}", protocol::CHANNEL_FD_ENV))?;
    if declared != protocol::CHANNEL_FD {
        return Err(format!(
            "agentd channel must be fd {}, not {declared}",
            protocol::CHANNEL_FD
        ));
    }
    if unsafe { libc::fcntl(protocol::CHANNEL_FD, libc::F_GETFD) } < 0 {
        return Err("agentd job channel is not open".to_string());
    }
    let stream = unsafe { std::os::unix::net::UnixStream::from_raw_fd(protocol::CHANNEL_FD) };
    stream
        .set_nonblocking(true)
        .map_err(|error| format!("configure agentd channel: {error}"))?;
    Ok(stream)
}

#[derive(Debug, Clone)]
struct LocalIdentity {
    pid: u32,
    start_time_ticks: Option<u64>,
    uid: u32,
    euid: u32,
    gid: u32,
    egid: u32,
    groups: Vec<u32>,
    no_new_privs: bool,
}

impl LocalIdentity {
    fn read() -> Result<Self, String> {
        let pid = std::process::id();
        let uid = unsafe { libc::getuid() } as u32;
        let euid = unsafe { libc::geteuid() } as u32;
        let gid = unsafe { libc::getgid() } as u32;
        let egid = unsafe { libc::getegid() } as u32;
        let mut groups = vec![0 as libc::gid_t; 128];
        let count = unsafe { libc::getgroups(groups.len() as libc::c_int, groups.as_mut_ptr()) };
        if count < 0 {
            return Err(format!(
                "read supplementary groups: {}",
                std::io::Error::last_os_error()
            ));
        }
        groups.truncate(count as usize);
        Ok(Self {
            pid,
            start_time_ticks: crate::proc::read_start_time_ticks_pub(pid),
            uid,
            euid,
            gid,
            egid,
            groups,
            no_new_privs: crate::caps::enforcement::process_has_no_new_privs(),
        })
    }

    /// Fail closed if the process is not the unprivileged identity the
    /// broker leased. Root is never acceptable: a task owned by root is
    /// refused by the supervisor before a worker is spawned, so seeing
    /// root here means the drop silently failed.
    ///
    /// Whether supplementary groups had to be cleared is the *broker's*
    /// policy, checked against the handshake report.
    fn require_expected_identity(&self, owner_uid: u32) -> Result<(), String> {
        if owner_uid == 0 {
            return Err(super::spawn::ROOT_OWNER_REFUSAL.to_string());
        }
        if self.uid != owner_uid || self.euid != owner_uid {
            return Err(format!(
                "agent worker runs as uid {} / euid {} but the task owner is {owner_uid}; \
                 privilege drop failed",
                self.uid, self.euid
            ));
        }
        if self.uid == 0 || self.euid == 0 || self.gid == 0 || self.egid == 0 {
            return Err(
                "refusing to run the agent runtime with root ids; privilege drop failed"
                    .to_string(),
            );
        }
        #[cfg(target_os = "linux")]
        if !self.no_new_privs {
            return Err(
                "refusing to run the agent runtime without PR_SET_NO_NEW_PRIVS".to_string(),
            );
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Channel I/O
// ---------------------------------------------------------------------------

/// Shared handles the agent runtime uses to reach the channel.
#[derive(Debug)]
struct ChannelState {
    tx: UnboundedSender<WorkerFrame>,
    cancelled: Arc<AtomicBool>,
    waiters: Mutex<HashMap<u64, SyncSender<ApprovalReply>>>,
    next_correlation: AtomicU64,
    asks_used: AtomicU32,
}

impl ChannelState {
    fn register(&self, correlation_id: u64) -> Receiver<ApprovalReply> {
        let (tx, rx) = sync_channel(1);
        if let Ok(mut waiters) = self.waiters.lock() {
            waiters.insert(correlation_id, tx);
        }
        rx
    }

    fn forget(&self, correlation_id: u64) {
        if let Ok(mut waiters) = self.waiters.lock() {
            waiters.remove(&correlation_id);
        }
    }

    fn deliver(&self, correlation_id: u64, reply: ApprovalReply) {
        let waiter = self
            .waiters
            .lock()
            .ok()
            .and_then(|mut waiters| waiters.remove(&correlation_id));
        if let Some(waiter) = waiter {
            let _ = waiter.try_send(reply);
        }
    }

    /// Wake every outstanding waiter so a cancelled or disconnected task
    /// cannot leave a tool call blocked until its timeout.
    fn refuse_all(&self, message: &str) {
        let drained: Vec<SyncSender<ApprovalReply>> = self
            .waiters
            .lock()
            .map(|mut waiters| waiters.drain().map(|(_, tx)| tx).collect())
            .unwrap_or_default();
        for waiter in drained {
            let _ = waiter.try_send(ApprovalReply::Refused {
                message: message.to_string(),
            });
        }
    }
}

struct ChannelIo {
    state: Arc<ChannelState>,
    handshake: Mutex<Option<Receiver<Result<Box<Assignment>, String>>>>,
    thread: Mutex<Option<std::thread::JoinHandle<()>>>,
}

impl ChannelIo {
    fn start(
        channel: std::os::unix::net::UnixStream,
        identity: LocalIdentity,
    ) -> Result<Self, String> {
        let (tx, rx) = mpsc::unbounded_channel::<WorkerFrame>();
        let state = Arc::new(ChannelState {
            tx,
            cancelled: Arc::new(AtomicBool::new(false)),
            waiters: Mutex::new(HashMap::new()),
            next_correlation: AtomicU64::new(1),
            asks_used: AtomicU32::new(0),
        });
        let (handshake_tx, handshake_rx) = sync_channel(1);
        let io_state = state.clone();
        let thread = std::thread::Builder::new()
            .name("agentd-io".to_string())
            .spawn(move || {
                let runtime = match tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                {
                    Ok(runtime) => runtime,
                    Err(error) => {
                        let _ = handshake_tx.send(Err(format!("agentd io runtime: {error}")));
                        return;
                    }
                };
                runtime.block_on(io_main(channel, identity, io_state, handshake_tx, rx));
            })
            .map_err(|error| format!("start agentd io thread: {error}"))?;

        Ok(Self {
            state,
            handshake: Mutex::new(Some(handshake_rx)),
            thread: Mutex::new(Some(thread)),
        })
    }

    fn handshake(&self) -> Result<Assignment, String> {
        let receiver = self
            .handshake
            .lock()
            .ok()
            .and_then(|mut slot| slot.take())
            .ok_or_else(|| "agentd handshake already taken".to_string())?;
        match receiver.recv_timeout(HANDSHAKE_TIMEOUT) {
            Ok(Ok(assignment)) => Ok(*assignment),
            Ok(Err(error)) => Err(error),
            Err(_) => Err("timed out waiting for an assignment from clawd".to_string()),
        }
    }

    fn finish(self) {
        self.state.refuse_all("agent worker is shutting down");
        let thread = self.thread.lock().ok().and_then(|mut slot| slot.take());
        if let Some(thread) = thread {
            // The io task ends on the result frame it just wrote; a
            // stuck peer must not keep the worker alive.
            let deadline = std::time::Instant::now() + IO_SHUTDOWN_TIMEOUT;
            while !thread.is_finished() && std::time::Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(10));
            }
            if thread.is_finished() {
                let _ = thread.join();
            }
        }
    }
}

async fn io_main(
    channel: std::os::unix::net::UnixStream,
    identity: LocalIdentity,
    state: Arc<ChannelState>,
    handshake: SyncSender<Result<Box<Assignment>, String>>,
    mut outbound: mpsc::UnboundedReceiver<WorkerFrame>,
) {
    let channel = match tokio::net::UnixStream::from_std(channel) {
        Ok(channel) => channel,
        Err(error) => {
            let _ = handshake.send(Err(format!("register agentd channel: {error}")));
            return;
        }
    };
    let (reader, mut writer) = channel.into_split();
    let mut frames = FrameReader::new(BufReader::new(reader));

    let assignment = match receive_assignment(&mut frames, &identity).await {
        Ok(assignment) => assignment,
        Err(error) => {
            let _ = handshake.send(Err(error));
            return;
        }
    };

    let hello = WorkerFrame::Hello(Box::new(WorkerHello {
        protocol: protocol::PROTOCOL_VERSION,
        grant: assignment.grant.clone(),
        pid: identity.pid,
        start_time_ticks: identity.start_time_ticks,
        uid: identity.uid,
        euid: identity.euid,
        gid: identity.gid,
        egid: identity.egid,
        supplementary_groups: identity.groups.clone(),
        no_new_privs: identity.no_new_privs,
    }));
    let encoded = match protocol::encode(&hello) {
        Ok(encoded) => encoded,
        Err(error) => {
            let _ = handshake.send(Err(error));
            return;
        }
    };
    if let Err(error) = writer.write_all(encoded.as_bytes()).await {
        let _ = handshake.send(Err(format!("write agentd handshake: {error}")));
        return;
    }
    let task_id = assignment.job.id.clone();
    if handshake.send(Ok(assignment)).is_err() {
        return;
    }

    let control_state = state.clone();
    let control = tokio::spawn(async move {
        watch_control(frames, task_id, control_state).await;
    });

    while let Some(frame) = outbound.recv().await {
        // The result is terminal: the sinks and the audit hook keep
        // sender handles alive (the hook registry is global), so the
        // pump ends on the frame rather than on the channel closing.
        let terminal = matches!(frame, WorkerFrame::Result { .. });
        let Ok(encoded) = protocol::encode(&frame) else {
            continue;
        };
        if writer.write_all(encoded.as_bytes()).await.is_err() {
            break;
        }
        let _ = writer.flush().await;
        if terminal {
            break;
        }
    }
    control.abort();
    state.refuse_all("agent worker channel closed");
    let _ = writer.shutdown().await;
}

async fn receive_assignment<R>(
    frames: &mut FrameReader<R>,
    identity: &LocalIdentity,
) -> Result<Box<Assignment>, String>
where
    R: tokio::io::AsyncBufRead + Unpin,
{
    let assignment = match frames.next_frame::<BrokerFrame>().await? {
        Some(BrokerFrame::Assign(assignment)) => assignment,
        Some(_) | None => return Err("expected an assignment from clawd".to_string()),
    };
    if assignment.protocol != protocol::PROTOCOL_VERSION {
        return Err(format!(
            "agentd protocol mismatch: clawd speaks v{}, claw-agentd speaks v{}; \
             reinstall so both come from the same build",
            assignment.protocol,
            protocol::PROTOCOL_VERSION
        ));
    }
    assignment
        .grant
        .validate_for_self(
            super::grant::now_ms(),
            identity.uid,
            identity.pid,
            identity.start_time_ticks,
        )
        .map_err(|error| error.to_string())?;
    if assignment.grant.claims.task_id != assignment.job.id {
        return Err("agentd grant does not cover the assigned task".to_string());
    }
    if assignment.grant.claims.session_id != assignment.job.session_id {
        return Err("agentd grant does not cover the assigned session".to_string());
    }
    let session_client = assignment
        .session
        .as_ref()
        .map(|session| session.client)
        .unwrap_or_default();
    if assignment.grant.claims.client != session_client {
        return Err("agentd grant does not cover the assigned session client".to_string());
    }
    if assignment.grant.claims.presence != assignment.presence {
        return Err("agentd grant does not cover the assigned presence lease".to_string());
    }
    let capability_generation = assignment
        .session
        .as_ref()
        .and_then(|session| session.caps.as_ref())
        .map(crate::agent::tools::exposure::capability_generation)
        .unwrap_or_else(|| {
            crate::agent::tools::exposure::capability_generation(&crate::caps::CapSet::new())
        });
    if assignment.grant.claims.capability_generation != capability_generation {
        return Err("agentd grant does not cover the assigned capability generation".to_string());
    }
    if !assignment
        .grant
        .claims
        .allows_route(protocol::ROUTE_APPROVAL)
    {
        // Without the route there is no way to reach consent, so every
        // gated tool would fail with an unexplained denial. Refusing
        // here makes that a named startup error instead.
        return Err("agentd grant does not allow permission mediation".to_string());
    }
    if assignment.job.owner_uid != identity.uid {
        return Err(format!(
            "assignment names owner uid {} but this worker runs as {}",
            assignment.job.owner_uid, identity.uid
        ));
    }
    identity.require_expected_identity(assignment.job.owner_uid)?;
    Ok(assignment)
}

/// Broker frames arriving mid-run. Cancellation is cooperative: the
/// interrupt is re-signalled until the loop unwinds, matching what the
/// in-process worker did, and the supervisor still terminates the
/// worker's process group if it ignores that.
async fn watch_control<R>(mut frames: FrameReader<R>, task_id: String, state: Arc<ChannelState>)
where
    R: tokio::io::AsyncBufRead + Unpin,
{
    loop {
        match frames.next_frame::<BrokerFrame>().await {
            Ok(Some(BrokerFrame::ApprovalReply {
                correlation_id,
                reply,
            })) => {
                state.deliver(correlation_id, reply);
            }
            Ok(Some(BrokerFrame::Cancel { task_id: target })) if target == task_id => break,
            Ok(Some(BrokerFrame::Shutdown)) => break,
            Ok(Some(_)) => continue,
            Ok(None) | Err(_) => {
                state.refuse_all("clawd closed the agent worker channel");
                return;
            }
        }
    }
    state.cancelled.store(true, Ordering::SeqCst);
    state.refuse_all("agent task was cancelled");
    loop {
        crate::agent::runtime::interrupt::signal(&task_id);
        tokio::time::sleep(INTERRUPT_RETRY).await;
    }
}

// ---------------------------------------------------------------------------
// Permission mediation
// ---------------------------------------------------------------------------

#[derive(Debug)]
struct ChannelApprovalGateway {
    task_id: String,
    state: Arc<ChannelState>,
}

impl ChannelApprovalGateway {
    /// Send one ask and block this thread for the broker's answer.
    ///
    /// Safe to block: channel I/O runs on its own thread, so the reader
    /// that delivers the reply cannot be starved by the agent runtime,
    /// however many of its threads are waiting here.
    fn ask(&self, ask: ApprovalAsk) -> Result<ApprovalReply, String> {
        if self.state.cancelled.load(Ordering::SeqCst) {
            return Err("agent task was cancelled".to_string());
        }
        let used = self.state.asks_used.fetch_add(1, Ordering::SeqCst);
        if used >= protocol::MAX_APPROVAL_ASKS {
            return Err(format!(
                "agent task exceeded its permission-mediation budget of {}",
                protocol::MAX_APPROVAL_ASKS
            ));
        }
        let correlation_id = self.state.next_correlation.fetch_add(1, Ordering::SeqCst);
        let waiter = self.state.register(correlation_id);
        if self
            .state
            .tx
            .send(WorkerFrame::Approval {
                task_id: self.task_id.clone(),
                correlation_id,
                ask,
            })
            .is_err()
        {
            self.state.forget(correlation_id);
            return Err("agent worker lost its supervisor channel".to_string());
        }
        match waiter.recv_timeout(APPROVAL_TIMEOUT) {
            Ok(reply) => Ok(reply),
            Err(_) => {
                self.state.forget(correlation_id);
                Err("timed out waiting for clawd to mediate the permission".to_string())
            }
        }
    }
}

impl ApprovalGateway for ChannelApprovalGateway {
    fn consume(&self, verb: Verb, scope: &Scope) -> Result<bool, String> {
        match self.ask(ApprovalAsk::Consume {
            verb: verb.as_str().to_string(),
            scope: scope.clone(),
        })? {
            ApprovalReply::Granted => Ok(true),
            ApprovalReply::Pending { .. } => Ok(false),
            ApprovalReply::Refused { message } => Err(message),
        }
    }

    fn request(&self, verb: Verb, scope: &Scope) -> Result<PendingApproval, String> {
        match self.ask(ApprovalAsk::Request {
            verb: verb.as_str().to_string(),
            scope: scope.clone(),
        })? {
            // A grant approved between the check and the ask is reported
            // as a pending request with no id; the retry spends it
            // through `consume`.
            ApprovalReply::Granted => Ok(PendingApproval { request_id: None }),
            ApprovalReply::Pending { request_id } => Ok(PendingApproval { request_id }),
            ApprovalReply::Refused { message } => Err(message),
        }
    }
}

// ---------------------------------------------------------------------------
// Execution
// ---------------------------------------------------------------------------

async fn execute(
    assignment: Assignment,
    tx: UnboundedSender<WorkerFrame>,
    cancelled: Arc<AtomicBool>,
) -> WorkerOutcome {
    let job = assignment.job;
    let task_id = job.id.clone();

    hooks::global_registry().register(Arc::new(WorkerAuditHook {
        task_id: task_id.clone(),
        tx: tx.clone(),
    }));

    let stream_sink: Arc<dyn StreamSink> = Arc::new(ChannelStreamSink {
        task_id: task_id.clone(),
        tx: tx.clone(),
    });
    let progress_sink: Arc<dyn ProgressSink> = Arc::new(ChannelProgressSink {
        task_id: task_id.clone(),
        tx: tx.clone(),
    });

    let request = JobExecution {
        id: task_id.clone(),
        prompt: job.prompt,
        context: job.context,
        branch_context: job.branch_context,
        session_id: job.session_id,
        max_turns: job.max_turns,
        presence: assignment.presence,
    };

    let home = std::path::PathBuf::from(&job.owner_home);
    let config = crate::config::intern_for_home(&home);
    let scoped = crate::agent::service::execute_job(request, stream_sink, progress_sink);
    let scoped = with_session(assignment.session, scoped);
    let scoped = crate::config::with_override(config, scoped);
    // The same per-owner scoping the in-process worker installed, so
    // config, credentials, consents and memory resolve inside the
    // owner's own account.
    let run = crate::paths::with_routed_job(crate::paths::with_user_override(
        job.owner_uid,
        home,
        scoped,
    ));

    tokio::pin!(run);
    let mut beat = tokio::time::interval(HEARTBEAT_INTERVAL);
    beat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let outcome = loop {
        tokio::select! {
            outcome = &mut run => break outcome,
            _ = beat.tick() => {
                if tx.send(WorkerFrame::Heartbeat { task_id: task_id.clone() }).is_err() {
                    break FinishOutcome::Error(
                        "agent worker lost its supervisor channel".to_string(),
                    );
                }
            }
        }
    };

    if cancelled.load(Ordering::SeqCst) {
        return WorkerOutcome::Cancelled;
    }
    match outcome {
        FinishOutcome::Ok {
            response,
            turns_used,
            provider,
            model,
            evidence,
            fallback,
        } => WorkerOutcome::Ok(Box::new(crate::agentd::protocol::CompletedRun {
            response,
            turns_used,
            provider,
            model,
            evidence: *evidence,
            fallback: *fallback,
        })),
        FinishOutcome::Error(message) => WorkerOutcome::Error { message },
        FinishOutcome::Cancelled => WorkerOutcome::Cancelled,
    }
}

/// Install the capability scope the *broker* derived. The worker never
/// authors its own capabilities.
async fn with_session<F>(session: Option<crate::proc::SessionInfo>, future: F) -> F::Output
where
    F: std::future::Future,
{
    match session {
        Some(session) => crate::proc::with_trusted_session_override(session, future).await,
        None => future.await,
    }
}

struct ChannelStreamSink {
    task_id: String,
    tx: UnboundedSender<WorkerFrame>,
}

impl StreamSink for ChannelStreamSink {
    fn on_event(&self, event: &StreamEvent) {
        let _ = self.tx.send(WorkerFrame::Stream {
            task_id: self.task_id.clone(),
            event: Box::new(event.clone()),
        });
    }
}

struct ChannelProgressSink {
    task_id: String,
    tx: UnboundedSender<WorkerFrame>,
}

impl ProgressSink for ChannelProgressSink {
    fn on_tool_start(&self, id: &str, name: &str, _input: &Value) {
        let _ = self.tx.send(WorkerFrame::Progress {
            task_id: self.task_id.clone(),
            progress: ProgressRecord::ToolStart {
                id: id.to_string(),
                name: name.to_string(),
            },
        });
    }

    fn on_tool_result(
        &self,
        id: &str,
        name: &str,
        ok: bool,
        latency_ms: u64,
        _bytes_returned: usize,
        _content_preview: &str,
    ) {
        let _ = self.tx.send(WorkerFrame::Progress {
            task_id: self.task_id.clone(),
            progress: ProgressRecord::ToolResult {
                id: id.to_string(),
                name: name.to_string(),
                ok,
                latency_ms,
            },
        });
    }
}

/// Forwards the same records `clawd` used to append in-process, already
/// projected through [`crate::audit_policy`], so the root-owned audit
/// trail keeps covering model-visible tool and turn activity without
/// the worker ever holding the log open.
#[derive(Debug)]
struct WorkerAuditHook {
    task_id: String,
    tx: UnboundedSender<WorkerFrame>,
}

impl WorkerAuditHook {
    fn emit(&self, record: RuntimeAuditRecord) {
        let _ = self.tx.send(WorkerFrame::Audit {
            task_id: self.task_id.clone(),
            record: Box::new(record),
        });
    }
}

impl Hook for WorkerAuditHook {
    fn name(&self) -> &str {
        "agentd-worker-audit"
    }

    fn pre_tool(&self, ctx: &HookContext, tool_call: &ToolCall) -> ToolDecision {
        self.emit(RuntimeAuditRecord::ToolStarted {
            session_id: audit_policy::safe_identity(&ctx.session_id),
            turn_index: ctx.turn_index,
            tool: audit_policy::tool_facts(&tool_call.name, &tool_call.input),
            tool_use_id: audit_policy::safe_identity(&tool_call.id),
        });
        ToolDecision::Allow
    }

    fn post_tool(
        &self,
        ctx: &HookContext,
        tool_call: &ToolCall,
        result: &ToolResultSummary,
    ) -> HookOutcome {
        self.emit(RuntimeAuditRecord::ToolFinished {
            session_id: audit_policy::safe_identity(&ctx.session_id),
            turn_index: ctx.turn_index,
            tool: audit_policy::tool_facts(&tool_call.name, &tool_call.input),
            tool_use_id: audit_policy::safe_identity(&tool_call.id),
            success: result.success,
            latency_ms: result.latency_ms,
            bytes_returned: result.bytes_returned,
            error: audit_policy::optional_text_digest(result.error.as_deref()),
        });
        HookOutcome::Continue
    }

    fn post_turn(&self, ctx: &HookContext, summary: &TurnSummary) -> HookOutcome {
        self.emit(RuntimeAuditRecord::TurnFinished {
            session_id: audit_policy::safe_identity(&ctx.session_id),
            turn_index: ctx.turn_index,
            provider: audit_policy::safe_identity(&ctx.provider),
            model: audit_policy::safe_identity(&ctx.model),
            success: summary.success,
            latency_ms: summary.latency_ms,
            input_tokens: summary.input_tokens,
            output_tokens: summary.output_tokens,
            cache_read_tokens: summary.cache_read_tokens,
            cache_write_tokens: summary.cache_write_tokens,
            tool_calls_made: summary.tool_calls_made,
            stop_reason: summary.stop_reason.clone(),
            error: audit_policy::optional_text_digest(summary.error.as_deref()),
        });
        HookOutcome::Continue
    }
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/agentd/worker.rs"
    ));
}
