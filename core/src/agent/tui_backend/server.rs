use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};
use tokio::sync::{mpsc, Mutex, OwnedSemaphorePermit, Semaphore};

use super::approvals::Approvals;
use super::backend::{Backend, Operation};
use super::events::{turn_view, Projection};
use super::history;
use super::input;
use super::protocol::{
    backend_string, notification, Params, RequestId, RpcError, MAX_HISTORY_ROWS, MAX_LOADED_THREADS,
};
use super::Options;

pub(super) struct LoadedThread {
    pub session_id: String,
    pub frontend_id: String,
    pub operation: Mutex<()>,
    pub active: Mutex<Option<String>>,
    pub model: Mutex<Option<String>>,
}

pub(super) struct Service {
    pub backend: Arc<dyn Backend>,
    pub options: Options,
    pub creation: Mutex<()>,
    loaded: Mutex<HashMap<String, Arc<LoadedThread>>>,
    stream_slots: Arc<Semaphore>,
}

impl Service {
    pub fn new(backend: Arc<dyn Backend>, options: Options) -> Self {
        Self {
            backend,
            options,
            creation: Mutex::new(()),
            loaded: Mutex::new(HashMap::new()),
            stream_slots: Arc::new(Semaphore::new(8)),
        }
    }

    pub async fn resolve(&self, id: &str) -> Result<String, RpcError> {
        if super::protocol::canonical_session_id(id).is_some() {
            return Ok(id.to_string());
        }
        let alias = uuid::Uuid::parse_str(id).map_err(|_| RpcError::params("Invalid threadId"))?;
        let result = self
            .backend
            .call(
                Operation::ConversationGet,
                json!({ "id": alias.to_string(), "limit": 1 }),
            )
            .await?;
        let conversation = history::conversation(result)?;
        let resolved = uuid::Uuid::parse_str(history::frontend_id(&conversation)?)
            .map_err(|_| RpcError::backend("Claw returned an invalid presentation_id"))?;
        if alias != resolved {
            return Err(RpcError::backend(
                "Claw resolved a different frontend conversation",
            ));
        }
        Ok(backend_string(&conversation, "id")?.to_string())
    }

    pub async fn conversation(&self, canonical: &str) -> Result<Value, RpcError> {
        let result = self
            .backend
            .call(
                Operation::ConversationGet,
                json!({ "id": canonical, "limit": MAX_HISTORY_ROWS }),
            )
            .await?;
        let conversation = history::conversation(result)?;
        if backend_string(&conversation, "id")? != canonical {
            return Err(RpcError::backend("Claw returned another conversation"));
        }
        self.with_parent_identity(conversation).await
    }

    pub async fn with_parent_identity(&self, mut conversation: Value) -> Result<Value, RpcError> {
        if let Some(parent_id) = conversation.get("parent_id").and_then(Value::as_str) {
            super::protocol::canonical_session_id(parent_id)
                .ok_or_else(|| RpcError::backend("Claw returned an invalid parent session id"))?;
            let result = self
                .backend
                .call(
                    Operation::ConversationGet,
                    json!({ "id": parent_id, "limit": 1 }),
                )
                .await?;
            // A soft-deleted parent's identity can still describe lineage. Its
            // history is not part of this metadata projection.
            let parent = result
                .get("conversation")
                .ok_or_else(|| RpcError::backend("Claw returned no parent conversation"))?;
            if backend_string(parent, "id")? != parent_id {
                return Err(RpcError::backend(
                    "Claw resolved a different parent conversation",
                ));
            }
            conversation["parent_presentation_id"] = json!(history::frontend_id(parent)?);
        }
        Ok(conversation)
    }

    pub async fn loaded(&self, canonical: &str) -> Result<Arc<LoadedThread>, RpcError> {
        if let Some(thread) = self.loaded.lock().await.get(canonical) {
            return Ok(thread.clone());
        }
        let conversation = self.conversation(canonical).await?;
        let frontend_id = history::frontend_id(&conversation)?.to_string();
        let mut loaded = self.loaded.lock().await;
        if let Some(thread) = loaded.get(canonical) {
            return Ok(thread.clone());
        }
        if loaded.len() >= MAX_LOADED_THREADS {
            return Err(RpcError::capacity());
        }
        let thread = Arc::new(LoadedThread {
            session_id: canonical.to_string(),
            frontend_id,
            operation: Mutex::new(()),
            active: Mutex::new(None),
            model: Mutex::new(None),
        });
        loaded.insert(canonical.to_string(), thread.clone());
        Ok(thread)
    }

