use super::{llm, memory, runtime, setup};
use serde_json::{json, Value};

pub(super) fn ask_cmd(args: &[String]) -> Result<Value, String> {
    let mut full = false;
    let mut session_id: Option<String> = None;
    let mut timeout_ms: Option<u64> = None;
    let mut positional: Vec<String> = Vec::with_capacity(args.len());
    let mut i = 0usize;
    while i < args.len() {
        match args[i].as_str() {
            "--full" => {
                full = true;
                i += 1;
            }
            "--no-full" => {
                full = false;
                i += 1;
            }
            "--session" => {
                let value = args
                    .get(i + 1)
                    .filter(|value| !value.trim().is_empty())
                    .ok_or_else(|| "--session needs a non-empty id".to_string())?;
                session_id = Some(value.clone());
                i += 2;
            }
            "--timeout-secs" => {
                let value = args
                    .get(i + 1)
                    .ok_or_else(|| "--timeout-secs needs a positive integer".to_string())?;
                let seconds = value
                    .parse::<u64>()
                    .map_err(|_| "--timeout-secs needs a positive integer".to_string())?;
                if seconds == 0 {
                    return Err("--timeout-secs needs a positive integer".to_string());
                }
                timeout_ms = Some(
                    seconds
                        .checked_mul(1_000)
                        .ok_or_else(|| "--timeout-secs is too large".to_string())?,
                );
                i += 2;
            }
            other if other.starts_with("--") => {
                return Err(format!(
                    "unknown ask flag: {other}. supported: --full | --no-full | --session <id> | --timeout-secs <n>"
                ));
            }
            _ => {
                positional.push(args[i].clone());
                i += 1;
            }
        }
    }
    let prompt = positional.first().cloned().unwrap_or_default();
    if prompt.is_empty() {
        return Err(
            "usage: cos agent ask \"<prompt>\" [--full] [--session <id>] [--timeout-secs <n>]"
                .into(),
        );
    }
    let envelope = match timeout_ms {
        Some(timeout_ms) => crate::clawd::agent_client::ask_in_session_with_timeout(
            &prompt,
            session_id.as_deref(),
            timeout_ms,
        )?,
        None => crate::clawd::agent_client::ask_in_session(&prompt, session_id.as_deref())?,
    };
    if full {
        Ok(envelope)
    } else {
        match envelope.get("answer").and_then(|value| value.as_str()) {
            Some(answer) => {
                println!("{answer}");
                Ok(Value::Null)
            }
            None => Ok(envelope),
        }
    }
}

