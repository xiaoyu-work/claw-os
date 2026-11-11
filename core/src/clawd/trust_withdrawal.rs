//! Checked Root reconciliation after an owner's independent protective policy write.

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::provenance::runtime::{InstanceClass, PackageRef};
use crate::provenance::withdrawal::{OwnerPolicy, Target};
use crate::provenance::{PackageKind, VerifiedPackage};

use super::client_identity::ClientIdentity;
use super::protocol::BrokerError;
use super::state::DaemonState;

const STATE_ROOT: &str = "/var/lib/cos/trust-withdrawals";
const MAX_RECORDS: usize = 256;
const MAX_TARGETS: usize = 1024;
const MAX_RECORD_BYTES: u64 = 1024 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", try_from = "RequestBody")]
pub enum Request {
    Reconcile { target: Target },
    Status { request_id: String },
    Recover { request_id: String },
}

#[derive(Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
enum RequestBody {
    Reconcile {
        target: Target,
    },
    Status {
        request_id: super::wire::bounded::Token<32>,
    },
    Recover {
        request_id: super::wire::bounded::Token<32>,
    },
}

impl TryFrom<RequestBody> for Request {
    type Error = String;
    fn try_from(body: RequestBody) -> Result<Self, String> {
        let request = match body {
            RequestBody::Reconcile { target } => Self::Reconcile { target },
            RequestBody::Status { request_id } => Self::Status {
                request_id: request_id.as_str().to_string(),
            },
            RequestBody::Recover { request_id } => Self::Recover {
                request_id: request_id.as_str().to_string(),
            },
        };
        request.validate()?;
        Ok(request)
    }
}

