use std::collections::{HashMap, VecDeque};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde_json::{json, Map, Value};
use tokio::sync::{mpsc, Mutex};
use tokio::task::{JoinError, JoinHandle};

use crate::generated::{
    normalize_mcp_call_context_integers, validate_mcp_call_context, McpCallContext,
};

use super::manifest::{self, ManifestTool};
use super::protocol::{
    ClientCapabilities, Implementation, InitializeParams, InitializeResult, ListToolsResult,
    ServerCapabilities, ToolDescriptor, ToolsCapability, ERR_INTERNAL, ERR_INVALID_PARAMS,
    ERR_INVALID_REQUEST, ERR_METHOD_NOT_FOUND, ERR_PARSE, JSONRPC_VERSION, PROTOCOL_VERSION,
};
use super::tool::{
    deadline_wait, CallContext, Cancellation, Progress, ProgressSink, Tool, ToolResult,
};
use super::transport::{Frame, StdioTransport, Transport, TransportError, MAX_FRAME_BYTES};

pub const CALL_CONTEXT_META_KEY: &str = "claw-os.dev/call-context";
pub const ERR_SERVER_BUSY: i64 = -32000;
const MAX_CALLS: usize = 64;
const INPUT_CHANNEL_CAPACITY: usize = 1;
const EOF_GRACE: Duration = Duration::from_millis(50);
const READER_TERMINATED: &str = "MCP reader task terminated unexpectedly";

#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("invalid App manifest: {0}")]
    Manifest(String),
    #[error("tool `{0}` is not declared in app.json.mcp.tools")]
    UndeclaredTool(String),
    #[error("tool `{0}` is already bound")]
    DuplicateBinding(String),
    #[error("missing handlers for manifest tools: {0}")]
    MissingBindings(String),
    #[error(transparent)]
    Transport(#[from] TransportError),
}

/// A manifest-bound MCP App service.
pub struct App {
    id: String,
    version: String,
    tools: Vec<ManifestTool>,
    tool_indexes: HashMap<String, usize>,
    bindings: HashMap<String, Arc<dyn Tool>>,
}

impl App {
    /// Load the authoritative App manifest from one bounded file snapshot.
    pub fn from_manifest(path: impl AsRef<Path>) -> Result<Self, AppError> {
        let manifest = manifest::load(path.as_ref())?;
        let tool_indexes = manifest
            .tools
            .iter()
            .enumerate()
            .map(|(index, tool)| (tool.name.clone(), index))
            .collect();
        Ok(Self {
            id: manifest.id,
            version: manifest.version,
            tools: manifest.tools,
            tool_indexes,
            bindings: HashMap::new(),
        })
    }