    pub async fn can_load(&self) -> Result<(), RpcError> {
        if self.loaded.lock().await.len() >= MAX_LOADED_THREADS {
            return Err(RpcError::capacity());
        }
        Ok(())
    }

    pub async fn loaded_model(&self, canonical: &str) -> Option<String> {
        let thread = self.loaded.lock().await.get(canonical).cloned()?;
        let selected = thread.model.lock().await.clone();
        Some(selected.unwrap_or_else(|| self.backend.info().model.clone()))
    }

    pub async fn active(
        &self,
        thread: &LoadedThread,
        conversation: &Value,
    ) -> Result<Option<Value>, RpcError> {
        let known = thread.active.lock().await.clone();
        if let Some(id) = known {
            let job = self
                .backend
                .call(Operation::TaskGet, json!({ "id": id }))
                .await?;
            if backend_string(&job, "session_id")? != thread.session_id
                || backend_string(&job, "id")? != id
            {
                return Err(RpcError::backend(
                    "Active task belongs to another conversation",
                ));
            }
            match backend_string(&job, "status")? {
                "pending" | "running" | "waiting_approval" => return Ok(Some(job)),
                "ok" | "error" | "cancelled" => {
                    let mut active = thread.active.lock().await;
                    if active.as_deref() == Some(id.as_str()) {
                        *active = None;
                    }
                }
                _ => return Err(RpcError::backend("Claw returned an unknown task status")),
            }
        }
        let jobs = history::current_jobs(self.backend.as_ref(), conversation).await?;
        let active = history::active_job(&jobs)?;
        *thread.active.lock().await = active
            .as_ref()
            .map(|job| backend_string(job, "id").map(str::to_string))
            .transpose()?;
        Ok(active)
    }

    pub fn stream_slot(&self) -> Result<OwnedSemaphorePermit, RpcError> {
        self.stream_slots
            .clone()
            .try_acquire_owned()
            .map_err(|_| RpcError::capacity())
    }
}

pub(super) struct Outcome {
    pub result: Value,
    pub notifications: Vec<Value>,
    pub drive: Option<Drive>,
}

impl Outcome {
    pub fn result(result: Value) -> Self {
        Self {
            result,
            notifications: Vec::new(),
            drive: None,
        }
    }
}

pub(super) struct Drive {
    pub job: Value,
    pub thread_id: String,
    pub client_message_id: Option<String>,
    pub subscription: Arc<AtomicBool>,
    pub slot: OwnedSemaphorePermit,
}

pub(super) struct Connection {
    pub service: Arc<Service>,
    initialized: AtomicU8,
    experimental: AtomicBool,
    opted_out: Mutex<HashSet<String>>,
    pub subscriptions: Mutex<HashMap<String, Arc<AtomicBool>>>,
    driving: Mutex<HashSet<String>>,
    approvals: Approvals,
}

impl Connection {
    pub fn new(service: Arc<Service>) -> Self {
        Self {
            service,
            initialized: AtomicU8::new(0),
            experimental: AtomicBool::new(false),
            opted_out: Mutex::new(HashSet::new()),
            subscriptions: Mutex::new(HashMap::new()),
            driving: Mutex::new(HashSet::new()),
            approvals: Approvals::default(),
        }
    }