impl Request {
    pub fn validate(&self) -> Result<(), String> {
        match self {
            Self::Reconcile { target } => target.validate(),
            Self::Status { request_id } | Self::Recover { request_id } => valid_id(request_id),
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct Binding {
    pub owner: u32,
    pub app: String,
    pub package: Option<PackageRef>,
    pub trust_context_uid: Option<u32>,
    pub context_generation_invalidated: bool,
}

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Phase {
    Selecting,
    Retiring,
    Indeterminate,
    Retired,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PendingApp {
    owner: u32,
    app: String,
    retired: bool,
    supporting_evidence_missing: bool,
    packages: Vec<PackageRef>,
    context_generation_closure: bool,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Pending {
    schema_version: u32,
    request_id: String,
    requester: u32,
    root_epoch: String,
    target: Target,
    source_generation: Option<u64>,
    source_fingerprint: Option<String>,
    phase: Phase,
    apps: Vec<PendingApp>,
    error: Option<String>,
}

fn epoch() -> &'static str {
    static EPOCH: OnceLock<String> = OnceLock::new();
    EPOCH.get_or_init(|| uuid::Uuid::new_v4().simple().to_string())
}

fn active() -> &'static Mutex<HashSet<String>> {
    static ACTIVE: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    ACTIVE.get_or_init(Default::default)
}

fn valid_id(id: &str) -> Result<(), String> {
    if id.len() == 32
        && id
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        Ok(())
    } else {
        Err("withdrawal request id must be 32 lowercase hexadecimal characters".into())
    }
}

fn root_exists() -> Result<bool, String> {
    match std::fs::symlink_metadata(STATE_ROOT) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(format!("Root withdrawal state is unavailable: {error}")),
        Ok(_) => {
            let metadata =
                crate::provenance::fsec::require_secure_location(Path::new(STATE_ROOT), &[0])
                    .map_err(|error| error.to_string())?;
            if !metadata.is_dir || metadata.mode & 0o077 != 0 {
                return Err("Root withdrawal state is not a protected private directory".into());
            }
            Ok(true)
        }
    }
}

fn path(id: &str) -> Result<PathBuf, String> {
    valid_id(id)?;
    Ok(Path::new(STATE_ROOT).join(format!("{id}.json")))
}

fn records() -> Result<Vec<Pending>, String> {
    if !root_exists()? {
        return Ok(Vec::new());
    }
    let mut records = Vec::new();
    for entry in std::fs::read_dir(STATE_ROOT).map_err(|error| error.to_string())? {
        let entry = entry.map_err(|error| error.to_string())?;
        let name = entry.file_name();
        let name = name
            .to_str()
            .ok_or("Root withdrawal record name is not UTF-8")?;
        if name.starts_with('.') {
            continue;
        }
        let id = name
            .strip_suffix(".json")
            .ok_or("unexpected Root withdrawal state entry")?;
        records.push(read(id)?);
        if records.len() > MAX_RECORDS {
            return Err("Root withdrawal state exceeds its bounded inventory".into());
        }
    }
    Ok(records)
}

fn read(id: &str) -> Result<Pending, String> {
    root_exists()?
        .then_some(())
        .ok_or("no protected withdrawal outcome exists")?;
    let path = path(id)?;
    let metadata = crate::provenance::fsec::require_secure_location(&path, &[0])
        .map_err(|error| error.to_string())?;
    if !metadata.is_file || metadata.nlink != 1 || metadata.size > MAX_RECORD_BYTES {
        return Err("Root withdrawal outcome is not a bounded single-link regular file".into());
    }
    #[cfg(unix)]
    let bytes = crate::provenance::fsec::DirHandle::open(Path::new(STATE_ROOT))
        .and_then(|directory| directory.open_file(&format!("{id}.json")))
        .and_then(|file| file.read_bounded(MAX_RECORD_BYTES))
        .map_err(|error| error.to_string())?;
    #[cfg(not(unix))]
    let bytes: Vec<u8> = return Err("Root withdrawal requires Linux".into());
    let pending: Pending = serde_json::from_slice(&bytes).map_err(|error| error.to_string())?;
    if pending.schema_version != 1 || pending.request_id != id || pending.apps.len() > MAX_TARGETS {
        return Err("Root withdrawal record has an invalid binding".into());
    }
    pending.target.validate()?;
    valid_id(&pending.root_epoch)?;
    if pending.apps.iter().any(|app| {
        app.app.is_empty()
            || app.app.len() > 128
            || pending.requester != 0 && app.owner != pending.requester
            || app.packages.len() > MAX_TARGETS
    }) {
        return Err("Root withdrawal target exceeds its App bound".into());
    }
    Ok(pending)
}

fn save(pending: &Pending) -> Result<(), String> {
    let bytes = serde_json::to_vec(pending).map_err(|error| error.to_string())?;
    if bytes.len() as u64 > MAX_RECORD_BYTES || pending.apps.len() > MAX_TARGETS {
        return Err("Root withdrawal outcome exceeds its persistence bound".into());
    }
    super::app_store::write_json(&path(&pending.request_id)?, pending)
}

fn visible(pending: &Pending) -> Value {
    json!({
        "request_id": pending.request_id,
        "scope": "apps_only",
        "target": pending.target,
        "root_retirement_confirmed": pending.phase == Phase::Retired,
        "no_affected_root_bindings": pending.phase == Phase::Retired && pending.apps.is_empty(),
        "state": pending.phase,
        "apps": pending.apps.iter().map(|app| json!({
            "owner_uid": app.owner, "app_id": app.app, "retired": app.retired,
            "supporting_evidence_missing": app.supporting_evidence_missing,
            "context_generation_closure": app.context_generation_closure,
        })).collect::<Vec<_>>(),
        "source_generation": pending.source_generation,
        "source_fingerprint": pending.source_fingerprint,
        "non_app_retirement": { "confirmed": false, "state": "unknown" },
        "error": pending.error,
    })
}

pub(crate) fn pending(owner: u32, app: &str) -> Result<bool, String> {
    Ok(records()?.iter().any(|record| {
        record.phase != Phase::Retired
            && record
                .apps
                .iter()
                .any(|target| target.owner == owner && target.app == app && !target.retired)
    }))
}

fn selector_applies(record: &Pending, owner: u32, package: &PackageRef) -> bool {
    (record.requester == 0 || record.requester == owner)
        && (!matches!(record.target, Target::Developer { .. }) || record.requester == 0)
        && record.target.matches(package)
}

pub(crate) fn check_package(owner: u32, package: &VerifiedPackage) -> Result<(), String> {
    package
        .assert_current(&crate::provenance::trust_store())
        .map_err(|error| error.to_string())?;
    check_reference(owner, &PackageRef::of(package))
}

fn check_reference(owner: u32, package: &PackageRef) -> Result<(), String> {
    // Keep positive verification in the existing daemon-default context.
    // These private stores can only deny; absent consumer grants never grant
    // or withdraw a developer grant supported by a different context.
    OwnerPolicy::load(0)?.check_denials(package)?;
    if owner != 0 {
        OwnerPolicy::load(owner)?.check_denials(package)?;
    }
    if records()?.iter().any(|record| {
        record.phase != Phase::Retired
            && (selector_applies(record, owner, package)
                || record
                    .apps
                    .iter()
                    .any(|app| app.owner == owner && app.app == package.id && !app.retired))
    }) {
        return Err(
            "Root App retirement is pending or indeterminate for this owner/package".into(),
        );
    }
    package.is_live(&crate::provenance::trust_store())
}

pub(crate) fn check_session(owner: u32, session: &str) -> Result<(), String> {
    #[cfg(test)]
    if !crate::test_env::root_app_backend_selected() {
        if let Some(result) = super::authority::authority().check_unit_app_session(owner, session) {
            return result;
        }
    }
    if let Some(shutdown) = crate::provenance::runtime::shutdown_for(owner, session)? {
        return Err(shutdown.reason);
    }
    let instance = crate::provenance::runtime::instance_for(owner, session)?
        .ok_or_else(|| format!(
            "no running-instance record for App session `{session}`; its package cannot be confirmed as still trusted"
        ))?;
    let package = instance
        .package
        .ok_or("Root App supporting package evidence is missing")?;
    if package.kind != PackageKind::App || instance.trust_context_uid != Some(0) {
        return Err(
            "Root App supporting trust context is missing or changed; retirement is indeterminate"
                .into(),
        );
    }
    check_reference(owner, &package)?;
    crate::provenance::runtime::assert_live_instance_now(owner, session)
}

pub(crate) async fn handle(
    state: &DaemonState,
    params: Value,
    client: &ClientIdentity,
) -> Result<Value, BrokerError> {
    let owner = client.require_uid().map_err(BrokerError::authorization)?;
    if client.extension_host.is_some() || client.process_uid() != Some(owner) {
        return Err(BrokerError::authorization(
            "withdrawal reconciliation requires a direct non-Host peer",
        ));
    }
    super::app_sessions::require_non_app_caller(client)
        .await
        .map_err(BrokerError::authorization)?;
    let pid = client
        .pid
        .ok_or_else(|| BrokerError::authorization("withdrawal peer PID is unavailable"))?;
    if client.start_time_ticks.is_none()
        || crate::proc::read_start_time_ticks_pub(pid) != client.start_time_ticks
        || crate::provenance::fsec::effective_uid() != 0
    {
        return Err(BrokerError::authorization(
            "withdrawal peer or Root broker identity changed",
        ));
    }
    let request: Request = serde_json::from_value(params)
        .map_err(|_| BrokerError::authorization("invalid closed withdrawal request"))?;
    request.validate().map_err(BrokerError::authorization)?;
    let recover = matches!(&request, Request::Recover { .. });
    match request {
        Request::Reconcile { target } => {
            let id = prepare(owner, target)?;
            run(state.clone(), id).await
        }
        Request::Status { request_id } | Request::Recover { request_id } => {
            let record = read(&request_id).map_err(BrokerError::unavailable)?;
            if record.requester != owner {
                return Err(BrokerError::authorization(
                    "withdrawal outcome belongs to another peer",
                ));
            }
            if recover {
                run(state.clone(), request_id).await
            } else {
                Ok(visible(&record))
            }
        }
    }
}

fn prepare(owner: u32, target: Target) -> Result<String, BrokerError> {
    static REQUESTS: Mutex<()> = Mutex::new(());
    let _requests = REQUESTS
        .lock()
        .map_err(|_| BrokerError::unavailable("Root withdrawal request lock failed"))?;
    if !root_exists().map_err(BrokerError::unavailable)? {
        let directory = Path::new(STATE_ROOT);
        crate::provenance::fsec::require_secure_location(
            directory.parent().ok_or_else(|| {
                BrokerError::unavailable("withdrawal state has no protected parent")
            })?,
            &[0],
        )
        .map_err(|error| BrokerError::unavailable(error.to_string()))?;
        crate::agent::util::ensure_durable_private_dir(directory)
            .map_err(|error| BrokerError::unavailable(error.to_string()))?;
        super::app_store::secure_directory(directory, 0o700).map_err(BrokerError::unavailable)?;
    }
    let existing = records().map_err(BrokerError::unavailable)?;
    if let Some(record) = existing.iter().find(|record| {
        record.requester == owner && record.target == target && record.phase != Phase::Retired
    }) {
        return Ok(record.request_id.clone());
    }
    if existing.len() >= MAX_RECORDS {
        return Err(BrokerError::unavailable(
            "Root withdrawal records are at capacity; own policy remains withdrawn",
        ));
    }
    let record = Pending {
        schema_version: 1,
        request_id: uuid::Uuid::new_v4().simple().to_string(),
        requester: owner,
        root_epoch: epoch().to_string(),
        target,
        source_generation: None,
        source_fingerprint: None,
        phase: Phase::Selecting,
        apps: Vec::new(),
        error: None,
    };
    save(&record).map_err(BrokerError::indeterminate)?;
    Ok(record.request_id)
}

struct Controller(String);
impl Drop for Controller {
    fn drop(&mut self) {
        if let Ok(mut active) = active().lock() {
            active.remove(&self.0);
        }
    }
}

async fn run(state: DaemonState, id: String) -> Result<Value, BrokerError> {
    {
        let mut active = active()
            .lock()
            .map_err(|_| BrokerError::unavailable("Root withdrawal controller lock failed"))?;
        if !active.insert(id.clone()) {
            return read(&id)
                .map(|record| visible(&record))
                .map_err(BrokerError::indeterminate);
        }
    }
    let controller = Controller(id.clone());
    let outcome_id = id.clone();
    let retained = tokio::spawn(async move {
        let _controller = controller;
        static CAPACITY: OnceLock<tokio::sync::Semaphore> = OnceLock::new();
        let _capacity = CAPACITY
            .get_or_init(|| tokio::sync::Semaphore::new(2))
            .acquire()
            .await
            .map_err(|_| "Root withdrawal capacity is unavailable")?;
        let mut record = read(&id)?;
        if record.phase == Phase::Retired {
            return Ok(visible(&record));
        }
        if record.root_epoch != epoch() {
            record.phase = Phase::Indeterminate;
            record.error = Some("Original Root custody belongs to an earlier daemon epoch; missing custody is not a checked retirement receipt".into());
            save(&record)?;
            return Ok(visible(&record));
        }
        let result = drive(&state, &mut record).await;
        if let Err(error) = result {
            record.phase = Phase::Indeterminate;
            record.error = Some(error);
            save(&record)?;
        }
        Ok::<_, String>(visible(&record))
    });
    retained
        .await
        .map_err(|error| {
            outcome_error(
                &outcome_id,
                &format!("Root reconciliation wait interrupted: {error}"),
            )
        })?
        .map_err(|error| outcome_error(&outcome_id, &error))
}

fn outcome_error(id: &str, message: &str) -> BrokerError {
    let mut error = BrokerError::indeterminate(message);
    error.data = Some(json!({ "request_id": id, "root_retirement_confirmed": false }));
    error
}

async fn drive(state: &DaemonState, record: &mut Pending) -> Result<(), String> {
    let source = OwnerPolicy::load(record.requester)?;
    if !record.target.observed(&source) {
        return Err("The current authenticated private policy does not prove every requested revocation; no selector is proof".into());
    }
    record.source_generation = source.generation;
    record.source_fingerprint = Some(source.fingerprint.clone());
    save(record)?;
    if record.apps.is_empty() {
        let bindings = tokio::time::timeout(
            Duration::from_secs(10),
            retained_bindings(state, (record.requester != 0).then_some(record.requester)),
        )
        .await
        .map_err(|_| "Root custody inventory is still draining")??;
        record.apps = select(record, &source, bindings)?;
        record.phase = Phase::Retiring;
        save(record)?;
    }
    for index in 0..record.apps.len() {
        if record.apps[index].retired {
            continue;
        }
        if record.apps[index].supporting_evidence_missing {
            return Err("A potentially affected App has no retained supporting context/package; Root recovery is required".into());
        }
        let app = record.apps[index].app.clone();
        let owner = record.apps[index].owner;
        let fence = std::sync::Arc::new(super::app_mutation::fence_owner(&app, owner).await?);
        // A shared package read lock serializes publication, while only this
        // owner's admission is closed. Retained cleanup never fences others.
        super::app_store::retire_for_owner(state, &app, owner, fence).await?;
        record.apps[index].retired = true;
        save(record)?;
    }
    let latest = OwnerPolicy::load(record.requester)?;
    if !record.target.observed(&latest) {
        return Err("Current private policy no longer proves the withdrawal; no pending custody was released".into());
    }
    record.source_generation = latest.generation;
    record.source_fingerprint = Some(latest.fingerprint.clone());
    let remaining = tokio::time::timeout(
        Duration::from_secs(10),
        retained_bindings(state, (record.requester != 0).then_some(record.requester)),
    )
    .await
    .map_err(|_| "Final Root custody inspection is still draining")??;
    let remaining = select(record, &latest, remaining)?;
    if !remaining.is_empty() {
        for app in remaining {
            if let Some(previous) = record
                .apps
                .iter_mut()
                .find(|previous| previous.owner == app.owner && previous.app == app.app)
            {
                *previous = app;
            } else {
                record.apps.push(app);
            }
        }
        save(record)?;
        return Err("Affected Root bindings remain or policy changed during retirement; checked reconciliation remains pending".into());
    }
    record.phase = Phase::Retired;
    record.error = None;
    save(record)
}

fn select(
    record: &Pending,
    source: &OwnerPolicy,
    bindings: Vec<Binding>,
) -> Result<Vec<PendingApp>, String> {
    let app_selector =
        !matches!(&record.target, Target::Developer { kind, .. } if *kind != PackageKind::App);
    let mut apps = BTreeMap::<(u32, String), (bool, Vec<PackageRef>, bool)>::new();
    for binding in bindings {
        if record.requester != 0 && record.requester != binding.owner {
            continue;
        }
        let context_closure = record.requester == 0
            && binding.trust_context_uid == Some(0)
            && binding.context_generation_invalidated;
        if !app_selector && !context_closure {
            continue;
        }
        let missing = binding.package.is_none()
            || binding.trust_context_uid.is_none()
            || binding.package.as_ref().is_some_and(|package| {
                package.kind != PackageKind::App
                    || package.id != binding.app
                    || package.tier == "developer" && package.developer_path.is_none()
            });
        if !missing {
            let package = binding
                .package
                .as_ref()
                .ok_or("retained package disappeared")?;
            if !record.target.matches(package) && !context_closure {
                continue;
            }
            if !context_closure {
                match &record.target {
                    Target::Revocation { .. } => {
                        if record.requester == 0 && binding.trust_context_uid != Some(0) {
                            continue;
                        }
                        if source.check_denials(package).is_ok() {
                            continue;
                        }
                    }
                    Target::Developer { .. } => {
                        if binding.trust_context_uid != Some(record.requester) {
                            continue;
                        }
                        if package.is_live(&source.store).is_ok() {
                            continue;
                        }
                    }
                }
            }
        } else if !context_closure
            && binding
                .package
                .as_ref()
                .is_some_and(|package| !record.target.matches(package))
        {
            continue;
        }
        let key = (binding.owner, binding.app);
        let entry = apps
            .entry(key)
            .or_insert_with(|| (false, Vec::new(), false));
        entry.0 |= missing;
        entry.2 |= context_closure;
        if let Some(package) = binding.package {
            if !entry.1.contains(&package) {
                entry.1.push(package);
            }
        }
        if apps.len() > MAX_TARGETS {
            return Err("affected Root App inventory exceeds its bound".into());
        }
    }
    Ok(apps
        .into_iter()
        .map(
            |((owner, app), (missing, packages, context_generation_closure))| PendingApp {
                owner,
                app,
                retired: false,
                supporting_evidence_missing: missing,
                packages,
                context_generation_closure,
            },
        )
        .collect())
}

pub(crate) async fn retained_bindings(
    state: &DaemonState,
    owner_filter: Option<u32>,
) -> Result<Vec<Binding>, String> {
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (state, owner_filter);
        Err("checked Root App reconciliation requires Linux".into())
    }
    #[cfg(target_os = "linux")]
    {
        let services = state
            .app_service_manager()
            .ok_or("Root App service custody is unavailable")?;
        let mut bindings = services.retained_trust_bindings(owner_filter).await?;
        bindings.extend(super::gui::retained_trust_bindings(owner_filter)?);
        bindings.extend(super::app_execution::retained_trust_bindings(owner_filter)?);
        bindings.extend(
            crate::extension_host::spawn::TaskAppData::retained_trust_bindings(owner_filter)?,
        );
        let root = Path::new("/run/cos/caps");
        match std::fs::symlink_metadata(root) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(bindings),
            Err(error) => return Err(error.to_string()),
            Ok(_) => {}
        }
        crate::provenance::fsec::require_secure_location(root, &[0])
            .map_err(|error| error.to_string())?;
        let mut owners = 0;
        for entry in std::fs::read_dir(root).map_err(|error| error.to_string())? {
            let entry = entry.map_err(|error| error.to_string())?;
            if !entry
                .file_type()
                .map_err(|error| error.to_string())?
                .is_dir()
            {
                continue;
            }
            let owner = entry
                .file_name()
                .to_str()
                .and_then(|name| name.parse::<u32>().ok())
                .ok_or("unexpected owner in Root capability inventory")?;
            if owner_filter.is_some_and(|expected| expected != owner) {
                continue;
            }
            owners += 1;
            if owners > MAX_TARGETS {
                return Err("Root owner inventory exceeds its bound".into());
            }
            crate::provenance::fsec::require_secure_location(&entry.path(), &[0])
                .map_err(|error| error.to_string())?;
            let instances = crate::provenance::runtime::running_instances(owner)?;
            for (session, instance) in &instances {
                if instance.class != InstanceClass::App {
                    continue;
                }
                let package = instance.package.clone();
                let app = package
                    .as_ref()
                    .map(|package| package.id.clone())
                    .or_else(|| {
                        crate::proc::try_registry_sessions_for_owner(owner)
                            .ok()?
                            .into_iter()
                            .find(|row| row.session_id == *session)?
                            .app_id
                    })
                    .ok_or("Root App record has no recoverable installed identity")?;
                if !bindings.iter().any(|binding| {
                    binding.owner == owner
                        && binding.app == app
                        && binding.package == package
                        && binding.trust_context_uid == Some(0)
                }) {
                    bindings.push(Binding {
                        owner,
                        app,
                        package,
                        trust_context_uid: instance.trust_context_uid,
                        context_generation_invalidated: false,
                    });
                }
            }
            for row in crate::proc::try_registry_sessions_for_owner(owner)? {
                let Some(app) = row.app_id else {
                    continue;
                };
                if !bindings
                    .iter()
                    .any(|binding| binding.owner == owner && binding.app == app)
                {
                    bindings.push(Binding {
                        owner,
                        app,
                        package: None,
                        trust_context_uid: None,
                        context_generation_invalidated: false,
                    });
                }
            }
            if bindings.len() > MAX_TARGETS * 4 {
                return Err("Root binding inventory exceeds its bound".into());
            }
        }
        Ok(bindings)
    }
}