    /// Load `COS_APP_MANIFEST`, falling back to `app.json` for direct
    /// development runs.
    pub fn from_environment() -> Result<Self, AppError> {
        let path = std::env::var_os("COS_APP_MANIFEST")
            .filter(|path| !path.is_empty())
            .unwrap_or_else(|| "app.json".into());
        Self::from_manifest(path)
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn version(&self) -> &str {
        &self.version
    }

    /// Bind implementation to a tool name already declared by the manifest.
    pub fn bind(&mut self, tool: Arc<dyn Tool>) -> Result<(), AppError> {
        let name = tool.name().to_string();
        if !self.tool_indexes.contains_key(&name) {
            return Err(AppError::UndeclaredTool(name));
        }
        if self.bindings.contains_key(&name) {
            return Err(AppError::DuplicateBinding(name));
        }
        self.bindings.insert(name, tool);
        Ok(())
    }

    pub async fn serve_stdio(self) -> Result<(), AppError> {
        self.serve(StdioTransport::stdio()).await
    }

    pub async fn serve(self, transport: impl Transport) -> Result<(), AppError> {
        let missing: Vec<&str> = self
            .tools
            .iter()
            .filter(|tool| !self.bindings.contains_key(&tool.name))
            .map(|tool| tool.name.as_str())
            .collect();
        if !missing.is_empty() {
            return Err(AppError::MissingBindings(missing.join(", ")));
        }
        Runtime::new(self, Arc::new(transport)).run().await
    }
}

struct Output {
    transport: Arc<dyn Transport>,
    write_lock: Mutex<()>,
    fatal: mpsc::UnboundedSender<TransportError>,
}

impl Output {
    async fn send(&self, value: Value) -> Result<(), TransportError> {
        let frame = match serde_json::to_string(&value) {
            Ok(frame) => frame,
            Err(error) => {
                let error = TransportError::Encode(error.to_string());
                let _ = self.fatal.send(error.clone());
                return Err(error);
            }
        };
        let _guard = self.write_lock.lock().await;
        if let Err(error) = self.transport.send(frame).await {
            let _ = self.fatal.send(error.clone());
            return Err(error);
        }
        Ok(())
    }
}

#[async_trait]
impl ProgressSink for Output {
    async fn emit_progress(
        &self,
        token: Value,
        progress: f64,
        update: Progress,
    ) -> Result<(), TransportError> {
        let mut params = Map::from_iter([
            ("progressToken".into(), token),
            ("progress".into(), json!(progress)),
        ]);
        if let Some(total) = update.total {
            params.insert("total".into(), json!(total));
        }
        if let Some(message) = update.message {
            params.insert("message".into(), Value::String(message));
        }
        self.send(json!({
            "jsonrpc": JSONRPC_VERSION,
            "method": "notifications/progress",
            "params": params
        }))
        .await
    }
}

struct CallState {
    key: String,
    id: Value,
    authenticated_call_id: String,
    cancellation: Arc<Cancellation>,
    suppress_response: AtomicBool,
}

struct PendingCall {
    state: Arc<CallState>,
    tool: Arc<dyn Tool>,
    args: Value,
    context: CallContext,
}

struct ActiveCall {
    state: Arc<CallState>,
    context: CallContext,
    handle: JoinHandle<ToolResult>,
    deadline_unix_ms: Option<u64>,
}

struct Runtime {
    id: String,
    version: String,
    tools: Vec<ManifestTool>,
    tool_indexes: HashMap<String, usize>,
    bindings: HashMap<String, Arc<dyn Tool>>,
    transport: Arc<dyn Transport>,
    output: Arc<Output>,
    fatal_rx: mpsc::UnboundedReceiver<TransportError>,
    calls: HashMap<String, Arc<CallState>>,
    pending: VecDeque<PendingCall>,
    active: Option<ActiveCall>,
}

impl Runtime {
    fn new(app: App, transport: Arc<dyn Transport>) -> Self {
        let (fatal_tx, fatal_rx) = mpsc::unbounded_channel();
        let output = Arc::new(Output {
            transport: transport.clone(),
            write_lock: Mutex::new(()),
            fatal: fatal_tx,
        });
        Self {
            id: app.id,
            version: app.version,
            tools: app.tools,
            tool_indexes: app.tool_indexes,
            bindings: app.bindings,
            transport,
            output,
            fatal_rx,
            calls: HashMap::new(),
            pending: VecDeque::new(),
            active: None,
        }
    }

    async fn run(mut self) -> Result<(), AppError> {
        let (input_tx, mut input_rx) = mpsc::channel(INPUT_CHANNEL_CAPACITY);
        let transport = self.transport.clone();
        let reader = tokio::spawn(read_input(transport, input_tx));
        let result = self.run_loop(&mut input_rx).await;
        drop(input_rx);

        let reader_was_running = !reader.is_finished();
        if reader_was_running {
            reader.abort();
        }
        let reader_result = reader.await;
        let reader_terminated = matches!(
            &result,
            Err(AppError::Transport(TransportError::Io(message)))
                if message == READER_TERMINATED
        );
        match reader_result {
            Ok(()) => result,
            Err(error) if error.is_cancelled() && reader_was_running => result,
            Err(error) if reader_terminated => {
                Err(TransportError::Io(format!("MCP reader task failed: {error}")).into())
            }
            Err(_) => result,
        }
    }