    pub async fn execute(&self, method: &str, value: Value) -> Result<Outcome, RpcError> {
        if method == "initialize" {
            return self.initialize(value).await;
        }
        if self.initialized.load(Ordering::Acquire) != 2 {
            return Err(RpcError::not_initialized());
        }
        if let Some(result) =
            super::bootstrap::read(method, value.clone(), self.service.backend.info())
        {
            return result.map(Outcome::result);
        }
        match method {
            "thread/start" => self.start_thread(value).await,
            "thread/resume" => self.resume_thread(value).await,
            "thread/read" => self.read_thread(value).await,
            "thread/list" => self.list_threads(value).await,
            "thread/loaded/list" => self.loaded_threads(value).await,
            "thread/unsubscribe" => self.unsubscribe_thread(value).await,
            "thread/name/set" | "thread/archive" | "thread/unarchive" | "thread/delete" => {
                self.update_thread(method, value).await
            }
            "thread/fork" => self.fork_thread(value).await,
            "thread/revert" => self.revert_thread(value).await,
            "thread/turns/list" => self.list_turns(value).await,
            "thread/items/list" => self.list_items(value).await,
            "thread/settings/update" => self.update_model_settings(value).await,
            "turn/start" => self.start_turn(value).await,
            "turn/interrupt" => self.interrupt_turn(value).await,
            "turn/steer" => {
                let params = Params::new(
                    value,
                    &[
                        "threadId",
                        "input",
                        "expectedTurnId",
                        "clientUserMessageId",
                        "responsesapiClientMetadata",
                        "additionalContext",
                    ],
                )?;
                let canonical = self
                    .service
                    .resolve(params.required_string("threadId")?)
                    .await?;
                let conversation = self.service.conversation(&canonical).await?;
                let thread = self.service.loaded(&canonical).await?;
                let job = self
                    .service
                    .active(&thread, &conversation)
                    .await?
                    .ok_or_else(|| RpcError::stale("No active turn to steer"))?;
                if backend_string(&job, "id")? != params.required_string("expectedTurnId")? {
                    return Err(RpcError::stale(
                        "expectedTurnId does not match the active Claw task",
                    ));
                }
                Err(RpcError::option(
                    "turn/steer (keep input in the TUI queue until the task finishes)",
                ))
            }
            "skills/list" => {
                let params = Params::new(value, &["cwds", "forceReload"])?;
                params.boolean("forceReload", false)?;
                if let Some(cwds) = params.array("cwds")? {
                    if cwds.len() > 1
                        || cwds.iter().any(|cwd| {
                            !cwd.as_str().is_some_and(|cwd| {
                                std::path::Path::new(cwd) == self.service.backend.info().home
                            })
                        })
                    {
                        return Err(RpcError::option(
                            "cwds (Claw uses its authenticated owner Skill catalogue)",
                        ));
                    }
                }
                self.service
                    .backend
                    .call(Operation::SkillsList, json!({}))
                    .await
                    .map(Outcome::result)
            }
            _ => Err(RpcError::unsupported(method)),
        }
    }

    pub async fn current_model(&self, thread: &LoadedThread) -> String {
        thread
            .model
            .lock()
            .await
            .clone()
            .unwrap_or_else(|| self.service.backend.info().model.clone())
    }

    pub async fn configure_model(
        &self,
        thread: &LoadedThread,
        params: &Params,
        jobs: Option<&[Value]>,
    ) -> Result<(), RpcError> {
        if let Some(model) = params.string("model")? {
            input::model_choice(model, self.service.backend.info())?;
            *thread.model.lock().await = Some(model.to_string());
            return Ok(());
        }
        if thread.model.lock().await.is_some() {
            return Ok(());
        }
        if let Some(last) = jobs.and_then(|jobs| jobs.last()) {
            let id = backend_string(last, "id")?;
            let job = self
                .service
                .backend
                .call(Operation::TaskGet, json!({ "id": id }))
                .await?;
            if backend_string(&job, "id")? != id
                || backend_string(&job, "session_id")? != backend_string(last, "session_id")?
            {
                return Err(RpcError::backend(
                    "Saved model belongs to another task or conversation",
                ));
            }
            if let Some(model) = job.get("requested_model").and_then(Value::as_str) {
                let mut selection = thread.model.lock().await;
                if selection.is_none() {
                    input::model_choice(model, self.service.backend.info())?;
                    *selection = Some(model.to_string());
                }
            }
        }
        Ok(())
    }

    async fn update_model_settings(&self, value: Value) -> Result<Outcome, RpcError> {
        let params = Params::new(
            value,
            &[
                "threadId",
                "disabledPluginIds",
                "cwd",
                "approvalPolicy",
                "approvalsReviewer",
                "sandboxPolicy",
                "permissions",
                "model",
                "serviceTier",
                "effort",
                "summary",
                "collaborationMode",
                "multiAgentMode",
                "personality",
            ],
        )?;
        input::settings(&params, self.service.backend.info())?;
        let canonical = self
            .service
            .resolve(params.required_string("threadId")?)
            .await?;
        self.service.conversation(&canonical).await?;
        let thread = self.service.loaded(&canonical).await?;
        let _operation = thread.operation.lock().await;
        self.configure_model(&thread, &params, None).await?;
        Ok(Outcome::result(json!({})))
    }