pub(crate) fn initialize(state: DaemonState) {
    if crate::provenance::fsec::effective_uid() != 0 {
        return;
    }
    static STARTED: OnceLock<()> = OnceLock::new();
    if STARTED.set(()).is_err() {
        return;
    }
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_secs(5));
        loop {
            tick.tick().await;
            let pending = match records() {
                Ok(records) => records,
                Err(error) => {
                    tracing::error!(%error, "Root withdrawal recovery state is unavailable");
                    continue;
                }
            };
            for record in pending
                .into_iter()
                .filter(|record| record.phase != Phase::Retired && record.root_epoch == epoch())
            {
                let _ = run(state.clone(), record.request_id).await;
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn package() -> PackageRef {
        PackageRef {
            kind: PackageKind::App,
            id: "withdrawal-fixture".into(),
            content_digest: format!("sha256:{}", "a".repeat(64)),
            publisher_key_id: None,
            tier: "developer".into(),
            developer_path: Some(PathBuf::from("/synthetic/retained/tree")),
        }
    }

    fn request(owner: u32) -> Pending {
        Pending {
            schema_version: 1,
            request_id: "a".repeat(32),
            requester: owner,
            root_epoch: "b".repeat(32),
            target: Target::Developer {
                kind: PackageKind::App,
                id: package().id,
            },
            source_generation: None,
            source_fingerprint: None,
            phase: Phase::Selecting,
            apps: Vec::new(),
            error: None,
        }
    }

    #[test]
    fn closed_requests_refuse_authority_selectors_and_malformed_references() {
        for value in [
            json!({"action":"reconcile","target":{"type":"developer","kind":"app","id":"fixture"},"owner":0}),
            json!({"action":"reconcile","target":{"type":"developer","kind":"app","id":"fixture","path":"/elsewhere"}}),
            json!({"action":"reconcile","target":{"type":"revocation","digest":"not-a-digest"}}),
            json!({"action":"status","request_id":"a".repeat(33)}),
            json!({"action":"recover","request_id":"a".repeat(32),"pid":1}),
        ] {
            assert!(serde_json::from_value::<Request>(value).is_err());
        }
    }

    #[test]
    fn developer_absence_only_affects_the_retained_supporting_context() {
        let policy = OwnerPolicy {
            owner: 0,
            generation: None,
            fingerprint: String::new(),
            store: std::sync::Arc::new(crate::provenance::TrustStore::default()),
        };
        let binding = Binding {
            owner: 101,
            app: package().id.clone(),
            package: Some(package()),
            trust_context_uid: Some(0),
            context_generation_invalidated: false,
        };
        assert!(select(&request(101), &policy, vec![binding.clone()])
            .unwrap()
            .is_empty());
        let affected = select(
            &request(0),
            &policy,
            vec![
                binding.clone(),
                Binding {
                    owner: 202,
                    ..binding.clone()
                },
            ],
        )
        .unwrap();
        assert_eq!(affected.len(), 2);
        assert!(select(
            &request(101),
            &policy,
            vec![Binding {
                owner: 202,
                trust_context_uid: Some(101),
                ..binding
            }]
        )
        .unwrap()
        .is_empty());
        let mut other = package();
        other.id = "unrelated-key".into();
        other.tier = "vendor".into();
        other.developer_path = None;
        let invalidated = Binding {
            owner: 101,
            app: other.id.clone(),
            package: Some(other),
            trust_context_uid: Some(0),
            context_generation_invalidated: true,
        };
        assert!(select(&request(101), &policy, vec![invalidated.clone()])
            .unwrap()
            .is_empty());
        let closure = select(&request(0), &policy, vec![invalidated]).unwrap();
        assert_eq!(closure.len(), 1);
        assert!(closure[0].context_generation_closure);
    }

    #[cfg(target_os = "linux")]
    mod restart_process {
        use super::*;
        use std::ffi::CString;
        use std::os::fd::AsRawFd;
        use std::os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt};
        use std::process::{Child, Command, Stdio};
        use std::time::Instant;

        const DRIVER: &str = "clawd::trust_withdrawal::tests::restart_process::root_controller_restart_keeps_unconfirmed_owner_scoped_outcome";
        const CONTROLLER: &str = "clawd::trust_withdrawal::tests::restart_process::controller_process";
        const FOREIGN: &str = "clawd::trust_withdrawal::tests::restart_process::foreign_owner_process";
        const ROLE: &str = "COS_TEST_WITHDRAWAL_EPOCH_ROLE";
        const ROOT: &str = "COS_TEST_WITHDRAWAL_EPOCH_ROOT";
        const UID: &str = "COS_TEST_WITHDRAWAL_EPOCH_FOREIGN_UID";
        const ID: &str = "COS_TEST_WITHDRAWAL_EPOCH_REQUEST_ID";
        const SOCKET: &str = "withdrawal.sock";

        struct OwnedProcess(Child);

        impl OwnedProcess {
            fn wait(&mut self, timeout: Duration) {
                let deadline = Instant::now() + timeout;
                loop {
                    if let Some(status) = self.0.try_wait().unwrap() {
                        assert!(status.success(), "fixture process failed: {status}");
                        return;
                    }
                    assert!(Instant::now() < deadline, "fixture process timed out");
                    std::thread::sleep(Duration::from_millis(10));
                }
            }
        }

        impl Drop for OwnedProcess {
            fn drop(&mut self) {
                if self.0.try_wait().ok().flatten().is_none() {
                    let _ = self.0.kill();
                    let _ = self.0.wait();
                }
            }
        }

        fn process(test: &str, role: &str, root: &Path) -> Command {
            let mut command = Command::new(std::env::current_exe().unwrap());
            command.args(["--exact", test, "--ignored", "--nocapture", "--test-threads=1"])
                .env(ROLE, role).env(ROOT, root)
                .stdout(Stdio::inherit()).stderr(Stdio::inherit());
            command
        }

        fn namespace_identity(name: &str) -> String {
            let metadata = std::fs::metadata(format!("/proc/self/ns/{name}")).unwrap();
            format!("{}:{}", metadata.dev(), metadata.ino())
        }

        fn require_private_namespaces() {
            assert_eq!(unsafe { libc::geteuid() }, 0, "Root controller fixture requires kernel UID 0");
            for name in ["mnt", "pid", "net"] {
                let outer = std::env::var(format!("COS_TEST_WITHDRAWAL_OUTER_{name}"))
                    .expect("namespace wrapper evidence is required");
                assert_ne!(namespace_identity(name), outer, "{name} namespace is not private");
            }
        }

        fn foreign_uid() -> u32 {
            let uid = 700_001;
            let mut passwd = unsafe { std::mem::zeroed::<libc::passwd>() };
            let mut group = unsafe { std::mem::zeroed::<libc::group>() };
            let mut buffer = vec![0u8; 64 * 1024];
            let mut found_passwd = std::ptr::null_mut();
            let mut found_group = std::ptr::null_mut();
            assert_eq!(unsafe { libc::getpwuid_r(uid, &mut passwd, buffer.as_mut_ptr().cast(), buffer.len(), &mut found_passwd) }, 0);
            assert!(found_passwd.is_null(), "fixture refuses to borrow an existing UID");
            assert_eq!(unsafe { libc::getgrgid_r(uid, &mut group, buffer.as_mut_ptr().cast(), buffer.len(), &mut found_group) }, 0);
            assert!(found_group.is_null(), "fixture refuses to borrow an existing GID");
            uid
        }

        fn bind(source: &Path, target: &Path) {
            let source = CString::new(source.as_os_str().as_encoded_bytes()).unwrap();
            let target = CString::new(target.as_os_str().as_encoded_bytes()).unwrap();
            assert_eq!(unsafe {
                libc::mount(source.as_ptr(), target.as_ptr(), std::ptr::null(), libc::MS_BIND, std::ptr::null())
            }, 0, "private bind failed: {}", std::io::Error::last_os_error());
        }

        fn require_private_state(root: &Path) {
            require_private_namespaces();
            for (source, target) in [
                (root.join("etc"), Path::new("/etc")),
                (root.join("var-lib"), Path::new("/var/lib")),
                (root.join("run"), Path::new("/run")),
            ] {
                let expected = std::fs::metadata(source).unwrap();
                let actual = std::fs::metadata(target).unwrap();
                assert_eq!((actual.dev(), actual.ino()), (expected.dev(), expected.ino()), "canonical state is not privately bound");
            }
            assert_eq!(crate::paths::verified_home_for_uid(0).unwrap(), Path::new("/run/cos/withdrawal-home"));
        }

        fn setup(root: &Path, uid: u32) -> PathBuf {
            require_private_namespaces();
            assert!(std::process::id() > 1, "PID 1 must supervise the authenticated driver");
            assert_eq!(unsafe {
                libc::mount(std::ptr::null(), c"/".as_ptr(), std::ptr::null(), libc::MS_REC | libc::MS_PRIVATE, std::ptr::null())
            }, 0);
            let home = PathBuf::from("/run/cos/withdrawal-home");
            let foreign = PathBuf::from("/run/cos/withdrawal-foreign");
            let home_source = root.join("run/cos/withdrawal-home");
            let foreign_source = root.join("run/cos/withdrawal-foreign");
            let control = root.join("control");
            for directory in [&home_source, &foreign_source, &control, &root.join("etc/cos"), &root.join("var-lib/cos"), &root.join("run/cos/caps"), &root.join("vendor-trust")] {
                std::fs::create_dir_all(directory).unwrap();
            }
            for directory in ["etc", "etc/cos", "var-lib", "var-lib/cos", "run", "run/cos", "run/cos/caps", "vendor-trust"] {
                std::fs::set_permissions(root.join(directory), std::fs::Permissions::from_mode(0o755)).unwrap();
            }
            std::fs::set_permissions(&control, std::fs::Permissions::from_mode(0o711)).unwrap();
            std::fs::set_permissions(&home_source, std::fs::Permissions::from_mode(0o700)).unwrap();
            std::fs::set_permissions(&foreign_source, std::fs::Permissions::from_mode(0o700)).unwrap();
            let foreign_path = CString::new(foreign_source.as_os_str().as_encoded_bytes()).unwrap();
            assert_eq!(unsafe { libc::chown(foreign_path.as_ptr(), uid, uid) }, 0);
            std::fs::write(root.join("etc/passwd"), format!(
                "root:x:0:0:Private root:{}:/bin/sh\nwithdrawal-foreign:x:{uid}:{uid}:Private foreign:{}:/bin/sh\n",
                home.display(), foreign.display(),
            )).unwrap();
            std::fs::write(root.join("etc/group"), format!("root:x:0:\nwithdrawal-foreign:x:{uid}:\n")).unwrap();
            std::fs::write(root.join("etc/nsswitch.conf"), "passwd: files\ngroup: files\nhosts: files\n").unwrap();
            for name in ["passwd", "group", "nsswitch.conf"] {
                std::fs::set_permissions(root.join("etc").join(name), std::fs::Permissions::from_mode(0o644)).unwrap();
            }
            bind(&root.join("etc"), Path::new("/etc"));
            bind(&root.join("var-lib"), Path::new("/var/lib"));
            bind(&root.join("run"), Path::new("/run"));
            match std::fs::symlink_metadata("/usr/lib/cos/trust") {
                Ok(metadata) => {
                    assert!(metadata.is_dir() && !metadata.file_type().is_symlink());
                    bind(&root.join("vendor-trust"), Path::new("/usr/lib/cos/trust"));
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => panic!("cannot privately mask vendor trust: {error}"),
            }
            for (name, value) in [
                ("HOME", home.clone()), ("COS_DATA_DIR", PathBuf::from("/var/lib/cos/epoch-test")),
                ("COS_CONFIG_DIR", PathBuf::from("/etc/cos")), ("COS_CACHE_DIR", PathBuf::from("/var/lib/cos/cache")),
                ("COS_LOG_DIR", PathBuf::from("/var/lib/cos/logs")), ("COS_RUNTIME_DIR", PathBuf::from("/run/cos")),
                ("COS_PROC_DATA_DIR", PathBuf::from("/run/cos/test-proc")), ("COS_APPS_DIR", PathBuf::from("/var/lib/cos/no-apps")),
                ("COS_PROVENANCE_RUNTIME_DIR", PathBuf::from("/run/cos/provenance")),
            ] { std::env::set_var(name, value); }
            for name in ["COS_SESSION", "COS_USER_DATA_DIR", "COS_USER_CONFIG_DIR", "COS_CAPS_DATA_DIR", "COS_PERMS_MODE"] {
                std::env::remove_var(name);
            }
            assert_eq!(crate::paths::verified_home_for_uid(0).unwrap(), home);
            assert_eq!(crate::paths::verified_home_for_uid(uid).unwrap(), foreign);
            require_private_state(root);
            control
        }

        fn denial_policy() -> Target {
            let trust = PathBuf::from("/run/cos/withdrawal-home/.config/cos/trust");
            let publishers = trust.join("publishers.d");
            let developer = trust.join("developer.d");
            for directory in [trust.parent().unwrap().parent().unwrap(), trust.parent().unwrap(), &trust, &publishers, &developer] {
                std::fs::create_dir_all(directory).unwrap();
                std::fs::set_permissions(directory, std::fs::Permissions::from_mode(0o700)).unwrap();
            }
            let digest = format!("sha256:{}", crate::crypto::sha256_hex(b"private unconfirmed withdrawal epoch fixture"));
            let policy = publishers.join("operator.json");
            std::fs::write(&policy, serde_json::to_vec(&json!({
                "schema": crate::provenance::trust::TRUST_SCHEMA_V1,
                "keys": [], "revoked_keys": [], "revoked_packages": [digest],
            })).unwrap()).unwrap();
            std::fs::set_permissions(policy, std::fs::Permissions::from_mode(0o600)).unwrap();
            crate::provenance::state::bump(&trust, crate::provenance::state::TrustDomain::Owner(0), &[publishers, developer]).unwrap();
            // No positive trust or App admission is fabricated for this journal-boundary fixture.
            Target::Revocation { key_id: None, digest: Some(digest) }
        }

        fn request(action: Value) -> crate::clawd::protocol::Response {
            crate::clawd::client::request_blocking(
                SOCKET,
                crate::clawd::protocol::Request::build(
                    crate::clawd::routes::Command::ProvenanceTrustWithdrawal, action,
                ),
            ).expect("exchange must authenticate the real Root socket peer")
        }

        fn result(response: crate::clawd::protocol::Response) -> Value {
            assert!(response.ok, "Root request failed: {:?}", response.error);
            response.result.unwrap()
        }

        fn ready(root: &Path, label: &str, process: &mut OwnedProcess) -> Value {
            let deadline = Instant::now() + Duration::from_secs(15);
            loop {
                if let Ok(bytes) = std::fs::read(root.join(format!("control/ready-{label}.json"))) {
                    if let Ok(value) = serde_json::from_slice::<Value>(&bytes) {
                        assert_eq!(value["pid"].as_u64(), Some(process.0.id() as u64));
                        return value;
                    }
                }
                assert!(process.0.try_wait().unwrap().is_none(), "controller exited before readiness");
                assert!(Instant::now() < deadline, "controller readiness timed out");
                std::thread::sleep(Duration::from_millis(10));
            }
        }

        #[test]
        #[ignore = "Root private-namespace controller-process acceptance; no reserved identities"]
        fn root_controller_restart_keeps_unconfirmed_owner_scoped_outcome() {
            if std::env::var(ROLE).as_deref() != Ok("driver") {
                assert_eq!(unsafe { libc::geteuid() }, 0, "requires Root for private namespaces");
                let uid = foreign_uid();
                let root = tempfile::Builder::new().prefix("withdrawal-epoch-")
                    .tempdir_in(std::env::current_dir().unwrap()).unwrap();
                let mut command = Command::new("unshare");
                command.args(["--mount", "--net", "--pid", "--fork", "--mount-proc", "--propagation", "private", "--kill-child=KILL"])
                    .args([
                        "/bin/bash", "--noprofile", "--norc", "-c",
                        "status=0; \"$1\" --exact \"$2\" --ignored --nocapture --test-threads=1 || status=$?; exit \"$status\"",
                        "withdrawal-epoch",
                    ])
                    .arg(std::env::current_exe().unwrap())
                    .arg(DRIVER)
                    .env(ROLE, "driver").env(ROOT, root.path()).env(UID, uid.to_string());
                for name in ["mnt", "pid", "net"] {
                    command.env(format!("COS_TEST_WITHDRAWAL_OUTER_{name}"), namespace_identity(name));
                }
                let mut child = OwnedProcess(command.spawn().unwrap());
                child.wait(Duration::from_secs(90));
                return;
            }
            let root = PathBuf::from(std::env::var_os(ROOT).unwrap());
            let uid: u32 = std::env::var(UID).unwrap().parse().unwrap();
            let control = setup(&root, uid);
            std::env::set_current_dir(&control).unwrap();
            let target = denial_policy();
            let mut first = OwnedProcess(process(CONTROLLER, "a", &root).current_dir(&control).spawn().unwrap());
            let first_ready = ready(&root, "a", &mut first);
            let initial = result(request(json!({"action":"reconcile","target":target})));
            assert_eq!(initial["state"], "indeterminate");
            assert_eq!(initial["root_retirement_confirmed"], false);
            assert!(initial["error"].as_str().unwrap().contains("service custody is unavailable"));
            let id = initial["request_id"].as_str().unwrap().to_string();
            let original = read(&id).unwrap();
            assert_eq!(original.root_epoch, first_ready["epoch"].as_str().unwrap());
            assert_eq!(original.requester, 0);
            assert!(original.source_generation.is_some() && original.source_fingerprint.is_some());
            assert_eq!(result(request(json!({"action":"status","request_id":id})))["root_retirement_confirmed"], false);
            let metadata = std::fs::metadata(path(&id).unwrap()).unwrap();
            assert_eq!(metadata.uid(), 0);
            assert_eq!(metadata.mode() & 0o777, 0o600);
            assert_eq!(metadata.nlink(), 1);
            assert_eq!(std::fs::metadata(STATE_ROOT).unwrap().mode() & 0o777, 0o700);
            first.0.kill().unwrap();
            assert!(!first.0.wait().unwrap().success(), "first controller must be lost, not cleanly drained");
            let socket = control.join(SOCKET);
            assert!(std::fs::symlink_metadata(&socket).unwrap().file_type().is_socket());
            std::fs::remove_file(socket).unwrap();
            std::fs::remove_dir_all("/run/cos/withdrawal-home/.config/cos/trust").unwrap();
            let mut second = OwnedProcess(process(CONTROLLER, "b", &root).current_dir(&control).spawn().unwrap());
            let second_ready = ready(&root, "b", &mut second);
            assert_ne!(first_ready["pid"], second_ready["pid"]);
            assert_ne!(first_ready["epoch"], second_ready["epoch"]);
            assert_eq!(result(request(json!({"action":"status","request_id":id})))["state"], "indeterminate");
            let before_foreign = std::fs::read(path(&id).unwrap()).unwrap();
            let mut foreign = OwnedProcess(process(FOREIGN, "foreign", &root).env(ID, &id).current_dir(&control).spawn().unwrap());
            foreign.wait(Duration::from_secs(15));
            assert_eq!(std::fs::read(path(&id).unwrap()).unwrap(), before_foreign, "another owner changed the protected outcome");
            for action in [
                json!({"action":"recover","request_id":id}),
                json!({"action":"reconcile","target":target}),
                json!({"action":"status","request_id":id}),
            ] {
                let outcome = result(request(action));
                assert_eq!(outcome["request_id"], id);
                assert_eq!(outcome["state"], "indeterminate");
                assert_eq!(outcome["root_retirement_confirmed"], false);
                assert!(outcome["error"].as_str().unwrap().contains("earlier daemon epoch"));
            }
            second.wait(Duration::from_secs(15));
            let persisted = read(&id).unwrap();
            assert_eq!(persisted.root_epoch, original.root_epoch);
            assert!(matches!(persisted.phase, Phase::Indeterminate));
            assert_eq!(persisted.apps.len(), original.apps.len());
            assert_eq!(persisted.source_generation, original.source_generation);
            assert_eq!(persisted.source_fingerprint, original.source_fingerprint);
            assert_eq!(records().unwrap().len(), 1, "reconciliation was replayed as a new request");
            assert!(!Path::new("/run/cos/withdrawal-home/.config/cos/trust").exists(), "recovery replayed a policy mutation");
            let peers: Vec<Value> = serde_json::from_slice(&std::fs::read(control.join("peers-b.json")).unwrap()).unwrap();
            assert_eq!(peers.iter().filter(|peer| peer["uid"].as_u64() == Some(uid as u64)).count(), 2);
            assert_eq!(peers.iter().filter(|peer| peer["uid"] == 0).count(), 4);
        }

        #[test]
        #[ignore = "subprocess entry for the controller restart fixture"]
        fn controller_process() {
            let Ok(label) = std::env::var(ROLE) else { return; };
            assert!(label == "a" || label == "b");
            require_private_namespaces();
            let root = PathBuf::from(std::env::var_os(ROOT).unwrap());
            require_private_state(&root);
            let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
            runtime.block_on(async {
                let listener = std::os::unix::net::UnixListener::bind(SOCKET).unwrap();
                crate::clawd::transport::peer::enable_credential_passing(listener.as_raw_fd()).unwrap();
                std::fs::set_permissions(SOCKET, std::fs::Permissions::from_mode(0o666)).unwrap();
                listener.set_nonblocking(true).unwrap();
                let listener = tokio::net::UnixListener::from_std(listener).unwrap();
                let state = DaemonState::try_new().unwrap();
                // This starts the real controller/dispatcher, not clawd's App
                // custody/NSS pool or installed-daemon recovery sequence.
                assert!(state.app_service_manager().is_none());
                initialize(state.clone());
                tokio::task::yield_now().await;
                std::fs::write(root.join(format!("control/ready-{label}.json")),
                    serde_json::to_vec(&json!({"pid":std::process::id(),"epoch":epoch()})).unwrap()).unwrap();
                let admission = crate::clawd::transport::Admission::new(crate::clawd::transport::Limits::default());
                let mut peers = Vec::new();
                for _ in 0..if label == "a" { 3 } else { 6 } {
                    let (stream, _) = tokio::time::timeout(Duration::from_secs(30), listener.accept()).await.unwrap().unwrap();
                    let mut stream = crate::clawd::transport::PeerStream::new(stream).unwrap();
                    let frame = tokio::time::timeout(Duration::from_secs(10), stream.read_request(crate::clawd::wire::MAX_REQUEST_BYTES)).await.unwrap().unwrap();
                    let crate::clawd::transport::ReadOutcome::Frame(frame) = frame else { panic!("expected authenticated frame"); };
                    let peer = crate::clawd::transport::peer::verify(frame.credentials).expect("kernel peer identity must verify");
                    let request: crate::clawd::wire::Request = serde_json::from_slice(&frame.body).unwrap();
                    assert_eq!(request.command, crate::clawd::routes::Command::ProvenanceTrustWithdrawal);
                    peers.push(json!({"uid":peer.uid,"pid":peer.pid,"start":peer.start_time_ticks}));
                    let client = ClientIdentity::from_peer(peer);
                    let response = crate::clawd::server::dispatch_verified_request(request, &client, &state, &admission).await;
                    let encoded = crate::clawd::protocol::encode_response(&response).unwrap();
                    tokio::time::timeout(Duration::from_secs(10), stream.write_response(&encoded)).await.unwrap().unwrap();
                }
                std::fs::write(root.join(format!("control/peers-{label}.json")), serde_json::to_vec(&peers).unwrap()).unwrap();
            });
        }

        #[test]
        #[ignore = "subprocess entry for the foreign-owner credential check"]
        fn foreign_owner_process() {
            if std::env::var(ROLE).as_deref() != Ok("foreign") { return; }
            require_private_namespaces();
            let root = PathBuf::from(std::env::var_os(ROOT).unwrap());
            require_private_state(&root);
            let uid: u32 = std::env::var(UID).unwrap().parse().unwrap();
            let id = std::env::var(ID).unwrap();
            std::env::set_current_dir(root.join("control")).unwrap();
            assert_eq!(unsafe { libc::setgroups(0, std::ptr::null()) }, 0);
            assert_eq!(unsafe { libc::setresgid(uid, uid, uid) }, 0);
            assert_eq!(unsafe { libc::setresuid(uid, uid, uid) }, 0);
            assert_eq!(unsafe { libc::getuid() }, uid);
            for action in ["status", "recover"] {
                let response = request(json!({"action":action,"request_id":id}));
                assert!(!response.ok && response.result.is_none());
                let error = response.error.unwrap();
                assert_eq!(error.code, "not_authorized");
                assert!(error.message.contains("another peer"), "{error:?}");
            }
        }
    }
}