    async fn run_loop(
        &mut self,
        input_rx: &mut mpsc::Receiver<ReaderEvent>,
    ) -> Result<(), AppError> {
        let mut eof_grace = None;
        loop {
            self.start_next();
            if eof_grace.is_some() && self.active.is_none() && self.pending.is_empty() {
                return Ok(());
            }
            let event = if let Some(active) = self.active.as_mut() {
                let deadline_unix_ms = active.deadline_unix_ms;
                if let Some(eof_deadline) = eof_grace {
                    tokio::select! {
                        biased;
                        joined = &mut active.handle => Event::Completed(joined),
                        failure = self.fatal_rx.recv() => Event::Fatal(failure),
                        _ = wait_for_deadline(deadline_unix_ms) => Event::Deadline,
                        _ = tokio::time::sleep_until(eof_deadline) => Event::EofGraceExpired,
                    }
                } else {
                    tokio::select! {
                        biased;
                        joined = &mut active.handle => Event::Completed(joined),
                        failure = self.fatal_rx.recv() => Event::Fatal(failure),
                        input = input_rx.recv() => Event::Input(input),
                        _ = wait_for_deadline(deadline_unix_ms) => Event::Deadline,
                    }
                }
            } else if let Some(eof_deadline) = eof_grace {
                tokio::select! {
                    biased;
                    failure = self.fatal_rx.recv() => Event::Fatal(failure),
                    _ = tokio::time::sleep_until(eof_deadline) => Event::EofGraceExpired,
                }
            } else {
                tokio::select! {
                    biased;
                    failure = self.fatal_rx.recv() => Event::Fatal(failure),
                    input = input_rx.recv() => Event::Input(input),
                }
            };
            match event {
                Event::Completed(joined) => {
                    if let Err(error) = self.finish_active(joined).await {
                        self.abort_all("MCP output failed", true).await;
                        return Err(error.into());
                    }
                }
                Event::Deadline => {
                    if let Err(error) = self.expire_active().await {
                        self.abort_all("MCP output failed", true).await;
                        return Err(error.into());
                    }
                }
                Event::Fatal(Some(error)) => {
                    self.abort_all("MCP output failed", true).await;
                    return Err(error.into());
                }
                Event::Fatal(None) => {}
                Event::Input(Some(ReaderEvent::Frame(frame))) => {
                    if let Err(error) = self.handle_frame(frame).await {
                        self.abort_all("MCP output failed", true).await;
                        return Err(error.into());
                    }
                }
                Event::Input(Some(ReaderEvent::Eof)) => {
                    eof_grace = Some(tokio::time::Instant::now() + EOF_GRACE);
                }
                Event::Input(Some(ReaderEvent::Error(error))) => {
                    self.abort_all("MCP input failed", true).await;
                    return Err(error.into());
                }
                Event::Input(None) => {
                    self.abort_all("MCP input failed", true).await;
                    return Err(TransportError::Io(READER_TERMINATED.into()).into());
                }
                Event::EofGraceExpired => {
                    self.abort_all("MCP input closed", true).await;
                    return Ok(());
                }
            }
        }
    }

    fn start_next(&mut self) {
        if self.active.is_some() {
            return;
        }
        while let Some(call) = self.pending.pop_front() {
            if call.state.suppress_response.load(Ordering::Acquire) {
                self.calls.remove(&call.state.key);
                continue;
            }
            let tool = call.tool;
            let args = call.args;
            let context = call.context;
            let handler_context = context.clone();
            let deadline_unix_ms = context.deadline_unix_ms();
            let handle = tokio::spawn(async move { tool.handle(args, handler_context).await });
            self.active = Some(ActiveCall {
                state: call.state,
                context,
                handle,
                deadline_unix_ms,
            });
            break;
        }
    }