    async fn initialize(&self, value: Value) -> Result<Outcome, RpcError> {
        let params = Params::new(value, &["clientInfo", "capabilities"])?;
        let client = Params::new(
            params
                .value("clientInfo")
                .cloned()
                .ok_or_else(|| RpcError::params("clientInfo is required"))?,
            &["name", "title", "version"],
        )?;
        for key in ["name", "version"] {
            let value = client.required_string(key)?;
            if value.len() > 128 || value.chars().any(char::is_control) {
                return Err(RpcError::params("Invalid clientInfo"));
            }
        }
        if client
            .string("title")?
            .is_some_and(|value| value.len() > 256)
        {
            return Err(RpcError::params("Invalid clientInfo title"));
        }
        let capabilities = Params::new(
            params.value("capabilities").cloned().unwrap_or(Value::Null),
            &[
                "experimentalApi",
                "requestAttestation",
                "mcpServerOpenaiFormElicitation",
                "optOutNotificationMethods",
                "extensions",
            ],
        )?;
        let experimental = capabilities.boolean("experimentalApi", false)?;
        capabilities.boolean("requestAttestation", false)?;
        capabilities.boolean("mcpServerOpenaiFormElicitation", false)?;
        if capabilities
            .value("extensions")
            .is_some_and(|extensions| !extensions.is_object())
        {
            return Err(RpcError::params("extensions must be an object"));
        }
        let opted_out = capabilities.unique_strings("optOutNotificationMethods", 128)?;
        if self
            .initialized
            .compare_exchange(0, 1, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return Err(RpcError::invalid_request("Already initialized"));
        }
        *self.opted_out.lock().await = opted_out;
        self.experimental.store(experimental, Ordering::Release);
        Ok(Outcome::result(json!({
            "userAgent": format!("claw/{}", env!("CARGO_PKG_VERSION")),
            "codexHome": self.service.backend.info().config_home,
            "platformFamily": "unix",
            "platformOs": std::env::consts::OS,
        })))
    }

    pub fn notify(&self, method: &str, params: Value) -> Result<(), RpcError> {
        if method == "initialized" {
            Params::new(params, &[])?;
            return self
                .initialized
                .compare_exchange(1, 2, Ordering::AcqRel, Ordering::Acquire)
                .map(|_| ())
                .map_err(|_| RpcError::invalid_request("Unexpected initialized notification"));
        }
        Err(RpcError::unsupported(method))
    }

    pub async fn answer(
        &self,
        id: RequestId,
        result: Result<Value, Value>,
    ) -> Result<Vec<Value>, RpcError> {
        if self.initialized.load(Ordering::Acquire) != 2 {
            return Err(RpcError::not_initialized());
        }
        self.approvals
            .respond(self.service.backend.as_ref(), id, result)
            .await
    }

    pub async fn subscribe(&self, canonical: &str) -> Arc<AtomicBool> {
        let mut subscriptions = self.subscriptions.lock().await;
        let subscription = subscriptions
            .entry(canonical.to_string())
            .or_insert_with(|| Arc::new(AtomicBool::new(true)))
            .clone();
        subscription.store(true, Ordering::Release);
        subscription
    }

    pub async fn resume_drive(
        &self,
        canonical: &str,
        job: Option<Value>,
    ) -> Result<Option<Drive>, RpcError> {
        let subscription = self.subscribe(canonical).await;
        let Some(job) = job else { return Ok(None) };
        if backend_string(&job, "session_id")? != canonical {
            return Err(RpcError::backend(
                "Cannot attach a live task from an ancestor session",
            ));
        }
        let thread_id = self.service.loaded(canonical).await?.frontend_id.clone();
        let id = backend_string(&job, "id")?.to_string();
        let mut driving = self.driving.lock().await;
        if driving.contains(&id) {
            return Ok(None);
        }
        let slot = self.service.stream_slot()?;
        driving.insert(id);
        Ok(Some(Drive {
            job,
            thread_id,
            client_message_id: None,
            subscription,
            slot,
        }))
    }

