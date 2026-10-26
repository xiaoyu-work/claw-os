//! One radio owner and one durable shared view across all presentations.

use super::super::protocol::BrokerError;
use super::{
    clip,
    files::{Source, Store},
    process, radio,
};
use base64::Engine;
use clawd_client::nearby::*;
use std::{
    collections::HashMap,
    fs::{self, OpenOptions},
    io::Write,
    os::unix::fs::OpenOptionsExt,
    sync::OnceLock,
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, BufReader},
    sync::{mpsc, oneshot, watch, Mutex},
    time::Instant,
};

struct Call {
    action: NearbyAction,
    reply: oneshot::Sender<Result<NearbyView, BrokerError>>,
}
struct Live {
    owner: u32,
    id: String,
    commands: mpsc::Sender<Call>,
    view: watch::Receiver<NearbyView>,
    targets: watch::Receiver<HashMap<String, Target>>,
}
struct Window {
    duration: u32,
    deadline: Instant,
}
fn active() -> &'static Mutex<Option<Live>> {
    static ACTIVE: OnceLock<Mutex<Option<Live>>> = OnceLock::new();
    ACTIVE.get_or_init(|| Mutex::new(None))
}
fn uploads_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}
fn now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

pub(super) async fn preflight_open(expected: RadioMode, allow: bool) -> Result<(), String> {
    if active().lock().await.is_some() {
        return Err("Nearby radio is already in use.".into());
    }
    if expected.disrupts_wifi() && !allow {
        return Err("Explicitly acknowledge Wi-Fi interruption before starting this mode.".into());
    }
    let probe = radio::probe().await;
    if !probe.available || probe.mode != Some(expected) {
        return Err(format!(
            "Radio is unavailable or its mode changed: {}",
            probe.diagnostics.join("; ")
        ));
    }
    Ok(())
}

pub(super) async fn send_address(owner: u32, session: &str, peer: &str) -> Result<String, String> {
    let guard = active().lock().await;
    let live = guard
        .as_ref()
        .filter(|live| live.owner == owner && live.id == session)
        .ok_or("Sharing window changed; refresh.")?;
    let view = live.view.borrow();
    let state = view
        .session
        .as_ref()
        .filter(|state| state.phase == SessionPhase::Ready)
        .ok_or("Sharing transport is not ready.")?;
    if !state.peers.iter().any(|item| item.id == peer) {
        return Err("Recipient is not currently discovered.".into());
    }
    let result = live
        .targets
        .borrow()
        .get(peer)
        .map(|target| target.address.clone())
        .ok_or("Recipient expired; discover it again.".into());
    result
}

pub(super) async fn status(owner: u32) -> Result<NearbyView, String> {
    let store = Store::open(owner)?;
    let uploads = read_uploads(&store).await?;
    let guard = active().lock().await;
    if let Some(live) = guard.as_ref().filter(|live| live.owner == owner) {
        let mut view = live.view.borrow().clone();
        drop(guard);
        view.uploads = uploads;
        view.validate(owner)?;
        return Ok(view);
    }
    drop(guard);
    let probe = radio::probe().await;
    let guard = active().lock().await;
    if let Some(live) = guard.as_ref().filter(|live| live.owner == owner) {
        let mut view = live.view.borrow().clone();
        view.uploads = uploads;
        view.validate(owner)?;
        return Ok(view);
    }
    store.recover_sessions()?;
    let mut view = store
        .load()?
        .unwrap_or_else(|| empty_view(&store, probe.clone()));
    if let Some(session) = &mut view.session {
        if matches!(
            session.phase,
            SessionPhase::Starting | SessionPhase::Ready | SessionPhase::Stopping
        ) {
            end(session,SessionPhase::Interrupted,"The backend restarted or disappeared. Inspect any remote result; no transfer is replayed.");
        }
    }
    view.probe = probe;
    view.uploads = uploads;
    store.save(&view)?;
    Ok(view)
}

fn empty_view(store: &Store, probe: RadioProbe) -> NearbyView {
    NearbyView {
        schema: SCHEMA,
        owner_uid: store.owner,
        probe,
        session: None,
        inbox: store.inbox().to_string_lossy().into_owned(),
        uploads: Vec::new(),
    }
}