    async fn finish_active(
        &mut self,
        joined: Result<ToolResult, JoinError>,
    ) -> Result<(), TransportError> {
        let active = self.active.take().expect("completed call must be active");
        self.calls.remove(&active.state.key);
        if active.state.suppress_response.load(Ordering::Acquire) {
            return Ok(());
        }
        let result = match joined {
            Ok(result) => match active.context.check_cancelled() {
                Ok(()) => result,
                Err(error) => ToolResult::error(error.to_string()),
            },
            Err(error) if error.is_panic() => {
                ToolResult::error(format!("MCP tool handler panicked: {error}"))
            }
            Err(error) => ToolResult::error(format!("MCP tool handler failed: {error}")),
        };
        self.send_tool_result(active.state.id.clone(), result).await
    }

    async fn expire_active(&mut self) -> Result<(), TransportError> {
        let active = self.active.take().expect("deadline requires active call");
        let message = format!("call `{}` exceeded its deadline", active.context.call_id());
        active.state.cancellation.cancel(message.clone()).await;
        active.handle.abort();
        let _ = active.handle.await;
        self.calls.remove(&active.state.key);
        if !active.state.suppress_response.load(Ordering::Acquire) {
            self.send_tool_result(active.state.id.clone(), ToolResult::error(message))
                .await?;
        }
        Ok(())
    }

    async fn handle_frame(&mut self, frame: Frame) -> Result<(), TransportError> {
        let text = match frame {
            Frame::Oversized => {
                return self
                    .send_error(
                        Value::Null,
                        ERR_PARSE,
                        format!("frame exceeds {MAX_FRAME_BYTES} bytes; rejected"),
                    )
                    .await;
            }
            Frame::InvalidUtf8 => {
                return self
                    .send_error(Value::Null, ERR_PARSE, "frame is not valid UTF-8")
                    .await;
            }
            Frame::Message(text) if text.len() > MAX_FRAME_BYTES => {
                return self
                    .send_error(
                        Value::Null,
                        ERR_PARSE,
                        format!("frame exceeds {MAX_FRAME_BYTES} bytes; rejected"),
                    )
                    .await;
            }
            Frame::Message(text) => text,
        };
        let message: Value = match serde_json::from_str(&text) {
            Ok(message) => message,
            Err(error) => {
                return self
                    .send_error(Value::Null, ERR_PARSE, format!("parse error: {error}"))
                    .await;
            }
        };
        let Some(object) = message.as_object() else {
            return self
                .send_error(Value::Null, ERR_INVALID_REQUEST, "request not an object")
                .await;
        };
        let has_id = object.contains_key("id");
        let id = object.get("id").cloned().unwrap_or(Value::Null);
        if has_id && !valid_request_id(&id) {
            return self
                .send_error(
                    Value::Null,
                    ERR_INVALID_REQUEST,
                    "request id must be a string, number, or null",
                )
                .await;
        }
        if object.get("jsonrpc").and_then(Value::as_str) != Some(JSONRPC_VERSION) {
            return self
                .send_error(id, ERR_INVALID_REQUEST, "missing jsonrpc 2.0 envelope")
                .await;
        }
        let Some(method) = object.get("method").and_then(Value::as_str) else {
            return self
                .send_error(id, ERR_INVALID_REQUEST, "request method must be a string")
                .await;
        };
        let params = object.get("params");
        if !has_id {
            if params.is_some_and(|params| !params.is_object() && !params.is_array()) {
                return self
                    .send_error(
                        Value::Null,
                        ERR_INVALID_REQUEST,
                        "request params must be an object or array",
                    )
                    .await;
            }
            self.handle_notification(method, params).await;
            return Ok(());
        }
        match method {
            "initialize" => self.handle_initialize(id, params).await,
            "ping" => {
                self.handle_ping(id, params, object.contains_key("params"))
                    .await
            }
            "tools/list" => {
                self.handle_tools_list(id, params, object.contains_key("params"))
                    .await
            }
            "tools/call" => self.queue_tool_call(id, params).await,
            _ => {
                self.send_error(
                    id,
                    ERR_METHOD_NOT_FOUND,
                    format!("unknown method `{method}`"),
                )
                .await
            }
        }
    }