    async fn start_turn(&self, value: Value) -> Result<Outcome, RpcError> {
        let params = Params::new(value, input::TURN_FIELDS)?;
        input::settings(&params, self.service.backend.info())?;
        let prompt = input::prompt(&params)?;
        let client_message_id = input::client_message_id(&params)?;
        let canonical = self
            .service
            .resolve(params.required_string("threadId")?)
            .await?;
        let thread = self.service.loaded(&canonical).await?;
        let _operation = thread.operation.lock().await;
        let conversation = self.service.conversation(&canonical).await?;
        if conversation.get("archived").and_then(Value::as_bool) == Some(true) {
            return Err(RpcError::failed(
                "Unarchive this conversation before submitting a turn",
            ));
        }
        if self.service.active(&thread, &conversation).await?.is_some() {
            return Err(RpcError::busy());
        }
        let model = match params.string("model")? {
            Some(model) => model.to_string(),
            None => self.current_model(&thread).await,
        };
        input::model_choice(&model, self.service.backend.info())?;
        let slot = self.service.stream_slot()?;
        let mut request = json!({
            "prompt": prompt,
            "session_id": canonical,
            "use_memory": self.service.options.use_memory,
            "model": model,
        });
        if let Some(max_turns) = self.service.options.max_turns {
            request["max_turns"] = json!(max_turns);
        }
        let job = self
            .service
            .backend
            .call(Operation::TaskSubmit, request)
            .await?;
        let id = backend_string(&job, "id")?.to_string();
        if backend_string(&job, "session_id")? != canonical {
            return Err(RpcError::backend(
                "Claw assigned a different conversation to the submitted turn",
            ));
        }
        if job.get("requested_model").and_then(Value::as_str) != Some(model.as_str()) {
            return Err(RpcError::backend(
                "Claw did not acknowledge the requested per-task model",
            ));
        }
        super::protocol::token(&id, "task id")?;
        *thread.active.lock().await = Some(id.clone());
        *thread.model.lock().await = Some(model);
        self.driving.lock().await.insert(id.clone());
        let subscription = self.subscribe(&canonical).await;
        Ok(Outcome {
            result: json!({ "turn": turn_view(&id, Vec::new(), "inProgress", None, &job) }),
            notifications: Vec::new(),
            drive: Some(Drive {
                job,
                thread_id: history::frontend_id(&conversation)?.to_string(),
                client_message_id,
                subscription,
                slot,
            }),
        })
    }

    async fn interrupt_turn(&self, value: Value) -> Result<Outcome, RpcError> {
        let params = Params::new(value, &["threadId", "turnId"])?;
        let canonical = self
            .service
            .resolve(params.required_string("threadId")?)
            .await?;
        let expected = params
            .string("turnId")?
            .ok_or_else(|| RpcError::params("turnId is required"))?;
        let thread = self.service.loaded(&canonical).await?;
        let conversation = self.service.conversation(&canonical).await?;
        let job = self
            .service
            .active(&thread, &conversation)
            .await?
            .ok_or_else(|| RpcError::stale("No active turn to interrupt"))?;
        let id = backend_string(&job, "id")?;
        if !expected.is_empty() && expected != id {
            return Err(RpcError::stale(
                "turnId does not match the active Claw task",
            ));
        }
        let cancelled = self
            .service
            .backend
            .call(Operation::TaskCancel, json!({ "id": id }))
            .await?;
        if backend_string(&cancelled, "id")? != id {
            return Err(RpcError::backend(
                "Cancellation response has a different task id",
            ));
        }
        if cancelled.get("cancelled").and_then(Value::as_bool) != Some(true)
            && cancelled.get("cancel_requested").and_then(Value::as_bool) != Some(true)
        {
            return Err(RpcError::stale(
                "The task was already terminal before cancellation",
            ));
        }
        Ok(Outcome::result(json!({})))
    }

    pub async fn drive(&self, drive: Drive, outgoing: mpsc::Sender<Value>) {
        let Drive {
            job,
            thread_id,
            client_message_id,
            subscription,
            slot: _slot,
        } = drive;
        let id = match backend_string(&job, "id") {
            Ok(id) => id.to_string(),
            Err(_) => return,
        };
        let result = self
            .drive_inner(
                &thread_id,
                &job,
                client_message_id.as_deref(),
                &subscription,
                &outgoing,
            )
            .await;
        self.driving.lock().await.remove(&id);
        if let Err(error) = result {
            if let Some(canonical) = job.get("session_id").and_then(Value::as_str) {
                let projection = Projection::new(thread_id, canonical.to_string(), id);
                let _ = self.emit(&outgoing, projection.stream_error(&error)).await;
            }
        }
    }