/// Recover from a poisoned [`std::sync::Mutex`] by taking the inner
/// data. Poisoning means a previous holder panicked, but for the
/// `LiveSink` / `ChatSink` aggregators that's strictly informational
/// — none of the data they hold becomes corrupted by a panic in
/// another tool-call thread, so silently dropping the poison flag
/// keeps the rest of the run going instead of aborting it. Callers
/// that need to surface partial state to JSON / stderr would
/// otherwise inherit a cascade of `.unwrap()` panics.
#[inline]
fn mlock<T>(m: &std::sync::Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

fn should_render_evidence_warning(
    stdout_is_tty: bool,
    status: &crate::agent::runtime::evidence::EvidenceStatus,
) -> bool {
    !stdout_is_tty
        && !matches!(
            status,
            crate::agent::runtime::evidence::EvidenceStatus::Verified
                | crate::agent::runtime::evidence::EvidenceStatus::NotRequired
        )
}

fn format_tool_failure(name: &str, latency_ms: u64, content_preview: &str) -> String {
    let elapsed =
        crate::agent::display::format_duration(std::time::Duration::from_millis(latency_ms));
    let header = format!("[tool failed: {name} after {elapsed}]");
    let detail = content_preview.trim();
    if detail.is_empty() {
        header
    } else {
        format!("{header}\n{detail}")
    }
}

#[derive(Debug, Default)]
struct TerminalOutputState {
    line_open: bool,
}

impl TerminalOutputState {
    fn reset(&mut self) {
        self.line_open = false;
    }

    fn write_text(&mut self, out: &mut impl std::io::Write, text: &str) {
        if text.is_empty() {
            return;
        }
        let _ = out.write_all(text.as_bytes());
        self.line_open = !text.ends_with('\n');
    }

    fn write_line(&mut self, out: &mut impl std::io::Write, line: &str) {
        if self.line_open {
            let _ = writeln!(out);
        }
        let _ = writeln!(out, "{line}");
        self.line_open = false;
    }

    fn finish_line(&mut self, out: &mut impl std::io::Write) {
        if self.line_open {
            let _ = writeln!(out);
            self.line_open = false;
        }
    }
}

/// `cos agent providers [--names <a,b,c>] [--probe-credentials]`
/// — diagnostic snapshot of every linked LLM provider plus the
/// canonical credential surface that would configure each one.
///
/// For the *active* provider (`config.agent.provider`) the user's
/// real `AgentConfig` is used so `is_configured` reflects what the
/// runtime actually sees. For the others a synthetic config is
/// substituted that hard-codes the canonical env-var + credential
/// names per alias (the convention this binary documents); that
/// way the answer to "what would happen if I switched my config
/// to provider X right now?" is honest, not a misleading
/// `not_configured` from a default-empty config.
///
/// `--probe-credentials` additionally scans the credential store
/// directly via `crate::credential::try_load(name, "agent")`. This
/// is opt-in because the probe touches `<data_dir>/credentials/`
/// which can be slow on networked storage; the env-var probe is
/// always cheap and always on.
/// Internal: single-turn streaming helper. The async core that the
/// removed `cos agent stream` CLI used to call. Kept as a helper
/// so the streaming unit tests still exercise text accumulation,
/// tool-call surfacing, warnings, etc. on the no-tools / no-memory
/// path. Not reachable from any CLI today — `cos agent ask
/// --stream` uses `live_cmd_async` (the full agent loop) instead.
async fn stream_cmd_async(
    provider: std::sync::Arc<dyn llm::Provider>,
    cfg: &crate::config::AgentConfig,
    user_prompt: &str,
) -> Result<Value, String> {
    use crate::agent::llm::types::{
        ChatRequest, FinishReason, Message, StreamEvent, ToolChoice, Usage,
    };
    use futures_util::StreamExt;
    use std::io::Write;

    let extra = cfg.system_prompt_path.as_deref().map(std::path::Path::new);
    let system = crate::agent::prompt::build_system_prompt_for(extra, Some(user_prompt));

    let request = ChatRequest {
        model: cfg.model.clone(),
        messages: vec![Message::user_text(user_prompt)],
        system: Some(system),
        tools: Vec::new(),
        tool_choice: ToolChoice::Auto,
        max_tokens: Some(cfg.max_tokens),
        temperature: Some(cfg.temperature),
        top_p: None,
        stop_sequences: Vec::new(),
        extra: serde_json::Value::Null,
    };

    let mut stream = provider
        .chat_stream(request)
        .await
        .map_err(|e| format!("chat_stream: {e}"))?;

    let mut answer = String::new();
    let mut finish: Option<FinishReason> = None;
    let mut usage = Usage::default();
    let mut tool_calls: Vec<serde_json::Value> = Vec::new();
    let mut warnings: Vec<String> = Vec::new();

    let stderr = std::io::stderr();
    let mut err_lock = stderr.lock();

    while let Some(event) = stream.next().await {
        match event {
            Ok(StreamEvent::TextDelta { text }) => {
                answer.push_str(&text);
                let _ = err_lock.write_all(text.as_bytes());
                let _ = err_lock.flush();
            }
            Ok(StreamEvent::ToolUseStart { id, name }) => {
                let _ = writeln!(err_lock, "\n[tool_use_start id={id} name={name}]");
            }
            Ok(StreamEvent::ToolInputDelta { partial_json, .. }) => {
                let _ = err_lock.write_all(partial_json.as_bytes());
                let _ = err_lock.flush();
            }
            Ok(StreamEvent::ToolUse(call)) => {
                let _ = writeln!(
                    err_lock,
                    "\n[tool_use id={} name={}] {}",
                    call.id, call.name, call.input
                );
                tool_calls.push(serde_json::json!({
                    "id": call.id,
                    "name": call.name,
                    "input": call.input,
                }));
            }
            Ok(StreamEvent::Reasoning { .. }) => {}
            Ok(StreamEvent::ToolState { .. }) => {}
            Ok(StreamEvent::Message(resp)) => {
                // Non-streaming providers (mock / openai_compat /
                // gemini / bedrock / llama_local today) emit the
                // whole response as a single Message event. Render
                // its assembled text and tool_calls so the UX
                // still looks like a stream.
                for block in &resp.content {
                    if let crate::agent::llm::types::ContentBlock::Text { text } = block {
                        answer.push_str(text);
                        let _ = err_lock.write_all(text.as_bytes());
                    }
                }
                for call in &resp.tool_calls {
                    let _ = writeln!(
                        err_lock,
                        "\n[tool_use id={} name={}] {}",
                        call.id, call.name, call.input
                    );
                    tool_calls.push(serde_json::json!({
                        "id": call.id,
                        "name": call.name,
                        "input": call.input,
                    }));
                }
                let _ = err_lock.flush();
            }
            Ok(StreamEvent::Done {
                finish: f,
                usage: u,
            }) => {
                finish = Some(f);
                usage = u;
                let _ = writeln!(err_lock);
                let _ = err_lock.flush();
            }
            Ok(StreamEvent::Warning { message }) => {
                let _ = writeln!(err_lock, "\n[warning] {message}");
                warnings.push(message);
            }
            Err(e) => {
                let _ = writeln!(err_lock, "\n[error] {e}");
                return Err(format!("stream error: {e}"));
            }
        }
    }

    Ok(json!({
        "answer": answer,
        "finish": finish.map(|f| format!("{f:?}")),
        "tool_calls": tool_calls,
        "usage": {
            "input_tokens": usage.input_tokens,
            "output_tokens": usage.output_tokens,
            "cache_read_tokens": usage.cache_read_tokens,
            "cache_write_tokens": usage.cache_write_tokens,
        },
        "warnings": warnings,
        "provider": provider.name(),
        "model": cfg.model,
    }))
}

/// `cos agent chat [--session <id>] [--no-stream] [--no-memory]
/// [--show-tools] [--max-turns N]` — interactive multi-turn REPL.
///
/// Reads prompts from stdin one line at a time and routes each
/// through the same agent runtime as `cos agent live`. With memory
/// enabled, the session-id is preserved across turns so:
///   1. Every prompt and assistant turn is recorded under the
///      same FTS-searchable conversation;
///   2. The session title is generated once on the first turn
///      (matches `ask`/`live` semantics);
///   3. Recent turns are replayed directly into each model request,
///      so short follow-ups such as "1" retain conversational context;
///   4. `cos_recall` invocations from inside the model can search
///      the running conversation as it grows.
///
/// **Slash commands** (recognised at the start of a non-empty
/// prompt; whitespace-trimmed):
///   - `/quit` / `/exit` / `/q` — leave the REPL.
///   - `/help` / `/?` — print the slash-command list.
///   - `/session` — print current session id and turn count.
///   - `/clear` — drop the current session and start a fresh one.
///   - `/history [N]` — show the last N (default 10) recorded
///     messages from the current session.
///   - `/tools` — list permitted tool names.
/// Any line that doesn't start with `/` is treated as a prompt.
///
/// Streaming behaviour mirrors `live`: tokens flow live to stderr;
/// the assistant's final text plus a one-line summary go to
/// stdout after each turn. Pass `--no-stream` to use the equivalent
/// non-streaming continuation path (useful for non-TTY use).
///
/// Stdin EOF (Ctrl+D / closed pipe) exits cleanly.
///
/// ## What this is **not**
///
/// `cos agent chat` is the kernel Agent's own REPL — it is *not*
/// an App entry point. Installed Apps that want a one-shot LLM call
/// must use `cos ai chat --app <id>` instead. Passing `--app` to
/// `cos agent chat` is rejected; the App-gated path lives under
/// `cos ai chat` so the kernel Agent's CLI surface (memory, skills,
/// hooks, sessions, recall, …) is never exposed to third-party Apps.
pub(super) fn chat_cmd(args: &[String]) -> Result<Value, String> {
    if args.iter().any(|a| a == "--app") {
        return Err(
            "`cos agent chat` is the kernel Agent's REPL and does not accept --app. \
             For one-shot App-gated calls use `cos ai chat --app <id> …` instead."
                .to_string(),
        );
    }

    let mut explicit_session: Option<String> = None;
    let mut streaming = true;
    let mut use_memory = true;
    let mut show_tools = false;
    let mut max_turns_override: Option<u32> = None;

    let mut i = 0usize;
    while i < args.len() {
        match args[i].as_str() {
            "--session" => {
                let v = args
                    .get(i + 1)
                    .ok_or_else(|| "--session needs <id>".to_string())?;
                explicit_session = Some(v.clone());
                i += 2;
            }
            "--no-stream" => {
                streaming = false;
                i += 1;
            }
            "--no-memory" => {
                use_memory = false;
                i += 1;
            }
            "--show-tools" => {
                show_tools = true;
                i += 1;
            }
            "--max-turns" => {
                let v = args
                    .get(i + 1)
                    .ok_or_else(|| "--max-turns needs <n>".to_string())?;
                max_turns_override = Some(v.parse().map_err(|e| format!("--max-turns: {e}"))?);
                i += 2;
            }
            other => return Err(format!("unknown flag for `chat`: {other}")),
        }
    }

    let config = crate::config::current_snapshot();
    let cfg = &config.agent;
    setup::is_ready(cfg)?;
    // Build the provider once and reuse across turns. If the user
    // mid-REPL wants a different model, they can `/quit` and re-launch.
    let provider = crate::ai::gate::build_system_provider(cfg)
        .map_err(|e| format!("provider unavailable: {e}"))?;

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| format!("tokio runtime: {e}"))?;
    let timeout = runtime::loop_::background_drain_timeout();
    runtime.block_on(async move {
        let outcome = chat_cmd_async(
            provider,
            cfg,
            explicit_session,
            streaming,
            use_memory,
            show_tools,
            max_turns_override,
        )
        .await;
        runtime::background::drain(timeout).await;
        outcome
    })
}