    async fn handle_notification(&mut self, method: &str, params: Option<&Value>) {
        if method != "notifications/cancelled" {
            return;
        }
        let Some(request_id) = params
            .and_then(Value::as_object)
            .and_then(|params| params.get("requestId"))
            .filter(|request_id| valid_request_id(request_id))
        else {
            return;
        };
        let Ok(key) = request_key(request_id) else {
            return;
        };
        if let Some(state) = self.calls.get(&key).cloned() {
            state.suppress_response.store(true, Ordering::Release);
            state
                .cancellation
                .cancel(format!(
                    "call `{}` was cancelled",
                    state.authenticated_call_id
                ))
                .await;
        }
    }

    async fn handle_initialize(
        &self,
        id: Value,
        params: Option<&Value>,
    ) -> Result<(), TransportError> {
        let Some(params) = params.and_then(Value::as_object) else {
            return self
                .send_error(
                    id,
                    ERR_INVALID_PARAMS,
                    "initialize params must be an object",
                )
                .await;
        };
        let parsed: InitializeParams = match serde_json::from_value(Value::Object(params.clone())) {
            Ok(parsed) => parsed,
            Err(error) => {
                return self
                    .send_error(id, ERR_INVALID_PARAMS, error.to_string())
                    .await;
            }
        };
        if let Err(message) = validate_initialize(&parsed, params) {
            return self.send_error(id, ERR_INVALID_PARAMS, message).await;
        }
        let result = InitializeResult {
            protocol_version: PROTOCOL_VERSION.into(),
            capabilities: ServerCapabilities {
                tools: Some(ToolsCapability {
                    list_changed: Some(false),
                }),
                ..Default::default()
            },
            server_info: Implementation {
                name: self.id.clone(),
                version: self.version.clone(),
            },
            instructions: None,
        };
        self.send_result(id, serde_json::to_value(result).map_err(encode_error)?)
            .await
    }

    async fn handle_ping(
        &self,
        id: Value,
        params: Option<&Value>,
        params_present: bool,
    ) -> Result<(), TransportError> {
        if params_present && !params.is_some_and(Value::is_object) {
            return self
                .send_error(id, ERR_INVALID_PARAMS, "ping params must be an object")
                .await;
        }
        self.send_result(id, json!({})).await
    }

    async fn handle_tools_list(
        &self,
        id: Value,
        params: Option<&Value>,
        params_present: bool,
    ) -> Result<(), TransportError> {
        if params_present {
            let Some(params) = params.and_then(Value::as_object) else {
                return self
                    .send_error(
                        id,
                        ERR_INVALID_PARAMS,
                        "tools/list params must be an object",
                    )
                    .await;
            };
            if params
                .get("cursor")
                .is_some_and(|cursor| !cursor.is_string())
            {
                return self
                    .send_error(id, ERR_INVALID_PARAMS, "tools/list cursor must be a string")
                    .await;
            }
        }
        let result = ListToolsResult {
            tools: self
                .tools
                .iter()
                .map(|tool| ToolDescriptor {
                    name: tool.name.clone(),
                    description: Some(tool.summary.clone()),
                    input_schema: tool.input_schema.clone(),
                })
                .collect(),
            next_cursor: None,
        };
        self.send_result(id, serde_json::to_value(result).map_err(encode_error)?)
            .await
    }