    async fn drive_inner(
        &self,
        thread_id: &str,
        job: &Value,
        client_id: Option<&str>,
        subscription: &AtomicBool,
        outgoing: &mpsc::Sender<Value>,
    ) -> Result<(), RpcError> {
        let task_id = backend_string(job, "id")?;
        let canonical = backend_string(job, "session_id")?;
        let mut projection = Projection::new(
            thread_id.to_string(),
            canonical.to_string(),
            task_id.to_string(),
        );
        for message in projection.begin(backend_string(job, "prompt")?, client_id, job) {
            self.emit(outgoing, message).await?;
        }
        let mut cursor = 0u64;
        loop {
            if !subscription.load(Ordering::Acquire) || outgoing.is_closed() {
                return Ok(());
            }
            let frame = self
                .service
                .backend
                .call(
                    Operation::TaskStream,
                    json!({
                        "id": task_id, "cursor": cursor, "timeout_ms": 1000,
                    }),
                )
                .await?;
            let frame_job = frame
                .get("job")
                .ok_or_else(|| RpcError::backend("Task stream has no job"))?;
            if backend_string(frame_job, "id")? != task_id
                || backend_string(frame_job, "session_id")? != canonical
            {
                return Err(RpcError::backend("Task stream identity changed"));
            }
            let next = frame
                .get("cursor")
                .and_then(Value::as_u64)
                .ok_or_else(|| RpcError::backend("Task stream has no cursor"))?;
            let events = frame
                .get("events")
                .and_then(Value::as_array)
                .ok_or_else(|| RpcError::backend("Task stream has no events"))?;
            if next < cursor || (!events.is_empty() && next == cursor) || events.len() > 16_384 {
                return Err(RpcError::backend(
                    "Task stream returned an invalid cursor or event count",
                ));
            }
            cursor = next;
            for record in events {
                if !subscription.load(Ordering::Acquire) {
                    return Ok(());
                }
                let projected = projection.record(record)?;
                for message in projected.messages {
                    self.emit(outgoing, message).await?;
                }
                if projected.resumed {
                    for message in self.approvals.resolve_task(task_id).await {
                        self.emit(outgoing, message).await?;
                    }
                }
                if !projected.approvals.is_empty() {
                    if self.experimental.load(Ordering::Acquire) {
                        match self
                            .approvals
                            .present(
                                self.service.backend.as_ref(),
                                thread_id,
                                canonical,
                                task_id,
                                &projected.approvals,
                            )
                            .await
                        {
                            Ok(messages) => {
                                for message in messages {
                                    self.emit(outgoing, message).await?;
                                }
                            }
                            Err(error) => {
                                self.emit(
                                    outgoing,
                                    notification(
                                        "warning",
                                        json!({
                                            "threadId": thread_id, "message": error.message,
                                        }),
                                    ),
                                )
                                .await?
                            }
                        }
                    } else {
                        self.emit(outgoing, notification("warning", json!({
                            "threadId": thread_id,
                            "message": "Claw is waiting for a root-owned approval. Review it with `cos approval pending`; this client did not opt into structured input.",
                        }))).await?;
                    }
                }
            }
            match frame.get("terminal").and_then(Value::as_bool) {
                Some(true) => {
                    let final_job = frame
                        .get("job")
                        .ok_or_else(|| RpcError::backend("Terminal stream has no job"))?;
                    for message in self.approvals.resolve_task(task_id).await {
                        self.emit(outgoing, message).await?;
                    }
                    for message in projection.finish(final_job)? {
                        self.emit(outgoing, message).await?;
                    }
                    let thread = self.service.loaded(canonical).await?;
                    let mut active = thread.active.lock().await;
                    if active.as_deref() == Some(task_id) {
                        *active = None;
                    }
                    return Ok(());
                }
                Some(false) => {}
                None => return Err(RpcError::backend("Task stream has no terminal flag")),
            }
            if events.is_empty() {
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
        }
    }

    pub async fn emit(
        &self,
        outgoing: &mpsc::Sender<Value>,
        message: Value,
    ) -> Result<(), RpcError> {
        if message.to_string().len() > super::protocol::MAX_MESSAGE_BYTES {
            return Err(RpcError::capacity());
        }
        if message.get("id").is_none()
            && message
                .get("method")
                .and_then(Value::as_str)
                .is_some_and(|method| method.is_empty())
        {
            return Err(RpcError::backend("Cannot emit an empty method"));
        }
        if message.get("id").is_none() {
            if let Some(method) = message.get("method").and_then(Value::as_str) {
                if self.opted_out.lock().await.contains(method) {
                    return Ok(());
                }
            }
        }
        tokio::time::timeout(Duration::from_secs(5), outgoing.send(message))
            .await
            .map_err(|_| RpcError::unavailable("TUI event queue is full"))?
            .map_err(|_| RpcError::unavailable("TUI disconnected"))
    }
}