async fn upload_view(store: &Store) -> Result<NearbyView, String> {
    let guard = active().lock().await;
    let mut view = if let Some(live) = guard.as_ref().filter(|live| live.owner == store.owner) {
        live.view.borrow().clone()
    } else if let Some(mut saved) = store.load()? {
        if let Some(session) = saved.session.as_mut().filter(|session| {
            matches!(
                session.phase,
                SessionPhase::Starting | SessionPhase::Ready | SessionPhase::Stopping
            )
        }) {
            end(
                session,
                SessionPhase::Interrupted,
                "Backend interrupted; no transfer is replayed.",
            );
        }
        saved
    } else {
        drop(guard);
        let view = empty_view(store, radio::probe().await);
        let guard = active().lock().await;
        if let Some(live) = guard.as_ref().filter(|live| live.owner == store.owner) {
            live.view.borrow().clone()
        } else {
            store.save(&view)?;
            view
        }
    };
    view.uploads = store.uploads()?;
    view.validate(store.owner)?;
    Ok(view)
}

async fn read_uploads(store: &Store) -> Result<Vec<UploadView>, String> {
    let _guard = uploads_lock().lock().await;
    store.recover_uploads()?;
    store.uploads()
}

pub(super) async fn validate_upload_selection(store: &Store, ids: &[String]) -> Result<(), String> {
    let _guard = uploads_lock().lock().await;
    store.upload_sources(ids).map(|_| ())
}

pub(super) async fn control(
    store: Store,
    action: NearbyAction,
) -> Result<NearbyResponse, BrokerError> {
    let owner = store.owner;
    match &action {
        NearbyAction::UploadBegin { .. }
        | NearbyAction::UploadChunk { .. }
        | NearbyAction::UploadFinish { .. }
        | NearbyAction::UploadDelete { .. } => {
            let _guard = uploads_lock().lock().await;
            store.recover_uploads()?;
            let upload = upload(&store, &action)?;
            let view = upload_view(&store).await?;
            Ok(NearbyResponse { view, upload })
        }
        NearbyAction::Open {
            duration_secs,
            receive,
            expected_mode,
            allow_disruption,
        } => {
            let probe = radio::probe().await;
            if !probe.available || probe.mode != Some(*expected_mode) {
                return Err(format!(
                    "Radio availability or mode changed; refresh before activating: {}",
                    probe.diagnostics.join("; ")
                )
                .into());
            }
            if expected_mode.disrupts_wifi() && !allow_disruption {
                return Err(BrokerError::execution("This radio mode disconnects Wi-Fi. Explicitly acknowledge the interruption before starting."));
            }
            let mut guard = active().lock().await;
            if guard.is_some() {
                return Err(BrokerError::execution(
                    "The nearby radio is already in use. Refresh or stop your own window first.",
                ));
            }
            let id = uuid::Uuid::new_v4().to_string();
            store.recover_sessions()?;
            store.session(&id)?;
            let expires = (chrono::Utc::now() + chrono::Duration::seconds((*duration_secs).into()))
                .to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
            let view = NearbyView {
                schema: SCHEMA,
                owner_uid: owner,
                probe,
                session: Some(NearbySession {
                    id: id.clone(),
                    phase: SessionPhase::Starting,
                    mode: *expected_mode,
                    receive: *receive,
                    expires_at: expires,
                    peers: Vec::new(),
                    offers: Vec::new(),
                    discovering: false,
                    transfers: Vec::new(),
                    message: None,
                }),
                inbox: store.inbox().to_string_lossy().into_owned(),
                uploads: store.uploads()?,
            };
            store.save(&view)?;
            let (commands, calls) = mpsc::channel(16);
            let (updates, subscription) = watch::channel(view.clone());
            let (target_updates, target_subscription) = watch::channel(HashMap::new());
            *guard = Some(Live {
                owner,
                id: id.clone(),
                commands,
                view: subscription,
                targets: target_subscription,
            });
            let window = Window {
                duration: *duration_secs,
                deadline: Instant::now() + Duration::from_secs((*duration_secs).into()),
            };
            let initial = view.clone();
            tokio::spawn(async move {
                let result = run(&store, initial, calls, &updates, target_updates, window).await;
                if let Err(error) = result {
                    let mut failed = updates.borrow().clone();
                    if let Some(session) = &mut failed.session {
                        end(session, SessionPhase::Failed, &clip(&error, 2048));
                    }
                    if let Err(persist) = store.save(&failed) {
                        tracing::error!(owner_uid=owner,%persist,"nearby failure state could not be persisted");
                    }
                    updates.send_replace(failed);
                }
                let mut active = active().lock().await;
                if active.as_ref().is_some_and(|live| live.id == id) {
                    *active = None;
                }
            });
            Ok(NearbyResponse { view, upload: None })
        }
        _ => {
            let guard = active().lock().await;
            let live = guard
                .as_ref()
                .filter(|live| live.owner == owner)
                .ok_or_else(|| {
                    BrokerError::execution("No active nearby window for this owner; refresh.")
                })?;
            let expected = match &action {
                NearbyAction::Discover { session_id }
                | NearbyAction::Send { session_id, .. }
                | NearbyAction::Decide { session_id, .. }
                | NearbyAction::Stop { session_id }
                | NearbyAction::Cancel { session_id, .. } => session_id,
                _ => return Err(BrokerError::execution("invalid live nearby action")),
            };
            if *expected != live.id {
                return Err(BrokerError::execution(
                    "The nearby window changed; do not replay a stale action.",
                ));
            }
            let commands = live.commands.clone();
            drop(guard);
            let (reply, answer) = oneshot::channel();
            commands
                .send(Call { action, reply })
                .await
                .map_err(|_| BrokerError::unavailable("Nearby backend ended before admission"))?;
            let view = tokio::time::timeout(Duration::from_secs(60), answer)
                .await
                .map_err(|_| {
                    BrokerError::indeterminate(
                        "Nearby operation acknowledgement timed out; refresh before retrying.",
                    )
                })?
                .map_err(|_| {
                    BrokerError::indeterminate("Nearby backend ended before acknowledgement")
                })??;
            Ok(NearbyResponse { view, upload: None })
        }
    }
}