    async fn queue_tool_call(
        &mut self,
        id: Value,
        params: Option<&Value>,
    ) -> Result<(), TransportError> {
        let Some(params) = params.and_then(Value::as_object) else {
            return self
                .send_error(
                    id,
                    ERR_INVALID_PARAMS,
                    "tools/call params must be an object",
                )
                .await;
        };
        if self.calls.len() >= MAX_CALLS {
            return self
                .send_error(id, ERR_SERVER_BUSY, "too many pending MCP tool calls")
                .await;
        }
        let key = request_key(&id).map_err(encode_error)?;
        if self.calls.contains_key(&key) {
            return self
                .send_error(id, ERR_INVALID_REQUEST, "duplicate active request id")
                .await;
        }
        let Some(name) = params.get("name").and_then(Value::as_str) else {
            return self
                .send_error(id, ERR_INVALID_PARAMS, "missing `name`")
                .await;
        };
        let Some(tool_index) = self.tool_indexes.get(name).copied() else {
            return self
                .send_error(id, ERR_INVALID_PARAMS, format!("unknown tool `{name}`"))
                .await;
        };
        let arguments = match params.get("arguments") {
            Some(Value::Object(arguments)) => arguments,
            Some(_) => {
                return self
                    .send_error(id, ERR_INVALID_PARAMS, "`arguments` must be an object")
                    .await;
            }
            None => &Map::new(),
        };
        let (authenticated, progress_token) = match parse_call_context(params) {
            Ok(context) => context,
            Err(error) => return self.send_error(id, error.code, error.message).await,
        };
        let manifest_tool = &self.tools[tool_index];
        let args = match manifest::resolve_arguments(manifest_tool, arguments) {
            Ok(args) => args,
            Err(error) => {
                return self
                    .send_tool_result(
                        id,
                        ToolResult::error(format!("bad arguments for `{name}`: {error}")),
                    )
                    .await;
            }
        };
        let cancellation = Arc::new(Cancellation::new());
        let authenticated_call_id = authenticated.call_id.clone();
        let context = CallContext::new(
            authenticated,
            cancellation.clone(),
            progress_token,
            self.output.clone(),
        );
        let state = Arc::new(CallState {
            key: key.clone(),
            id,
            authenticated_call_id,
            cancellation,
            suppress_response: AtomicBool::new(false),
        });
        let tool = self
            .bindings
            .get(name)
            .expect("bindings validated before serving")
            .clone();
        self.calls.insert(key, state.clone());
        self.pending.push_back(PendingCall {
            state,
            tool,
            args,
            context,
        });
        Ok(())
    }

    async fn send_tool_result(&self, id: Value, result: ToolResult) -> Result<(), TransportError> {
        let mut payload = Map::from_iter([
            (
                "content".into(),
                json!([{"type": "text", "text": result.text}]),
            ),
            ("isError".into(), Value::Bool(result.is_error)),
        ]);
        if let Some(structured) = result.structured_content {
            payload.insert("structuredContent".into(), Value::Object(structured));
        }
        self.send_result(id, Value::Object(payload)).await
    }

    async fn send_result(&self, id: Value, result: Value) -> Result<(), TransportError> {
        self.output
            .send(json!({
                "jsonrpc": JSONRPC_VERSION,
                "id": id,
                "result": result
            }))
            .await
    }

    async fn send_error(
        &self,
        id: Value,
        code: i64,
        message: impl Into<String>,
    ) -> Result<(), TransportError> {
        self.output
            .send(json!({
                "jsonrpc": JSONRPC_VERSION,
                "id": id,
                "error": {"code": code, "message": message.into()}
            }))
            .await
    }