#[allow(clippy::too_many_arguments)]
async fn chat_cmd_async(
    provider: std::sync::Arc<dyn llm::Provider>,
    cfg_in: &crate::config::AgentConfig,
    explicit_session: Option<String>,
    streaming: bool,
    use_memory: bool,
    show_tools: bool,
    max_turns_override: Option<u32>,
) -> Result<Value, String> {
    use crate::agent::llm::accumulate::StreamSink;
    use crate::agent::llm::types::StreamEvent;
    use std::collections::HashSet;
    use std::io::{BufRead, Write};
    use std::sync::{Arc, Mutex};

    // Apply --max-turns override locally without mutating global config.
    let mut cfg_owned = cfg_in.clone();
    if let Some(n) = max_turns_override {
        cfg_owned.max_turns = n;
    }
    let cfg = &cfg_owned;
    let mut session_id: String = explicit_session
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());

    // Build the registry once. MCP servers attach the same way as
    // `live`/`ask`, so the model has the full toolbox.
    let mut effective_config = (*crate::config::current_snapshot()).clone();
    effective_config.agent = cfg.clone();
    let registry_deps = crate::agent::tools::registry::RegistryDeps::load(
        Arc::new(effective_config),
        crate::agent::tools::registry::RegistryPaths::from_process(),
    );
    let runtime_deps = registry_deps.runtime.clone();
    let mut tools = crate::agent::tools::registry::default_registry_with_deps(&registry_deps);
    let guardrails = runtime::loop_::guardrails_from_cfg(cfg);
    tools.set_guardrails(guardrails.clone());
    tools.set_approval(runtime::loop_::approval_from_cfg(cfg));
    let exposure = crate::agent::tools::exposure::ToolExposureContext::from_current_session(
        Some(&session_id),
        None,
        crate::agent::tools::exposure::ExecutionHost::Direct,
        guardrails,
    )?;
    let _mcp_handles = runtime::loop_::attach_mcp_servers_for_cli(&mut tools, cfg, &exposure).await;

    let memory_db = if use_memory {
        registry_deps.memory.clone()
    } else {
        None
    };

    let stdout = std::io::stdout();
    let stderr = std::io::stderr();
    // When the user is at an interactive terminal, the stream sink
    // already echoed the assistant's text to stderr (which is the
    // same terminal as stdout), so printing the assembled answer
    // again to stdout would duplicate it. Skip the second copy in
    // that case. When stdout is piped to a file or another command
    // we still want the canonical answer on stdout — the streaming
    // copy on stderr is the "progress" view there.
    let stdout_is_tty = std::io::IsTerminal::is_terminal(&std::io::stdout());

    // Header banner — to stderr so a piped-stdout consumer only
    // sees the assistant outputs.
    {
        let mut e = stderr.lock();
        let _ = writeln!(
            e,
            "cos agent chat — provider={} model={} session={} memory={} streaming={} max_turns={} progressive_tools={}",
            cfg.provider,
            cfg.model,
            session_id,
            if memory_db.is_some() { "on" } else { "off" },
            if streaming { "on" } else { "off" },
            cfg.max_turns,
            if cfg.progressive_tools_enabled {
                "on"
            } else {
                "off"
            },
        );
        let _ = writeln!(e, "Type /help for commands. Ctrl-D or /quit to exit.");
        if show_tools {
            let names = tools.names();
            let _ = writeln!(e, "tools ({}): {}", names.len(), names.join(", "));
        }
    }

    let stdin = std::io::stdin();
    let mut input = Vec::new();
    let mut prompt_seq: u32 = 0;

    /// Stream sink shared across turns — re-used so allocation
    /// happens once. Each turn calls `reset()` before invoking
    /// the runtime so per-turn state doesn't bleed.
    ///
    /// `verbose_telemetry` controls the `[turn done finish=...]`
    /// telemetry line. We only want it when stdout is being piped
    /// somewhere (so a log consumer can see finish reasons); on an
    /// interactive terminal it's just noise after every reply.
    struct ChatSink {
        verbose_telemetry: bool,
        tool_calls: Mutex<Vec<serde_json::Value>>,
        announced_tools: Mutex<HashSet<String>>,
        warnings: Mutex<Vec<String>>,
        last_usage: Mutex<Option<crate::agent::llm::types::Usage>>,
        last_finish: Mutex<Option<crate::agent::llm::types::FinishReason>>,
        terminal: Arc<Mutex<TerminalOutputState>>,
        // Heartbeat keyed by tool_use id. Started when the runtime
        // dispatches a tool (via `ProgressSink::on_tool_start`),
        // cancelled when the result arrives. Without it the REPL
        // appeared frozen during slow filesystem walks — the user
        // saw the `[tool: name]` line and nothing else for 60s+.
        heartbeat: crate::agent::runtime::progress::Heartbeat,
    }
    impl ChatSink {
        fn new(verbose_telemetry: bool) -> Self {
            Self {
                verbose_telemetry,
                tool_calls: Mutex::new(Vec::new()),
                announced_tools: Mutex::new(HashSet::new()),
                warnings: Mutex::new(Vec::new()),
                last_usage: Mutex::new(None),
                last_finish: Mutex::new(None),
                terminal: Arc::new(Mutex::new(TerminalOutputState::default())),
                heartbeat: crate::agent::runtime::progress::Heartbeat::new(),
            }
        }
        fn reset(&self) {
            mlock(&self.tool_calls).clear();
            mlock(&self.announced_tools).clear();
            mlock(&self.warnings).clear();
            *mlock(&self.last_usage) = None;
            *mlock(&self.last_finish) = None;
            mlock(&self.terminal).reset();
        }

        fn announce_tool(&self, id: &str, name: &str, out: &mut impl Write) {
            let should_announce =
                id.is_empty() || mlock(&self.announced_tools).insert(id.to_string());
            if should_announce {
                mlock(&self.terminal).write_line(out, &format!("[tool: {name}]"));
            }
        }
    }
    impl StreamSink for ChatSink {
        fn on_event(&self, event: &StreamEvent) {
            let stderr = std::io::stderr();
            let mut e = stderr.lock();
            match event {
                StreamEvent::TextDelta { text } => {
                    mlock(&self.terminal).write_text(&mut e, text);
                    let _ = e.flush();
                }
                StreamEvent::ToolUseStart { id, name } => {
                    self.announce_tool(id, name, &mut e);
                }
                StreamEvent::ToolInputDelta { .. } => {}
                StreamEvent::ToolUse(call) => {
                    self.announce_tool(&call.id, &call.name, &mut e);
                    mlock(&self.tool_calls).push(serde_json::json!({
                        "id": call.id,
                        "name": call.name,
                    }));
                }
                StreamEvent::Reasoning { .. } => {}
                StreamEvent::ToolState { .. } => {}
                StreamEvent::Message(resp) => {
                    for block in &resp.content {
                        if let crate::agent::llm::types::ContentBlock::Text { text } = block {
                            mlock(&self.terminal).write_text(&mut e, text);
                        }
                    }
                    for call in &resp.tool_calls {
                        self.announce_tool(&call.id, &call.name, &mut e);
                        mlock(&self.tool_calls).push(serde_json::json!({
                            "id": call.id,
                            "name": call.name,
                        }));
                    }
                    let _ = e.flush();
                }
                StreamEvent::Done { finish, usage } => {
                    if self.verbose_telemetry {
                        mlock(&self.terminal)
                            .write_line(&mut e, &format!("[turn done finish={finish:?}]"));
                    } else {
                        mlock(&self.terminal).finish_line(&mut e);
                    }
                    *mlock(&self.last_usage) = Some(usage.clone());
                    *mlock(&self.last_finish) = Some(*finish);
                }
                StreamEvent::Warning { message } => {
                    mlock(&self.terminal).write_line(&mut e, &format!("[warning] {message}"));
                    mlock(&self.warnings).push(message.clone());
                }
            }
        }
    }

    impl crate::agent::runtime::progress::ProgressSink for ChatSink {
        fn on_tool_start(&self, id: &str, name: &str, _input: &serde_json::Value) {
            self.announce_tool(id, name, &mut std::io::stderr().lock());
            let terminal = Arc::clone(&self.terminal);
            self.heartbeat.start_with_callback(id, move |cancelled| {
                let mut terminal = mlock(&terminal);
                if cancelled.load(std::sync::atomic::Ordering::Acquire) {
                    return;
                }
                let mut stderr = std::io::stderr().lock();
                terminal.write_text(&mut stderr, ".");
                let _ = stderr.flush();
            });
        }

        fn on_tool_result(
            &self,
            id: &str,
            name: &str,
            ok: bool,
            latency_ms: u64,
            _bytes_returned: usize,
            content_preview: &str,
        ) {
            self.heartbeat.stop(id);
            {
                let mut stderr = std::io::stderr().lock();
                mlock(&self.terminal).finish_line(&mut stderr);
            }
            if !ok {
                mlock(&self.terminal).write_line(
                    &mut std::io::stderr().lock(),
                    &format_tool_failure(name, latency_ms, content_preview),
                );
            }
        }
    }

    let sink_obj = Arc::new(ChatSink::new(!stdout_is_tty));

    let clean_exit = loop {
        // Prompt user (to stderr so stdout stays clean for
        // assistant text).
        {
            let mut e = stderr.lock();
            let _ = write!(e, "you> ");
            let _ = e.flush();
        }
        input.clear();
        let n = match stdin.lock().read_until(b'\n', &mut input) {
            Ok(n) => n,
            Err(e) => {
                return Err(format!("stdin read error: {e}"));
            }
        };
        if n == 0 {
            // EOF
            let _ = writeln!(stderr.lock(), "\n[eof]");
            break true;
        }

        let decoded = String::from_utf8_lossy(&input);
        let had_invalid_utf8 = matches!(&decoded, std::borrow::Cow::Owned(_));
        if had_invalid_utf8 {
            tracing::debug!("chat input contained invalid UTF-8; invalid bytes were removed");
        }
        let line = decoded.trim();
        let repaired_command = had_invalid_utf8.then(|| line.replace('\u{FFFD}', ""));
        let command_line = repaired_command.as_deref().unwrap_or(line);
        if command_line.is_empty() {
            continue;
        }

        // Slash commands.
        if let Some(rest) = command_line.strip_prefix('/') {
            let mut parts = rest.split_whitespace();
            let cmd = parts.next().unwrap_or("");
            match cmd {
                "quit" | "exit" | "q" => {
                    break true;
                }
                "help" | "?" => {
                    let mut e = stderr.lock();
                    let _ = writeln!(e, "/quit | /exit | /q       leave the REPL");
                    let _ = writeln!(e, "/help | /?               this help");
                    let _ = writeln!(e, "/session                 print current session id");
                    let _ = writeln!(e, "/clear                   start a fresh session id");
                    let _ = writeln!(
                        e,
                        "/history [N]             show last N (default 10) messages"
                    );
                    let _ = writeln!(e, "/tools                   list permitted tools");
                }
                "session" => {
                    let mut e = stderr.lock();
                    let _ = writeln!(e, "session={session_id} prompts_so_far={prompt_seq}");
                }
                "clear" => {
                    session_id = uuid::Uuid::new_v4().to_string();
                    prompt_seq = 0;
                    let mut e = stderr.lock();
                    let _ = writeln!(e, "[new session: {session_id}]");
                }
                "history" => {
                    let n: usize = parts.next().and_then(|s| s.parse().ok()).unwrap_or(10);
                    if let Some(db) = &memory_db {
                        match db.recent(&session_id, n) {
                            Ok(rows) => {
                                let mut e = stderr.lock();
                                if rows.is_empty() {
                                    let _ = writeln!(e, "(no messages yet)");
                                } else {
                                    for r in &rows {
                                        let content = memory::history::sanitize_stored_content(
                                            &r.role, &r.content,
                                        );
                                        let snippet: String = content.chars().take(140).collect();
                                        let _ = writeln!(e, "[{}] {}", r.role, snippet);
                                    }
                                }
                            }
                            Err(e) => {
                                let _ = writeln!(stderr.lock(), "history error: {e}");
                            }
                        }
                    } else {
                        let _ = writeln!(stderr.lock(), "history unavailable (memory off)");
                    }
                }
                "tools" => {
                    let names = tools.names();
                    let _ = writeln!(
                        stderr.lock(),
                        "tools ({}): {}",
                        names.len(),
                        names.join(", ")
                    );
                }
                other => {
                    let _ = writeln!(stderr.lock(), "unknown slash command: /{other} (try /help)");
                }
            }
            continue;
        }

        prompt_seq += 1;

        // Run a turn.
        let user_prompt = line.to_string();
        let result = if streaming {
            sink_obj.reset();
            let sink: Arc<dyn StreamSink> = sink_obj.clone();
            let progress: Arc<dyn crate::agent::runtime::progress::ProgressSink> = sink_obj.clone();
            if let Some(db) = &memory_db {
                let request = runtime::loop_::RuntimeRequest::streaming(
                    provider.clone(),
                    cfg,
                    &user_prompt,
                    &tools,
                    sink,
                    progress,
                )
                .with_continuation(db, &session_id, 100)
                .with_exposure(&exposure);
                runtime::loop_::run_with_deps(&runtime_deps, request).await
            } else {
                let request = runtime::loop_::RuntimeRequest::streaming(
                    provider.clone(),
                    cfg,
                    &user_prompt,
                    &tools,
                    sink,
                    progress,
                )
                .with_exposure(&exposure);
                runtime::loop_::run_with_deps(&runtime_deps, request).await
            }
        } else if let Some(db) = &memory_db {
            let request = runtime::loop_::RuntimeRequest::buffered(
                provider.clone(),
                cfg,
                &user_prompt,
                &tools,
            )
            .with_continuation(db, &session_id, 100)
            .with_exposure(&exposure);
            runtime::loop_::run_with_deps(&runtime_deps, request).await
        } else {
            let request = runtime::loop_::RuntimeRequest::buffered(
                provider.clone(),
                cfg,
                &user_prompt,
                &tools,
            )
            .with_exposure(&exposure);
            runtime::loop_::run_with_deps(&runtime_deps, request).await
        };

        match result {
            Ok(ask_result) => {
                // Streaming sink echoes incremental text to stderr.
                // When stderr+stdout share a terminal (interactive
                // use), printing the final answer to stdout would
                // duplicate the response on screen. Only emit the
                // stdout copy when piping or when streaming is off
                // (i.e. the user hasn't seen the text yet).
                let print_final_to_stdout = !(streaming && stdout_is_tty);
                if print_final_to_stdout {
                    let mut o = stdout.lock();
                    let _ = writeln!(o, "{}", ask_result.answer);
                    let _ = o.flush();
                }

                // Per-turn telemetry footer (turn index, model,
                // session id) is debugging metadata. Keep it for
                // piped/logged runs but suppress on an interactive
                // terminal so the conversation reads cleanly.
                if !stdout_is_tty {
                    let mut e = stderr.lock();
                    let _ = writeln!(
                        e,
                        "[turn {} done; turns={} model={} session={}]",
                        prompt_seq, ask_result.turns, ask_result.model, ask_result.session_id
                    );
                }
                if should_render_evidence_warning(stdout_is_tty, &ask_result.evidence.status) {
                    let _ = writeln!(
                        stderr.lock(),
                        "[warning: response could not be fully verified]"
                    );
                } else if !matches!(
                    ask_result.evidence.status,
                    crate::agent::runtime::evidence::EvidenceStatus::Verified
                        | crate::agent::runtime::evidence::EvidenceStatus::NotRequired
                ) {
                    tracing::debug!(
                        status = ?ask_result.evidence.status,
                        warnings = ?ask_result.evidence.warnings,
                        "interactive response evidence was incomplete"
                    );
                }
                if ask_result
                    .fallback
                    .as_ref()
                    .is_some_and(|fallback| fallback.degraded)
                {
                    if let Some(fallback) = &ask_result.fallback {
                        let _ = writeln!(
                            stderr.lock(),
                            "[provider fallback: {}/{} -> {}/{}]",
                            fallback.primary_provider,
                            fallback.primary_model,
                            fallback.active_provider,
                            fallback.active_model
                        );
                    }
                }
            }
            Err(err) => {
                let _ = writeln!(stderr.lock(), "[error] {err}");
                // Don't break — let the user retry / clear / quit.
            }
        }
    };

    Ok(json!({
        "status": if clean_exit { "ok" } else { "interrupted" },
        "session_id": session_id,
        "prompts": prompt_seq,
        "provider": cfg.provider,
        "model": cfg.model,
    }))
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/agent/conversation_commands.rs"
    ));
}