fn upload(store: &Store, action: &NearbyAction) -> Result<Option<UploadView>, String> {
    match action {
        NearbyAction::UploadBegin { name, bytes } => {
            let current = store.uploads()?;
            if current.len() >= MAX_FILES
                || current.iter().map(|item| item.bytes).sum::<u64>() + bytes > MAX_TOTAL_BYTES
            {
                return Err("Nearby upload quota reached; delete unused uploads first.".into());
            }
            let id = uuid::Uuid::new_v4().to_string();
            OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(store.upload_path(&id)?)
                .map_err(|e| e.to_string())?
                .sync_all()
                .map_err(|e| e.to_string())?;
            let view = UploadView {
                id,
                name: name.clone(),
                bytes: *bytes,
                written: 0,
                ready: false,
            };
            store.save_upload(&view)?;
            Ok(Some(view))
        }
        NearbyAction::UploadChunk {
            upload_id,
            offset,
            data,
        } => {
            let mut view = store.upload(upload_id)?;
            if view.ready || *offset != view.written {
                return Err("Upload offset changed; never replay an uncertain chunk.".into());
            }
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(data)
                .map_err(|_| "invalid upload base64")?;
            if bytes.is_empty()
                || bytes.len() > UPLOAD_CHUNK_BYTES
                || view.written + bytes.len() as u64 > view.bytes
            {
                return Err("Upload exceeds its declared size.".into());
            }
            let mut file = OpenOptions::new()
                .append(true)
                .custom_flags(libc::O_NOFOLLOW)
                .open(store.upload_path(upload_id)?)
                .map_err(|e| e.to_string())?;
            if file.metadata().map_err(|e| e.to_string())?.len() != view.written {
                return Err("Upload data and metadata differ; delete this upload.".into());
            }
            file.write_all(&bytes).map_err(|e| e.to_string())?;
            file.sync_all().map_err(|e| e.to_string())?;
            view.written += bytes.len() as u64;
            store.save_upload(&view)?;
            Ok(Some(view))
        }
        NearbyAction::UploadFinish { upload_id } => {
            let mut view = store.upload(upload_id)?;
            if view.bytes != view.written
                || fs::metadata(store.upload_path(upload_id)?)
                    .map_err(|e| e.to_string())?
                    .len()
                    != view.bytes
            {
                return Err("Upload is incomplete; no partial file is sent.".into());
            }
            view.ready = true;
            store.save_upload(&view)?;
            Ok(Some(view))
        }
        NearbyAction::UploadDelete { upload_id } => {
            store.delete_upload(upload_id)?;
            Ok(None)
        }
        _ => Err("not an upload operation".into()),
    }
}

#[derive(Clone)]
struct Target {
    address: String,
    flags: u32,
}
struct Pending {
    worker: u64,
    expires: Instant,
}

fn publish(
    store: &Store,
    view: &mut NearbyView,
    updates: &watch::Sender<NearbyView>,
) -> Result<(), String> {
    view.uploads = store.uploads()?;
    store.save(view)?;
    updates.send_replace(view.clone());
    Ok(())
}

