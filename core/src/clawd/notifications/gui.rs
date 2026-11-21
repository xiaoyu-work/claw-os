//! One capability-bound presenter, not an App inbox or a second durable queue.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::os::fd::{AsFd, OwnedFd};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock, Weak};

use claw_display_control::ProcessIdentity;
use claw_notification_presentation::{Card, Preferences};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::sync::oneshot;

use crate::caps::manifest::ScopeBinding;
use crate::caps::{Cap, Scope, Verb};
use crate::clawd::app_sessions::gui::Registration;
use crate::clawd::authority::Decision;
use crate::clawd::client_identity::ClientIdentity;
use crate::clawd::protocol::BrokerError;
use crate::clawd::wire::bounded::{Text, Token};
use crate::notifications::{
    self, DeliveryChannel, DeliveryResult, DeliveryState, Notification, NotificationMutation,
    NotificationService, NotificationState,
};

pub(crate) const RESOURCE: &str = "notifications.presentation";
const HANDLES: usize = 200;
const CARD_BYTES: usize = 512 * 1024;
const RETAINED_BYTES: usize = 16 * 1024 * 1024;
const LEASE_MS: i64 = 30_000;

static PRESENTERS: OnceLock<Mutex<HashMap<u32, Weak<Window>>>> = OnceLock::new();
tokio::task_local! { static WINDOW: Arc<Window>; }

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Exchange {
    session: Token,
    #[serde(default)]
    event: Option<Feedback>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum PanelRequest {
    Snapshot {
        #[serde(default)]
        after: u32,
        #[serde(default)]
        if_revision: Option<u64>,
        #[serde(default)]
        generation: Option<Token<32>>,
    },
    Dismiss {
        generation: Token<32>,
        handle: u32,
    },
    Activate {
        generation: Token<32>,
        handle: u32,
        action_id: Text<4096>,
    },
    SetPreferences {
        generation: Token<32>,
        preferences: Preferences,
    },
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum Feedback {
    Presented {
        handle: u32,
        token: Token<32>,
        card: Option<Text<CARD_BYTES>>,
    },
    Closed {
        handle: u32,
        token: Token<32>,
        reason: CloseReason,
    },
    Activated {
        handle: u32,
        token: Token<32>,
        action: Text<4096>,
    },
    Failed {
        handle: u32,
        token: Token<32>,
    },
    Preferences {
        preferences: Preferences,
        request_id: Option<Token<32>>,
    },
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
enum CloseReason {
    Expired,
    Dismissed,
    Closed,
}

impl Feedback {
    fn validate(&self) -> Result<Option<Card>, BrokerError> {
        let pair = match self {
            Self::Presented {
                handle,
                token,
                card,
            } => {
                let card = card
                    .as_ref()
                    .map(|payload| Card::from_json(payload.as_str()))
                    .transpose()
                    .map_err(|_| BrokerError::execution("invalid bounded presentation card"))?;
                if card.as_ref().is_some_and(|card| card.id != *handle) {
                    return Err(BrokerError::execution("presentation handle mismatch"));
                }
                validate_token(token.as_str())?;
                claw_notification_presentation::validate_handle(*handle)
                    .map_err(|_| BrokerError::execution("invalid presentation handle"))?;
                return Ok(card);
            }
            Self::Closed { handle, token, .. }
            | Self::Activated { handle, token, .. }
            | Self::Failed { handle, token } => Some((*handle, token.as_str())),
            Self::Preferences {
                preferences,
                request_id,
            } => {
                preferences
                    .validate()
                    .map_err(|_| BrokerError::execution("invalid presentation preferences"))?;
                if let Some(id) = request_id {
                    validate_token(id.as_str())?;
                }
                None
            }
        };
        if let Some((handle, token)) = pair {
            validate_token(token)?;
            claw_notification_presentation::validate_handle(handle)
                .map_err(|_| BrokerError::execution("invalid presentation handle"))?;
        }
        Ok(None)
    }

    fn invokes(&self) -> bool {
        matches!(
            self,
            Self::Activated { .. }
                | Self::Closed {
                    reason: CloseReason::Dismissed,
                    ..
                }
                | Self::Preferences {
                    request_id: Some(_),
                    ..
                }
        )
    }
}

fn validate_token(value: &str) -> Result<(), BrokerError> {
    if value.len() == 32
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        Ok(())
    } else {
        Err(BrokerError::execution("invalid presentation correlation"))
    }
}

fn capability(verb: Verb) -> Cap {
    Cap::new(verb, Scope::name(RESOURCE))
}

fn declares_resource(selected: &crate::caps::manifest::GuiEntry<'_>, verb: Verb) -> bool {
    !selected.legacy_needs
        && selected.launch_needs().any(|need| {
            need.verb == verb
                && match &need.scope {
                    ScopeBinding::Fixed { scope: Scope::Wild } => verb == Verb::UI_PANEL,
                    ScopeBinding::Fixed {
                        scope: Scope::Name(name),
                    } => verb != Verb::UI_PANEL && name == RESOURCE,
                    _ => false,
                }
        })
}

struct Entry {
    record: Notification,
    token: String,
    attempts: u32,
    lease_until: i64,
    accepted: bool,
    closing: bool,
    card: Option<String>,
    replaces: Option<u32>,
}

struct PreferenceRequest {
    id: String,
    muted: bool,
    result: oneshot::Sender<Preferences>,
}

struct State {
    next: u32,
    revision: u64,
    entries: BTreeMap<u32, Entry>,
    commands: VecDeque<Value>,
    preferences: Option<Preferences>,
    preference_request: Option<PreferenceRequest>,
    last_preference_request: Option<(String, bool)>,
}

impl Default for State {
    fn default() -> Self {
        Self {
            next: 1,
            revision: 1,
            entries: BTreeMap::new(),
            commands: VecDeque::new(),
            preferences: None,
            preference_request: None,
            last_preference_request: None,
        }
    }
}

pub(crate) struct Window {
    registration: Arc<Registration>,
    process: OnceLock<ProcessIdentity>,
    process_fd: OnceLock<OwnedFd>,
    generation: String,
    stopped: AtomicBool,
    released: AtomicBool,
    state: Mutex<State>,
}

pub(crate) fn within<F: std::future::Future>(
    window: Arc<Window>,
    future: F,
) -> impl std::future::Future<Output = F::Output> {
    WINDOW.scope(window, future)
}

impl Window {
    pub(crate) fn new(registration: Arc<Registration>) -> Arc<Self> {
        Arc::new(Self {
            registration,
            process: OnceLock::new(),
            process_fd: OnceLock::new(),
            generation: uuid::Uuid::new_v4().simple().to_string(),
            stopped: AtomicBool::new(false),
            released: AtomicBool::new(false),
            state: Mutex::new(State::default()),
        })
    }

    pub(crate) fn bind(&self, process: ProcessIdentity) -> Result<(), String> {
        let descriptor = process.pidfd().map_err(|error| error.to_string())?;
        self.process_fd
            .set(descriptor)
            .map_err(|_| "notification presenter pidfd is already bound")?;
        self.process
            .set(process)
            .map_err(|_| "notification presenter is already bound".into())
    }

    pub(crate) fn stop(&self) {
        self.stopped.store(true, Ordering::Release);
    }

    fn live(&self) -> Result<(), BrokerError> {
        if self.stopped.load(Ordering::Acquire) {
            return Err(BrokerError::authorization(
                "notification presenter is retiring",
            ));
        }
        let view =
            self.registration
                .snapshot(self.process.get().ok_or_else(|| {
                    BrokerError::authorization("notification presenter is not bound")
                })?)
                .map_err(BrokerError::authorization)?;
        if !view.caps.covers(&capability(Verb::IPC_SUBSCRIBE))
            || !view.caps.covers(&Cap::unscoped(Verb::UI_PANEL))
        {
            return Err(BrokerError::authorization(
                "notification presentation subscription was revoked",
            ));
        }
        Ok(())
    }

    fn declared(&self, verb: Verb) -> Result<(), BrokerError> {
        let selected = self
            .registration
            .launch
            .manifest()
            .gui_entry(self.registration.gui_entry.as_deref())
            .map_err(BrokerError::authorization)?;
        if declares_resource(&selected, verb) {
            Ok(())
        } else {
            Err(BrokerError::authorization("this signed GUI must explicitly declare the exact notifications.presentation resource"))
        }
    }

    fn claim_presenter(self: &Arc<Self>) -> Result<(), BrokerError> {
        let mut presenters = PRESENTERS
            .get_or_init(Mutex::default)
            .lock()
            .map_err(|_| BrokerError::unavailable("notification presenter registry is poisoned"))?;
        if let Some(held) = presenters
            .get(&self.registration.owner())
            .and_then(Weak::upgrade)
        {
            if !Arc::ptr_eq(&held, self) && !held.released.load(Ordering::Acquire) {
                return Err(BrokerError::unavailable(
                    "another native presenter retains this owner's display custody",
                ));
            }
        }
        presenters.insert(self.registration.owner(), Arc::downgrade(self));
        Ok(())
    }

    fn exchange(
        self: &Arc<Self>,
        request: Exchange,
        client: &ClientIdentity,
        authority: &Decision,
        service: &dyn NotificationService,
    ) -> Result<Value, BrokerError> {
        let projected = request
            .event
            .as_ref()
            .map(Feedback::validate)
            .transpose()?
            .flatten();
        if request.session.as_str() != self.registration.session_id
            || authority.session_id() != Some(self.registration.session_id.as_str())
            || authority.owner_uid() != self.registration.owner()
            || client.require_uid().map_err(BrokerError::authorization)?
                != self.registration.owner()
            || authority.app_id() != Some(self.registration.launch.app_id())
        {
            return Err(BrokerError::authorization(
                "notification presentation is outside this exact GUI owner/App/session",
            ));
        }
        if self.stopped.load(Ordering::Acquire) {
            return Err(BrokerError::authorization(
                "notification presenter is retiring",
            ));
        }
        self.declared(Verb::IPC_SUBSCRIBE)?;
        self.declared(Verb::UI_PANEL)?;
        let view =
            self.registration
                .snapshot(self.process.get().ok_or_else(|| {
                    BrokerError::authorization("notification presenter is not bound")
                })?)
                .map_err(BrokerError::authorization)?;
        if view.id.audit_ref() != *authority.grant_ref()
            || view.generation != authority.generation()
        {
            return Err(BrokerError::authorization(
                "notification presentation grant changed",
            ));
        }
        let mut state = self
            .state
            .lock()
            .map_err(|_| BrokerError::unavailable("notification presentation state is poisoned"))?;
        let invoking = request.event.as_ref().is_some_and(Feedback::invokes)
            || !state.commands.is_empty()
            || state.preference_request.is_some();
        let mut needs = vec![
            Cap::unscoped(Verb::UI_PANEL),
            capability(Verb::IPC_SUBSCRIBE),
        ];
        if invoking {
            self.declared(Verb::IPC_INVOKE)?;
            needs.push(capability(Verb::IPC_INVOKE));
        }
        let _authorized = authority
            .require_all(&needs)
            .map_err(BrokerError::authorization)?;
        self.claim_presenter()?;
        if let Some(event) = request.event {
            apply_feedback(
                &mut state,
                event,
                projected,
                self.registration.owner(),
                service,
            )?;
        }
        let command = next_command(&mut state, self.registration.owner(), service)?;
        Ok(json!({"version":1,"command":command}))
    }

    /// Called only after the supervisor has checked process/cgroup retirement.
    pub(crate) fn release_after_death(&self) -> Result<(), String> {
        if !self.stopped.load(Ordering::Acquire) {
            return Err("notification presenter has not stopped".into());
        }
        if let Some(descriptor) = self.process_fd.get() {
            if !claw_display_control::identity::pidfd_exited(descriptor.as_fd())
                .map_err(|error| error.to_string())?
            {
                return Err("notification presenter process is still alive".into());
            }
        }
        let mut state = self
            .state
            .lock()
            .map_err(|_| "notification presentation state is poisoned")?;
        if state.entries.values().any(|entry| !entry.accepted) {
            let service = notifications::open_default().map_err(|error| error.to_string())?;
            for entry in state.entries.values().filter(|entry| !entry.accepted) {
                fail_delivery(&service, self.registration.owner(), entry)
                    .map_err(|error| error.message)?;
            }
        }
        state.entries.clear();
        state.commands.clear();
        state.preference_request.take();
        state.revision = state.revision.saturating_add(1);
        self.released.store(true, Ordering::Release);
        Ok(())
    }
}

fn storage(error: notifications::NotificationError) -> BrokerError {
    tracing::warn!(%error, "notification presentation storage failed");
    BrokerError::unavailable("Notification Service storage is unavailable")
}

fn current_delivery(
    service: &dyn NotificationService,
    owner: u32,
    entry: &Entry,
) -> Result<bool, BrokerError> {
    let record = match service.get(owner, &entry.record.id) {
        Ok(record) => record,
        Err(notifications::NotificationError::NotFound) => return Ok(false),
        Err(error) => return Err(storage(error)),
    };
    Ok(record.deliveries.iter().any(|delivery| {
        delivery.channel == DeliveryChannel::Desktop
            && delivery.attempts == entry.attempts
            && delivery.state == DeliveryState::Delivering
    }))
}

fn fail_delivery(
    service: &dyn NotificationService,
    owner: u32,
    entry: &Entry,
) -> Result<(), BrokerError> {
    if current_delivery(service, owner, entry)? {
        service
            .complete_delivery_at(
                owner,
                &entry.record.id,
                DeliveryChannel::Desktop,
                Some(notifications::DeliveryRevision {
                    occurrences: entry.record.occurrences,
                    attempts: entry.attempts,
                }),
                DeliveryResult::Failed {
                    error_code: "presentation".into(),
                    retry_at_ms: notifications::now_ms() + LEASE_MS,
                },
            )
            .map_err(storage)?;
    }
    Ok(())
}

fn apply_feedback(
    state: &mut State,
    event: Feedback,
    card: Option<Card>,
    owner: u32,
    service: &dyn NotificationService,
) -> Result<(), BrokerError> {
    let (handle, token) = match &event {
        Feedback::Presented { handle, token, .. }
        | Feedback::Closed { handle, token, .. }
        | Feedback::Activated { handle, token, .. }
        | Feedback::Failed { handle, token } => (*handle, token.as_str()),
        Feedback::Preferences {
            preferences,
            request_id,
        } => {
            if let Some(id) = request_id {
                let expected = state
                    .preference_request
                    .as_ref()
                    .filter(|request| request.id == id.as_str())
                    .map(|request| request.muted)
                    .or_else(|| {
                        state
                            .last_preference_request
                            .as_ref()
                            .filter(|(request, _)| request == id.as_str())
                            .map(|(_, muted)| *muted)
                    })
                    .ok_or_else(|| {
                        BrokerError::execution("presentation preference request is stale")
                    })?;
                if preferences.muted != expected {
                    return Err(BrokerError::execution(
                        "presenter did not apply the requested popup mute",
                    ));
                }
                if state
                    .preference_request
                    .as_ref()
                    .is_some_and(|request| request.id == id.as_str())
                {
                    if let Some(pending) = state.preference_request.take() {
                        let _ = pending.result.send(preferences.clone());
                    }
                }
                state.last_preference_request = Some((id.as_str().to_owned(), preferences.muted));
            }
            state.preferences = Some(preferences.clone());
            state.revision = state.revision.saturating_add(1);
            return Ok(());
        }
    };
    let entry = state
        .entries
        .get(&handle)
        .filter(|entry| entry.token == token)
        .ok_or_else(|| {
            BrokerError::execution("presentation handle is stale or belongs to another window")
        })?;
    match event {
        Feedback::Presented { .. } => {
            if entry.accepted {
                return Ok(());
            }
            if entry.closing
                || notifications::now_ms() >= entry.lease_until
                || !current_delivery(service, owner, entry)?
            {
                fail_delivery(service, owner, entry)?;
                state
                    .entries
                    .get_mut(&handle)
                    .expect("matched entry")
                    .closing = true;
                state.revision = state.revision.saturating_add(1);
                return Ok(());
            }
            if entry
                .record
                .presentation
                .as_ref()
                .is_some_and(|value| value.transient)
                != card.is_none()
            {
                return Err(BrokerError::execution(
                    "presentation history does not match the durable transient policy",
                ));
            }
            let payload = card
                .map(|card| card.to_json())
                .transpose()
                .map_err(|_| BrokerError::execution("invalid presentation projection"))?;
            if payload
                .as_ref()
                .is_some_and(|payload| payload.len() > CARD_BYTES)
            {
                return Err(BrokerError::execution(
                    "presentation projection exceeds the transport limit",
                ));
            }
            let completed = service
                .complete_delivery_at(
                    owner,
                    &entry.record.id,
                    DeliveryChannel::Desktop,
                    Some(notifications::DeliveryRevision {
                        occurrences: entry.record.occurrences,
                        attempts: entry.attempts,
                    }),
                    DeliveryResult::Delivered,
                )
                .map_err(storage)?;
            if completed.is_none() {
                state
                    .entries
                    .get_mut(&handle)
                    .expect("matched entry")
                    .closing = true;
                state.revision = state.revision.saturating_add(1);
                return Ok(());
            }
            let entry = state.entries.get_mut(&handle).expect("matched entry");
            entry.accepted = true;
            entry.card = payload;
            while state
                .entries
                .values()
                .filter_map(|entry| entry.card.as_ref())
                .map(String::len)
                .sum::<usize>()
                > RETAINED_BYTES
            {
                let retired = state
                    .entries
                    .iter_mut()
                    .find(|(id, entry)| **id != handle && entry.card.is_some())
                    .expect("one bounded card fits the retained presentation limit");
                retired.1.card.take();
                retired.1.closing = true;
            }
        }
        Feedback::Closed { reason, .. } => {
            if matches!(reason, CloseReason::Dismissed) {
                if let Some(record) = service
                    .mutate_at(
                        owner,
                        &entry.record.id,
                        Some(entry.record.occurrences),
                        NotificationMutation::Dismiss,
                    )
                    .map_err(storage)?
                {
                    super::super::system_journal::record_notification_event(
                        "notification.state-changed",
                        &record,
                    );
                }
            }
            state.entries.remove(&handle);
        }
        Feedback::Activated { action, .. } => {
            if action.as_str() != "default" || !entry.accepted || entry.closing {
                return Err(BrokerError::execution("presentation action is not active"));
            }
            if let Some(record) = service
                .mutate_at(
                    owner,
                    &entry.record.id,
                    Some(entry.record.occurrences),
                    NotificationMutation::Acknowledge,
                )
                .map_err(storage)?
            {
                super::super::system_journal::record_notification_event(
                    "notification.state-changed",
                    &record,
                );
            }
            state
                .entries
                .get_mut(&handle)
                .expect("matched entry")
                .closing = true;
        }
        Feedback::Failed { .. } => {
            fail_delivery(service, owner, entry)?;
            state
                .entries
                .get_mut(&handle)
                .expect("matched entry")
                .closing = true;
        }
        Feedback::Preferences { .. } => unreachable!(),
    }
    state.revision = state.revision.saturating_add(1);
    Ok(())
}

fn next_command(
    state: &mut State,
    owner: u32,
    service: &dyn NotificationService,
) -> Result<Value, BrokerError> {
    for (handle, entry) in &mut state.entries {
        let record = match service.get(owner, &entry.record.id) {
            Ok(record) => Some(record),
            Err(notifications::NotificationError::NotFound) => None,
            Err(error) => return Err(storage(error)),
        };
        let terminal = record.as_ref().is_none_or(|record| {
            matches!(
                record.state,
                NotificationState::Dismissed | NotificationState::Acknowledged
            ) || record
                .expires_at_ms
                .is_some_and(|until| until <= notifications::now_ms())
        });
        if terminal || (!entry.accepted && notifications::now_ms() >= entry.lease_until) {
            if !terminal && !entry.closing {
                fail_delivery(service, owner, entry)?;
            }
            entry.closing = true;
        }
        if entry.closing {
            return Ok(json!({"kind":"close","handle":handle,"token":entry.token}));
        }
    }
    if let Some(command) = state.commands.pop_front() {
        return Ok(command);
    }
    if let Some(request) = &state.preference_request {
        if !request.result.is_closed() {
            return Ok(json!({"kind":"preferences","request_id":request.id,"muted":request.muted}));
        }
        if let Some(request) = state.preference_request.take() {
            state.last_preference_request = Some((request.id, request.muted));
        }
    }
    if let Some((handle, entry)) = state.entries.iter().find(|(_, entry)| !entry.accepted) {
        return Ok(present(*handle, entry));
    }
    if state.entries.len() >= HANDLES {
        if let Some((handle, entry)) = state.entries.iter_mut().next() {
            entry.closing = true;
            return Ok(json!({"kind":"close","handle":handle,"token":entry.token}));
        }
    }
    if let Some(claim) = service
        .claim_deliveries(Some(owner), DeliveryChannel::Desktop, 1, LEASE_MS)
        .map_err(storage)?
        .into_iter()
        .next()
    {
        let previous = state
            .entries
            .iter()
            .find_map(|(id, entry)| (entry.record.id == claim.notification.id).then_some(*id));
        if let Some(previous) = previous {
            state
                .entries
                .get_mut(&previous)
                .expect("matched previous presentation")
                .closing = true;
        }
        let handle = state.next;
        state.next = state.next.checked_add(1).unwrap_or(1);
        while state.entries.contains_key(&state.next) {
            state.next = state.next.checked_add(1).unwrap_or(1);
        }
        let entry = Entry {
            record: claim.notification,
            token: uuid::Uuid::new_v4().simple().to_string(),
            attempts: claim.attempts,
            lease_until: notifications::now_ms() + LEASE_MS,
            accepted: false,
            closing: false,
            card: None,
            replaces: previous,
        };
        let command = present(handle, &entry);
        state.entries.insert(handle, entry);
        state.revision = state.revision.saturating_add(1);
        return Ok(command);
    }
    Ok(Value::Null)
}

fn present(handle: u32, entry: &Entry) -> Value {
    let record = &entry.record;
    let presentation = record.presentation.as_ref();
    json!({"kind":"present","handle":handle,"token":entry.token,"replaces":entry.replaces,
        "app_name":presentation.map_or("Yu OS Agent", |value| value.app_name.as_str()),
        "icon":presentation.map_or("com.yuos.Agent", |value| value.icon.as_str()),
        "summary":record.title,"body":record.body,
        "urgency":match record.severity { notifications::Severity::Critical | notifications::Severity::Error => 2,
            notifications::Severity::Warning => 1, _ => 0 },
        "expire_ms":presentation.map_or(if record.severity == notifications::Severity::Critical {0} else {-1}, |value| value.expire_ms),
        "transient":presentation.is_some_and(|value| value.transient)})
}

pub(crate) async fn control(
    params: Value,
    client: &ClientIdentity,
    authority: &Decision,
) -> Result<Value, BrokerError> {
    let request = serde_json::from_value(params)
        .map_err(|_| BrokerError::execution("invalid closed notification presentation exchange"))?;
    let window = WINDOW.try_with(Arc::clone).map_err(|_| {
        BrokerError::authorization(
            "notification presentation requires the exact Root-held private GUI endpoint",
        )
    })?;
    let service = notifications::open_default().map_err(storage)?;
    window.exchange(request, client, authority, &service)
}

pub(crate) fn cli(mut params: serde_json::Map<String, Value>) -> Result<Value, String> {
    if params.contains_key("session") {
        return Err("notification presentation derives its session from the bound Host".into());
    }
    params.insert(
        "session".into(),
        json!(std::env::var("COS_SESSION")
            .map_err(|_| "notification presentation requires its bound GUI session")?),
    );
    let request: Exchange = serde_json::from_value(Value::Object(params))
        .map_err(|_| "invalid closed notification presentation exchange")?;
    if let Some(event) = &request.event {
        event.validate().map_err(|error| error.message)?;
    }
    let response =
        crate::clawd::client::request_gui_resource_blocking(crate::clawd::protocol::Request::new(
            crate::clawd::routes::Command::parse("gui.notification.exchange")
                .ok_or("Root notification presentation route is unavailable")?,
            serde_json::to_value(request).map_err(|error| error.to_string())?,
        ))
        .map_err(|error| format!("notification presentation exchange is unconfirmed: {error}"))?;
    if response.ok {
        response
            .result
            .ok_or_else(|| "notification presentation reply is missing".into())
    } else {
        Err(response
            .error
            .map(|error| error.message)
            .unwrap_or_else(|| "notification presentation was refused".into()))
    }
}

pub(super) fn presenter(owner: u32) -> Result<Arc<Window>, BrokerError> {
    let window = PRESENTERS
        .get_or_init(Mutex::default)
        .lock()
        .map_err(|_| BrokerError::unavailable("notification presenter registry is poisoned"))?
        .get(&owner)
        .and_then(Weak::upgrade)
        .ok_or_else(|| {
            BrokerError::unavailable("no authorized native notification presenter is running")
        })?;
    if window.stopped.load(Ordering::Acquire) || window.released.load(Ordering::Acquire) {
        return Err(BrokerError::unavailable(
            "native notification presenter has retired",
        ));
    }
    window.live()?;
    Ok(window)
}

pub(super) fn retains_desktop(owner: u32) -> Result<bool, String> {
    let presenters = PRESENTERS
        .get_or_init(Mutex::default)
        .lock()
        .map_err(|_| "notification presenter registry is poisoned")?;
    Ok(presenters
        .get(&owner)
        .and_then(Weak::upgrade)
        .is_some_and(|window| !window.released.load(Ordering::Acquire)))
}

pub(crate) async fn panel_control(
    params: Value,
    client: &ClientIdentity,
) -> Result<Value, BrokerError> {
    let request: PanelRequest = serde_json::from_value(params)
        .map_err(|_| BrokerError::execution("invalid closed notification panel request"))?;
    let owner = client.require_uid().map_err(BrokerError::authorization)?;
    let window = presenter(owner)?;
    let generation = match &request {
        PanelRequest::Snapshot { generation, .. } => generation.as_ref(),
        PanelRequest::Dismiss { generation, .. }
        | PanelRequest::Activate { generation, .. }
        | PanelRequest::SetPreferences { generation, .. } => Some(generation),
    };
    if generation.is_some_and(|generation| generation.as_str() != window.generation) {
        return Err(BrokerError::execution(
            "the notification panel's presenter has changed",
        ));
    }
    if !matches!(&request, PanelRequest::Snapshot { .. }) {
        window.declared(Verb::IPC_INVOKE)?;
        let view = window.registration.snapshot(window.process.get()
            .ok_or_else(|| BrokerError::unavailable("native notification presenter is not bound"))?)
            .map_err(BrokerError::authorization)?;
        if !view.caps.covers(&capability(Verb::IPC_INVOKE)) {
            return Err(BrokerError::authorization("native notification controls are not authorized"));
        }
    }
    let completion = {
        let mut state = window
            .state
            .lock()
            .map_err(|_| BrokerError::unavailable("notification presentation state is poisoned"))?;
        match request {
            PanelRequest::Snapshot {
                after, if_revision, ..
            } => {
                let preferences = state.preferences.as_ref().ok_or_else(|| {
                    BrokerError::unavailable("native notification preferences are not ready")
                })?;
                let unchanged = if_revision == Some(state.revision);
                let mut cards = Vec::new();
                let mut bytes = 16 * 1024;
                let mut more = false;
                if !unchanged {
                    for (_, entry) in state
                        .entries
                        .range((std::ops::Bound::Excluded(after), std::ops::Bound::Unbounded))
                    {
                        let Some(card) = entry.card.as_ref().filter(|_| !entry.closing) else {
                            continue;
                        };
                        let size = serde_json::to_string(card)
                            .map_err(|_| BrokerError::execution("invalid presentation card"))?
                            .len();
                        if bytes + size >= crate::clawd::wire::MAX_RESPONSE_BYTES
                            || cards.len() == 16
                        {
                            more = true;
                            break;
                        }
                        bytes += size;
                        cards.push(card);
                    }
                    if cards.is_empty() && more {
                        return Err(BrokerError::execution(
                            "presentation card exceeds the response limit",
                        ));
                    }
                }
                return Ok(
                    json!({"version":1,"generation":window.generation,"revision":state.revision,
                    "unchanged":unchanged,"preferences":preferences,"cards":cards,"more":more}),
                );
            }
            PanelRequest::Dismiss { handle, .. } => {
                enqueue_action(&mut state, handle, None)?;
                return Ok(json!({"queued":true}));
            }
            PanelRequest::Activate {
                handle, action_id, ..
            } => {
                enqueue_action(&mut state, handle, Some(action_id.as_str()))?;
                return Ok(json!({"queued":true}));
            }
            PanelRequest::SetPreferences { preferences, .. } => {
                preferences
                    .validate()
                    .map_err(|_| BrokerError::execution("invalid presentation preferences"))?;
                if state
                    .preference_request
                    .as_ref()
                    .is_some_and(|request| !request.result.is_closed())
                {
                    return Err(BrokerError::unavailable(
                        "a native popup preference update is already pending",
                    ));
                }
                let (result, completion) = oneshot::channel();
                if let Some(previous) = state.preference_request.take() {
                    state.last_preference_request = Some((previous.id, previous.muted));
                }
                state.preference_request = Some(PreferenceRequest {
                    id: uuid::Uuid::new_v4().simple().to_string(),
                    muted: preferences.muted,
                    result,
                });
                completion
            }
        }
    };
    let preferences = tokio::time::timeout(std::time::Duration::from_secs(2), completion)
        .await
        .map_err(|_| {
            BrokerError::indeterminate(
                "native popup preference acknowledgement timed out; refresh before retrying",
            )
        })?
        .map_err(|_| BrokerError::unavailable("native popup preference update was retired"))?;
    window.live()?;
    Ok(json!({"preferences":preferences}))
}

fn enqueue_action(state: &mut State, handle: u32, action: Option<&str>) -> Result<(), BrokerError> {
    let entry = state
        .entries
        .get(&handle)
        .filter(|entry| entry.accepted && !entry.closing && entry.card.is_some())
        .ok_or_else(|| BrokerError::execution("notification panel handle is no longer active"))?;
    if state.commands.len() >= 16 {
        return Err(BrokerError::unavailable(
            "notification panel controls are busy",
        ));
    }
    let command = if let Some(action) = action {
        if action != "default" {
            return Err(BrokerError::execution(
                "notification action is not declared",
            ));
        }
        json!({"kind":"activate","handle":handle,"token":entry.token,"action":action})
    } else {
        json!({"kind":"dismiss","handle":handle,"token":entry.token})
    };
    state.commands.push_back(command);
    Ok(())
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/clawd/notifications/gui.rs"
    ));
}