    async fn abort_all(&mut self, reason: &str, suppress: bool) {
        let states: Vec<Arc<CallState>> = self.calls.values().cloned().collect();
        for state in states {
            if suppress {
                state.suppress_response.store(true, Ordering::Release);
            }
            state.cancellation.cancel(reason).await;
        }
        self.pending.clear();
        self.calls.clear();
        if let Some(active) = self.active.take() {
            active.handle.abort();
            let _ = active.handle.await;
        }
    }
}

enum Event {
    Input(Option<ReaderEvent>),
    Completed(Result<ToolResult, JoinError>),
    Deadline,
    EofGraceExpired,
    Fatal(Option<TransportError>),
}

enum ReaderEvent {
    Frame(Frame),
    Eof,
    Error(TransportError),
}

struct RpcError {
    code: i64,
    message: String,
}

fn parse_call_context(
    params: &Map<String, Value>,
) -> Result<(McpCallContext, Option<Value>), RpcError> {
    let meta = params
        .get("_meta")
        .and_then(Value::as_object)
        .ok_or_else(|| RpcError {
            code: ERR_INVALID_PARAMS,
            message: "`_meta` must be an object".into(),
        })?;
    let progress_token = match meta.get("progressToken") {
        Some(Value::String(token)) => Some(Value::String(token.clone())),
        Some(Value::Number(number)) if manifest::number_is_integer(number) => {
            Some(Value::Number(number.clone()))
        }
        Some(_) => {
            return Err(RpcError {
                code: ERR_INVALID_PARAMS,
                message: "`_meta.progressToken` must be a string or integer".into(),
            });
        }
        None => None,
    };
    let raw_context = meta.get(CALL_CONTEXT_META_KEY).ok_or_else(|| RpcError {
        code: ERR_INVALID_PARAMS,
        message: format!("missing authenticated `{CALL_CONTEXT_META_KEY}`"),
    })?;
    validate_mcp_call_context(raw_context).map_err(|error| RpcError {
        code: ERR_INVALID_PARAMS,
        message: format!("invalid authenticated call context: {error}"),
    })?;
    let mut normalized = raw_context.clone();
    normalize_mcp_call_context_integers(&mut normalized);
    let authenticated = serde_json::from_value(normalized).map_err(|error| RpcError {
        code: ERR_INTERNAL,
        message: format!("cannot materialize authenticated call context: {error}"),
    })?;
    Ok((authenticated, progress_token))
}

fn validate_initialize(parsed: &InitializeParams, raw: &Map<String, Value>) -> Result<(), String> {
    let capabilities = raw
        .get("capabilities")
        .and_then(Value::as_object)
        .ok_or_else(|| "missing `capabilities`".to_string())?;
    for name in ["experimental", "sampling", "elicitation"] {
        if capabilities
            .get(name)
            .is_some_and(|value| !value.is_object())
        {
            return Err(format!("`capabilities.{name}` must be an object"));
        }
    }
    if let Some(roots) = capabilities.get("roots") {
        let roots = roots
            .as_object()
            .ok_or_else(|| "`capabilities.roots` must be an object".to_string())?;
        if roots
            .get("listChanged")
            .is_some_and(|value| !value.is_boolean())
        {
            return Err("`capabilities.roots.listChanged` must be a boolean".into());
        }
    }
    if parsed.protocol_version.is_empty()
        || parsed.client_info.name.is_empty()
        || parsed.client_info.version.is_empty()
    {
        return Err("missing or invalid `clientInfo`".into());
    }
    let _capabilities: &ClientCapabilities = &parsed.capabilities;
    Ok(())
}

fn valid_request_id(value: &Value) -> bool {
    value.is_null() || value.is_string() || value.is_number()
}

fn request_key(value: &Value) -> Result<String, serde_json::Error> {
    serde_json::to_string(value)
}

fn encode_error(error: serde_json::Error) -> TransportError {
    TransportError::Encode(error.to_string())
}

async fn read_input(transport: Arc<dyn Transport>, input: mpsc::Sender<ReaderEvent>) {
    loop {
        let event = match transport.recv().await {
            Ok(Some(frame)) => ReaderEvent::Frame(frame),
            Ok(None) => ReaderEvent::Eof,
            Err(error) => ReaderEvent::Error(error),
        };
        let terminal = matches!(event, ReaderEvent::Eof | ReaderEvent::Error(_));
        if input.send(event).await.is_err() || terminal {
            return;
        }
    }
}

async fn wait_for_deadline(deadline_unix_ms: Option<u64>) {
    match deadline_unix_ms {
        Some(deadline_unix_ms) => loop {
            let Some(wait) = deadline_wait(deadline_unix_ms) else {
                return;
            };
            tokio::time::sleep(wait).await;
        },
        None => std::future::pending().await,
    }
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/mcp/server.rs"
    ));
}