fn ensure_receive_capacity(store: &Store, view: &NearbyView, id: &str) -> Result<(), String> {
    let mut candidate = view.clone();
    let session = candidate.session.as_mut().ok_or("window disappeared")?;
    let offer = session
        .offers
        .iter()
        .find(|offer| offer.id == id && offer.phase == TransferPhase::Offered)
        .ok_or("The exact offer is no longer pending.")?;
    session.transfers.push(NearbyTransfer {
        id: id.into(),
        incoming: true,
        peer: offer.sender.clone(),
        files: offer.files.clone(),
        phase: TransferPhase::Received,
        bytes: 0,
        total_bytes: None,
        received_paths: offer
            .files
            .iter()
            .map(|file| {
                store
                    .inbox()
                    .join(id)
                    .join(&file.name)
                    .to_string_lossy()
                    .into_owned()
            })
            .collect(),
        message: Some("Files received into the dedicated inbox; not opened or executed.".into()),
    });
    candidate.validate(store.owner)?;
    if serde_json::to_vec(&candidate)
        .map_err(|e| e.to_string())?
        .len()
        > MAX_FRAME_BYTES - 16 * 1024
    {
        return Err("This window cannot safely retain another receive result; stop it and open a new window.".into());
    }
    Ok(())
}

async fn run(
    store: &Store,
    mut view: NearbyView,
    mut calls: mpsc::Receiver<Call>,
    updates: &watch::Sender<NearbyView>,
    target_updates: watch::Sender<HashMap<String, Target>>,
    window: Window,
) -> Result<(), String> {
    let receive = view.session.as_ref().ok_or("window disappeared")?.receive;
    let lease = tokio::time::timeout_at(
        window.deadline,
        radio::Lease::acquire(window.duration, receive),
    )
    .await
    .map_err(|_| "Window expired during acquisition; radio recovery is not yet confirmed.")??;
    let mut advertisement = None;
    let mut stop_reply = None;
    let mut launched = false;
    let mut reaped = false;
    let result=async {
        match tokio::time::timeout(Duration::from_secs(5),super::bluetooth::Registration::start()).await {
            Ok(Ok(held))=>advertisement=Some(held),
            failed=>{
                let error=match failed {Ok(Err(error))=>error,_=>"Bluetooth advertisement setup timed out".into()};
                view.session.as_mut().ok_or("window disappeared")?.message=Some(clip(&format!(
                    "Bluetooth wake is unavailable: {error}. Receiving can continue; discovery may find only already-listening devices."
                ),2048));
                publish(store,&mut view,updates)?;
            }
        }
        let root=store.session(&view.session.as_ref().ok_or("window is missing")?.id)?;
        let mut child=process::spawn(&root,store.owner)?;
        launched=true;
        let mut input=child.stdin.take();
        let mut diagnostics=None;
        let result=async {
            let input=input.as_mut().ok_or("transport stdin unavailable")?;
            let output=child.stdout.take().ok_or("transport stdout unavailable")?;
            let stderr=child.stderr.take().ok_or("transport stderr unavailable")?;
            diagnostics=Some(tokio::spawn(async move {
                let mut bytes=Vec::new();
                stderr.take(8193).read_to_end(&mut bytes).await.map_err(|e|e.to_string())?;
                if bytes.len()>8192 {return Err("transport diagnostic limit exceeded".to_string());}
                Ok(clip(&String::from_utf8_lossy(&bytes),2048))
            }));
            process::send(input,&WorkerCommand::Start(lease.link.clone())).await?;
            let mut output=BufReader::new(output);let mut event_bytes=Vec::new();
            let mut targets:HashMap<String,Target>=HashMap::new();let mut offers:HashMap<String,Pending>=HashMap::new();
            let mut stopping=false;let mut stop_deadline=None;let mut exited=false;
            let mut ticker=tokio::time::interval(Duration::from_secs(1));
            loop {
                tokio::select! {
                    event=process::event(&mut output,&mut event_bytes)=>{
                        let Some(event)=event? else {return Err("Transport closed without checked shutdown.".to_string());};
                        if matches!(event,WorkerEvent::Stopped{}) {break;}
                        apply_event(store,&mut view,event,&mut targets,&mut offers,lease.link.index)?;
                        target_updates.send_replace(targets.clone());
                        publish(store,&mut view,updates)?;
                    }
                    call=calls.recv(),if !stopping=>{
                        let Some(call)=call else {break;};
                        match action(store,&mut view,input,call.action,&mut targets,&mut offers,window.deadline).await {
                            Ok(stop)=>{
                                publish(store,&mut view,updates)?;
                                target_updates.send_replace(targets.clone());
                                if stop {
                                    stopping=true;stop_deadline=Some(Instant::now()+Duration::from_secs(10));stop_reply=Some(call.reply);
                                } else {let _=call.reply.send(Ok(view.clone()));}
                            }
                            Err(ActionError::Rejected(error))=>{let _=call.reply.send(Err(BrokerError::execution(error)));}
                            Err(ActionError::Transport(error))=>{
                                let _=call.reply.send(Err(BrokerError::indeterminate(format!("Transport acknowledgement is uncertain; refresh, do not replay: {error}"))));
                                return Err(error);
                            }
                        }
                    }
                    _=ticker.tick()=>{
                        if Instant::now()>=window.deadline && !stopping {
                            process::send(input,&WorkerCommand::Stop{}).await?;
                            stopping=true;stop_deadline=Some(Instant::now()+Duration::from_secs(10));
                            view.session.as_mut().ok_or("window disappeared")?.phase=SessionPhase::Stopping;
                        }
                        if !stopping {lease.check_alive().await?;}
                        let session=view.session.as_mut().ok_or("window disappeared")?;
                        let mut changed=false;
                        for offer in &mut session.offers {
                            if offer.phase==TransferPhase::Offered && offers.get(&offer.id).is_some_and(|p|Instant::now()>=p.expires) {
                                offer.phase=TransferPhase::Expired;offers.remove(&offer.id);changed=true;
                            }
                        }
                        if session.discovering && !stopping {
                            process::send(input,&WorkerCommand::Discover{peers:lease.peers().await?}).await?;
                        }
                        if changed {publish(store,&mut view,updates)?;}
                    }
                    status=child.wait(),if !exited=>{
                        let status=status.map_err(|e|e.to_string())?;
                        exited=true;
                        if !status.success() {return Err(format!("Transport exited abnormally ({status})."));}
                    }
                }
                if stop_deadline.is_some_and(|deadline|Instant::now()>=deadline) {return Err("Transport shutdown timed out.".into());}
            }
            Ok::<_,String>(())
        }.await;
        drop(input);
        if result.is_err() {let _=child.start_kill();}
        let status=match tokio::time::timeout(Duration::from_secs(5),child.wait()).await {
            Ok(status)=>status.map_err(|e|e.to_string())?,
            Err(_)=>{let _=child.start_kill();child.wait().await.map_err(|e|e.to_string())?}
        };
        reaped=true;
        let diagnostics=match diagnostics {Some(task)=>task.await.map_err(|e|e.to_string())??,None=>String::new()};
        if !status.success() {return Err(format!("Transport retirement ended abnormally ({status}); {diagnostics}"));}
        result.map_err(|error|format!("{error} {diagnostics}"))
    }.await;
    let bluetooth = match advertisement {
        Some(held) => held.stop().await,
        None => Ok(()),
    };
    let restored = lease.release().await;
    let retired = if !launched || reaped {
        store.retire_session(&view.session.as_ref().ok_or("window disappeared")?.id)
    } else {
        Err("Containment retirement is unconfirmed; private staged bytes were retained.".into())
    };
    let errors: Vec<_> = [
        result.err(),
        bluetooth
            .err()
            .map(|e| format!("Bluetooth retirement is unconfirmed: {e}")),
        restored
            .err()
            .map(|e| format!("Radio recovery requires inspection: {e}")),
        retired.err(),
    ]
    .into_iter()
    .flatten()
    .collect();
    let finished = if errors.is_empty() {
        end(view.session.as_mut().ok_or("window disappeared")?,SessionPhase::Ended,
                "Window closed and radio restoration acknowledged. Interrupted sends may have remote effects.");
        publish(store, &mut view, updates)
    } else {
        let message = errors.join("; ");
        end(
            view.session.as_mut().ok_or("window disappeared")?,
            SessionPhase::Failed,
            &message,
        );
        publish(store, &mut view, updates)
            .map_err(|persist| format!("{message}; failure state persistence failed: {persist}"))?;
        Err(message)
    };
    if let Some(reply) = stop_reply {
        let _ = reply.send(
            finished
                .as_ref()
                .map(|_| view.clone())
                .map_err(|error| BrokerError::indeterminate(error.clone())),
        );
    }
    finished
}

enum ActionError {
    Rejected(String),
    Transport(String),
}
impl From<String> for ActionError {
    fn from(error: String) -> Self {
        Self::Rejected(error)
    }
}
impl From<&str> for ActionError {
    fn from(error: &str) -> Self {
        Self::Rejected(error.into())
    }
}

async fn action(
    store: &Store,
    view: &mut NearbyView,
    input: &mut tokio::process::ChildStdin,
    action: NearbyAction,
    targets: &mut HashMap<String, Target>,
    pending: &mut HashMap<String, Pending>,
    deadline: Instant,
) -> Result<bool, ActionError> {
    if Instant::now() >= deadline && !matches!(action, NearbyAction::Stop { .. }) {
        return Err("Sharing window expired; refresh before starting another.".into());
    }
    if let NearbyAction::Decide {
        offer_id,
        accept: true,
        ..
    } = &action
    {
        ensure_receive_capacity(store, view, offer_id)?;
    }
    let session = view.session.as_mut().ok_or("window disappeared")?;
    if !matches!(session.phase, SessionPhase::Ready) && !matches!(action, NearbyAction::Stop { .. })
    {
        return Err("Wait until the radio and isolated transport are ready.".into());
    }
    match action {
        NearbyAction::Discover { .. } => {
            if session.discovering {
                return Err("Discovery is already running; wait for its result.".into());
            }
            session.peers.clear();
            targets.clear();
            session.discovering = true;
            process::send(input, &WorkerCommand::Discover { peers: Vec::new() })
                .await
                .map_err(ActionError::Transport)?;
        }
        NearbyAction::Stop { .. } => {
            process::send(input, &WorkerCommand::Stop {})
                .await
                .map_err(ActionError::Transport)?;
            session.phase = SessionPhase::Stopping;
            return Ok(true);
        }
        NearbyAction::Decide {
            offer_id, accept, ..
        } => {
            if accept
                && session
                    .offers
                    .iter()
                    .any(|offer| offer.phase == TransferPhase::Accepted)
            {
                return Err(
                    "Finish the accepted incoming transfer before accepting another.".into(),
                );
            }
            let offer = session
                .offers
                .iter_mut()
                .find(|offer| offer.id == offer_id && offer.phase == TransferPhase::Offered)
                .ok_or("The offer is no longer pending.")?;
            let held = pending
                .get(&offer_id)
                .filter(|held| held.expires > Instant::now())
                .ok_or("The offer expired; no decision was applied.")?;
            if accept && session.transfers.len() >= MAX_TRANSFERS {
                return Err("Window transfer quota reached; close and open another.".into());
            }
            process::send(
                input,
                &WorkerCommand::Decide {
                    id: held.worker,
                    accept,
                },
            )
            .await
            .map_err(ActionError::Transport)?;
            offer.phase = if accept {
                TransferPhase::Accepted
            } else {
                TransferPhase::Declined
            };
            if accept {
                session.transfers.push(NearbyTransfer {
                    id: offer.id.clone(),
                    incoming: true,
                    peer: offer.sender.clone(),
                    files: offer.files.clone(),
                    phase: offer.phase,
                    bytes: 0,
                    total_bytes: None,
                    received_paths: Vec::new(),
                    message: None,
                });
            }
            if !accept {
                pending.remove(&offer_id);
            }
        }
        NearbyAction::Send {
            peer_id,
            paths,
            uploads,
            ..
        } => {
            if session.transfers.len() >= MAX_TRANSFERS
                || session
                    .transfers
                    .iter()
                    .any(|transfer| !transfer.incoming && !transfer.phase.terminal())
            {
                return Err(
                    "Another send is in flight or this window reached its transfer quota.".into(),
                );
            }
            let peer = session
                .peers
                .iter()
                .find(|peer| peer.id == peer_id)
                .ok_or("Recipient is not in this window's discovery results.")?;
            let target = targets
                .get(&peer_id)
                .ok_or("Recipient expired; discover it again.")?;
            let id = uuid::Uuid::new_v4().to_string();
            let _upload_guard = if uploads.is_empty() {
                None
            } else {
                Some(uploads_lock().lock().await)
            };
            let sources = if uploads.is_empty() {
                paths
                    .iter()
                    .map(|path| {
                        let canonical = store.source(path)?;
                        if canonical != std::path::Path::new(path) {
                            return Err(
                                "Source path changed after authorization; select it again.".into(),
                            );
                        }
                        let name = canonical
                            .file_name()
                            .and_then(|name| name.to_str())
                            .ok_or("source name must be UTF-8")?
                            .to_string();
                        Ok(Source {
                            path: canonical,
                            name,
                        })
                    })
                    .collect::<Result<Vec<_>, String>>()?
            } else {
                store.upload_sources(&uploads)?
            };
            let (assigned, files) =
                match store.snapshot(&session.id, &id, &sources, !uploads.is_empty()) {
                    Ok(snapshot) => snapshot,
                    Err(error) => {
                        store.retire_send(&session.id, &id).map_err(|retirement| {
                            format!("{error}; staging cleanup failed: {retirement}")
                        })?;
                        return Err(error.into());
                    }
                };
            drop(_upload_guard);
            if Instant::now() >= deadline {
                store.retire_send(&session.id, &id)?;
                return Err("Window expired during staging; no send was dispatched.".into());
            }
            process::send(
                input,
                &WorkerCommand::Send {
                    id: id.clone(),
                    address: target.address.clone(),
                    flags: target.flags,
                    files: assigned,
                },
            )
            .await
            .map_err(ActionError::Transport)?;
            session.transfers.push(NearbyTransfer {
                id,
                incoming: false,
                peer: peer.name.clone(),
                files,
                phase: TransferPhase::Sending,
                bytes: 0,
                total_bytes: None,
                received_paths: Vec::new(),
                message: None,
            });
        }
        NearbyAction::Cancel { transfer_id, .. } => {
            let transfer = session
                .transfers
                .iter()
                .find(|transfer| {
                    transfer.id == transfer_id && !transfer.incoming && !transfer.phase.terminal()
                })
                .ok_or("No matching in-flight send to cancel.")?;
            process::send(
                input,
                &WorkerCommand::Cancel {
                    id: transfer.id.clone(),
                },
            )
            .await
            .map_err(ActionError::Transport)?;
        }
        _ => return Err("unsupported live nearby action".into()),
    }
    Ok(false)
}

fn apply_event(
    store: &Store,
    view: &mut NearbyView,
    event: WorkerEvent,
    targets: &mut HashMap<String, Target>,
    pending: &mut HashMap<String, Pending>,
    index: u32,
) -> Result<(), String> {
    let session = view.session.as_mut().ok_or("window disappeared")?;
    match event {
        WorkerEvent::Ready {} => {
            if session.phase != SessionPhase::Starting {
                return Err("unexpected transport readiness".into());
            }
            session.phase = SessionPhase::Ready;
        }
        WorkerEvent::Peer {
            address,
            name,
            flags,
        } => {
            let parsed: std::net::SocketAddrV6 =
                address.parse().map_err(|_| "invalid discovered address")?;
            if parsed.ip().segments()[0] & 0xffc0 != 0xfe80
                || parsed.scope_id() != index
                || parsed.port() == 0
            {
                return Err("discovered peer is outside the exact radio link".into());
            }
            if !session.discovering || targets.values().any(|target| target.address == address) {
                return Ok(());
            }
            if session.peers.len() < MAX_PEERS {
                let id = uuid::Uuid::new_v4().to_string();
                session.peers.push(NearbyPeer {
                    id: id.clone(),
                    name: clip(&name, 240),
                    discovered_at: now(),
                });
                targets.insert(id, Target { address, flags });
            }
        }
        WorkerEvent::DiscoveryFinished {} => session.discovering = false,
        WorkerEvent::Offer { id, sender, files } => {
            validate_files(&files)?;
            if !session.receive
                || session.offers.len() >= MAX_OFFERS
                || pending.values().any(|held| held.worker == id)
            {
                return Err("incoming offer exceeds window policy or repeats an identity".into());
            }
            let offer_id = uuid::Uuid::new_v4().to_string();
            let expires = chrono::Utc::now() + chrono::Duration::seconds(15);
            session.offers.push(NearbyOffer {
                id: offer_id.clone(),
                sender: clip(&sender, 240),
                files,
                expires_at: expires.to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
                phase: TransferPhase::Offered,
            });
            pending.insert(
                offer_id.clone(),
                Pending {
                    worker: id,
                    expires: Instant::now() + Duration::from_secs(15),
                },
            );
            store.save(view)?;
            let mut draft=crate::notifications::NotificationDraft::new("nearby","nearby.offer",
                crate::notifications::Severity::Info,"Nearby device wants to share files",
                "Open Nearby Sharing and confirm the exact offer before it expires. Device names are unverified.")
                .dedupe(format!("nearby-offer:{offer_id}"));
            draft.expires_at_ms = Some(expires.timestamp_millis());
            if let Err(error) = crate::clawd::notifications::publish_for_owner(store.owner, draft) {
                tracing::warn!(owner_uid=store.owner,%error,"nearby offer notification failed");
            }
        }
        event @ (WorkerEvent::Withdrawn { .. } | WorkerEvent::ReceiveFailed { .. }) => {
            let (id, phase, message) = match event {
                WorkerEvent::Withdrawn { id } => (
                    id,
                    TransferPhase::Expired,
                    "The sender left or the offer expired.".to_string(),
                ),
                WorkerEvent::ReceiveFailed { id, message } => {
                    (id, TransferPhase::Failed, clip(&message, 2048))
                }
                _ => unreachable!(),
            };
            if let Some((offer_id, _)) = pending.iter().find(|(_, pending)| pending.worker == id) {
                let offer_id = offer_id.clone();
                pending.remove(&offer_id);
                if let Some(offer) = session.offers.iter_mut().find(|offer| offer.id == offer_id) {
                    offer.phase = phase;
                }
                if let Some(transfer) = session
                    .transfers
                    .iter_mut()
                    .find(|transfer| transfer.id == offer_id)
                {
                    transfer.phase = phase;
                    transfer.message = Some(message);
                }
            }
        }
        WorkerEvent::Received { id, paths } => {
            let offer_id = pending
                .iter()
                .find(|(_, pending)| pending.worker == id)
                .map(|(id, _)| id.clone())
                .ok_or("received transfer has no accepted offer")?;
            let transfer = session
                .transfers
                .iter_mut()
                .find(|transfer| {
                    transfer.id == offer_id && transfer.phase == TransferPhase::Accepted
                })
                .ok_or("incoming files were not explicitly accepted")?;
            let received = match store.import(&session.id, &offer_id, &transfer.files, &paths) {
                Ok(imported) => {
                    transfer.received_paths = imported.paths;
                    transfer.bytes = imported.bytes;
                    transfer.total_bytes = Some(imported.bytes);
                    transfer.phase = TransferPhase::Received;
                    transfer.message = Some(
                        "Files received into the dedicated inbox; not opened or executed.".into(),
                    );
                    true
                }
                Err(error) => {
                    transfer.received_paths = error.paths;
                    transfer.phase = TransferPhase::Failed;
                    transfer.message = Some(clip(
                        &format!(
                            "Import did not complete. Inspect any listed partial inbox files: {}",
                            error.message
                        ),
                        2048,
                    ));
                    false
                }
            };
            if let Some(offer) = session.offers.iter_mut().find(|offer| offer.id == offer_id) {
                offer.phase = transfer.phase;
            }
            pending.remove(&offer_id);
            store.save(view)?;
            let state = view.session.as_ref().ok_or("window disappeared")?;
            let saved = state
                .transfers
                .iter()
                .find(|transfer| transfer.id == offer_id)
                .ok_or("incoming result disappeared")?;
            super::super::audit::record_nearby_transfer(store.owner, &state.id, saved)?;
            let (event, severity, title, body) = if received {
                (
                    "nearby.received",
                    crate::notifications::Severity::Info,
                    "Nearby files received",
                    "Open Nearby Sharing to inspect the received files.",
                )
            } else {
                ("nearby.receive_failed",crate::notifications::Severity::Warning,"Nearby transfer needs inspection","Import did not complete. Inspect Nearby Sharing and any listed partial inbox files.")
            };
            let draft = crate::notifications::NotificationDraft::new(
                "nearby", event, severity, title, body,
            )
            .dedupe(format!("nearby:{offer_id}"));
            if let Err(error) = crate::clawd::notifications::publish_for_owner(store.owner, draft) {
                tracing::warn!(owner_uid=store.owner,%error,"nearby receive notification failed");
            }
            if !received {
                return Err(
                    "Incoming import did not complete; the window is closing for inspection."
                        .into(),
                );
            }
        }
        WorkerEvent::Progress { id, bytes, total } => {
            let transfer = session
                .transfers
                .iter_mut()
                .find(|transfer| transfer.id == id && !transfer.phase.terminal())
                .ok_or("progress for an unknown send")?;
            if bytes > total || total > MAX_WIRE_BYTES {
                return Err("invalid transfer progress".into());
            }
            transfer.bytes = bytes;
            transfer.total_bytes = Some(total);
        }
        WorkerEvent::Sent { id, outcome } => {
            let transfer = session
                .transfers
                .iter_mut()
                .find(|transfer| transfer.id == id && !transfer.incoming)
                .ok_or("unknown outgoing transfer")?;
            if transfer.phase.terminal() {
                return Err("outgoing outcome was already terminal; it cannot be rewritten".into());
            }
            transfer.phase = match outcome.as_str() {
                "delivered" => TransferPhase::Delivered,
                "declined" => TransferPhase::Declined,
                "interrupted" => TransferPhase::Interrupted,
                _ => TransferPhase::Failed,
            };
            transfer.message = Some(if outcome == "delivered" {
                "Receiver acknowledged the protocol transfer; this does not prove a person read the files.".into()
            } else {
                clip(&outcome, 2048)
            });
            store.retire_send(&session.id, &id)?;
            super::super::audit::record_nearby_transfer(store.owner, &session.id, transfer)?;
        }
        WorkerEvent::Error { message } => return Err(clip(&message, 2048)),
        WorkerEvent::Stopped {} => {}
    }
    Ok(())
}

fn end(session: &mut NearbySession, phase: SessionPhase, message: &str) {
    session.phase = phase;
    session.message = Some(clip(message, 2048));
    session.discovering = false;
    session.peers.clear();
    for offer in &mut session.offers {
        if !offer.phase.terminal() {
            offer.phase = TransferPhase::Expired;
        }
    }
    for transfer in &mut session.transfers {
        if !transfer.phase.terminal() {
            transfer.phase = TransferPhase::Interrupted;
            transfer.message = Some("Outcome not confirmed; no automatic retry.".into());
        }
    }
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/clawd/nearby/runtime.rs"
    ));
}
