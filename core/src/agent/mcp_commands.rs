use super::tools;
use serde_json::{json, Value};

/// Apply ad-hoc `--allow` / `--deny` overrides to a base
/// [`AgentConfig`] for one-shot scoping (currently used by
/// `cos agent mcp serve`). Returns the merged config without
/// mutating the input. Extracted so the merge logic can be tested
/// independently of the blocking server entry point.
fn merge_mcp_overrides(
    base: &crate::config::AgentConfig,
    allow: Option<Vec<String>>,
    deny: Vec<String>,
) -> crate::config::AgentConfig {
    let mut out = base.clone();
    if let Some(a) = allow {
        out.tool_allow = Some(a);
    }
    out.tool_deny.extend(deny);
    if !out.tool_deny.iter().any(|name| name == "cos_oauth_login") {
        out.tool_deny.push("cos_oauth_login".to_string());
    }
    out
}

/// `cos agent mcp [serve|status]` — MCP (Model Context Protocol)
/// server that exposes the agent's tool registry to external clients
/// over newline-delimited JSON-RPC on stdio. `serve` runs in the
/// foreground until stdin closes; `status` reports the catalog
/// without listening.
pub(super) fn mcp_cmd(args: &[String]) -> Result<Value, String> {
    use crate::agent::tools::mcp::{server::McpServer, transport::StdioTransport};
    use std::sync::Arc;
    let sub = args.first().map(|s| s.as_str()).unwrap_or("status");
    match sub {
        "status" | "" => {
            let config = crate::config::current_snapshot();
            let cfg = &config.agent;
            let deps = tools::registry::RegistryDeps::load_current();
            let mut tools = tools::registry::default_registry_with_deps(&deps);
            tools.set_guardrails(crate::agent::runtime::loop_::guardrails_from_cfg(cfg));
            tools.set_approval(crate::agent::runtime::loop_::approval_from_cfg(cfg));
            let configured_servers: Vec<Value> = cfg
                .mcp_servers
                .iter()
                .map(|s| {
                    json!({
                        "name": s.name,
                        "command": s.command,
                        "args_count": s.args.len(),
                        "env_count": s.env.len(),
                        "timeout_secs": s.timeout_secs,
                        "enabled": s.enabled,
                    })
                })
                .collect();
            let enabled_count = cfg.mcp_servers.iter().filter(|s| s.enabled).count();
            Ok(json!({
                "status": "ready",
                "transport": "stdio",
                "server_name": format!("cos-agent/{}", env!("CARGO_PKG_VERSION")),
                "tools_registered": tools.names_unfiltered().len(),
                "tools_permitted": tools.names().len(),
                "tools": tools.names(),
                "external_servers_configured": cfg.mcp_servers.len(),
                "external_servers_enabled": enabled_count,
                "external_servers": configured_servers,
            }))
        }
        "servers" => {
            // `cos agent mcp servers [--probe]` — list configured
            // external MCP servers. With `--probe`, attempt to attach
            // each enabled one and report tool counts (does not
            // mutate global state; the runtime registry is built
            // fresh inside this call and dropped on return).
            let probe = args.iter().any(|a| a == "--probe");
            let config = crate::config::current_snapshot();
            let cfg = &config.agent;
            if !probe {
                let entries: Vec<Value> = cfg
                    .mcp_servers
                    .iter()
                    .map(|s| {
                        json!({
                            "name": s.name,
                            "command": s.command,
                            "args": s.args,
                            "env_keys": s.env.keys().collect::<Vec<_>>(),
                            "cwd": s.cwd,
                            "timeout_secs": s.timeout_secs,
                            "enabled": s.enabled,
                        })
                    })
                    .collect();
                return Ok(json!({
                    "ok": true,
                    "probed": false,
                    "count": cfg.mcp_servers.len(),
                    "servers": entries,
                }));
            }
            // Probe: attach each enabled server, report tools, drop
            // handles immediately (children torn down). Best-effort:
            // failed attachments are reported per-server.
            use crate::agent::tools::mcp::integration::{attach_server, McpServerSpec};
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|e| format!("tokio runtime: {e}"))?;
            let report = runtime.block_on(async {
                let mut out: Vec<Value> = Vec::with_capacity(cfg.mcp_servers.len());
                for s in &cfg.mcp_servers {
                    if !s.enabled {
                        out.push(json!({
                            "name": s.name,
                            "enabled": false,
                            "skipped": true,
                        }));
                        continue;
                    }
                    let spec = McpServerSpec {
                        name: s.name.clone(),
                        command: s.command.clone(),
                        args: s.args.clone(),
                        env: s.env.clone(),
                        cwd: s.cwd.clone(),
                        timeout_secs: s.timeout_secs,
                        url: None,
                        bearer_env: None,
                        // Operator configuration, not an installed
                        // package: the machine owner wrote this into
                        // config.json, so there is no publisher to
                        // authenticate.
                        provenance: None,
                    };
                    let mut throwaway_registry =
                        crate::agent::tools::registry::ToolRegistry::new();
                    match attach_server(&spec, &mut throwaway_registry).await {
                        Ok(handle) => {
                            let tools = throwaway_registry.names_unfiltered();
                            out.push(json!({
                                "name": s.name,
                                "enabled": true,
                                "ok": true,
                                "tool_count": handle.tool_count(),
                                "tools": tools,
                            }));
                            // handle dropped here — child killed
                        }
                        Err(e) => {
                            out.push(json!({
                                "name": s.name,
                                "enabled": true,
                                "ok": false,
                                "error": e,
                            }));
                        }
                    }
                }
                out
            });
            Ok(json!({
                "ok": true,
                "probed": true,
                "count": cfg.mcp_servers.len(),
                "servers": report,
            }))
        }
        "probe" => mcp_probe(&args[1..]),
        "call" => mcp_call(&args[1..]),
        "serve" => {
            // Build the registry exactly as `agent::ask` would, so
            // the same guardrails/approval policy applies to MCP-
            // initiated tool calls. Ad-hoc --allow / --deny flags
            // narrow the tool surface for this serve invocation
            // without touching global config — useful for exposing a
            // restricted catalogue to a single MCP client.
            let config = crate::config::current_snapshot();
            let cfg = &config.agent;
            let mut allow_overrides: Option<Vec<String>> = None;
            let mut deny_overrides: Vec<String> = Vec::new();
            let mut i = 1usize;
            while i < args.len() {
                match args[i].as_str() {
                    "--allow" => {
                        let v = args
                            .get(i + 1)
                            .ok_or_else(|| "--allow needs <tool-name>".to_string())?;
                        allow_overrides
                            .get_or_insert_with(Vec::new)
                            .push(v.clone());
                        i += 2;
                    }
                    "--deny" => {
                        let v = args
                            .get(i + 1)
                            .ok_or_else(|| "--deny needs <tool-name>".to_string())?;
                        deny_overrides.push(v.clone());
                        i += 2;
                    }
                    other => {
                        return Err(format!(
                            "unknown flag for `mcp serve`: {other}. try --allow <name> | --deny <name>"
                        ))
                    }
                }
            }
            let deps = tools::registry::RegistryDeps::load_current();
            let mut tools = tools::registry::default_registry_with_deps(&deps);
            // Honour allow override when supplied; otherwise inherit
            // cfg.tool_allow via the standard helper. --deny appends
            // to (does not replace) cfg.tool_deny so global denies
            // still apply.
            let merged = merge_mcp_overrides(cfg, allow_overrides, deny_overrides);
            tools.set_guardrails(crate::agent::runtime::loop_::guardrails_from_cfg(&merged));
            tools.set_approval(crate::agent::runtime::loop_::approval_from_cfg(cfg));
            let registry = Arc::new(tools);
            let server = McpServer::new(
                "cos-agent",
                env!("CARGO_PKG_VERSION"),
                registry,
            );
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|e| format!("tokio runtime: {e}"))?;
            runtime
                .block_on(server.serve(StdioTransport::stdio()))
                .map_err(|e| format!("mcp serve: {e}"))?;
            Ok(json!({"status": "stopped", "reason": "stdin closed"}))
        }
        other => Err(format!(
            "unknown mcp subcommand: {other}. try: status | servers [--probe] | serve | probe | call"
        )),
    }
}

/// Spec-shared parser for `--cmd / --arg / --env / --cwd / --timeout`.
#[derive(Debug)]
struct McpSpawnSpec {
    cmd: String,
    args: Vec<String>,
    env: Vec<(String, String)>,
    cwd: Option<String>,
    timeout_secs: u64,
}

fn parse_mcp_spawn_spec(args: &[String]) -> Result<(McpSpawnSpec, Vec<String>), String> {
    let mut cmd: Option<String> = None;
    let mut child_args: Vec<String> = Vec::new();
    let mut env: Vec<(String, String)> = Vec::new();
    let mut cwd: Option<String> = None;
    let mut timeout_secs: u64 = 30;
    let mut leftover: Vec<String> = Vec::new();
    let mut i = 0usize;
    while i < args.len() {
        match args[i].as_str() {
            "--cmd" => {
                cmd = Some(
                    args.get(i + 1)
                        .cloned()
                        .ok_or_else(|| "--cmd needs a value".to_string())?,
                );
                i += 2;
            }
            "--arg" => {
                child_args.push(
                    args.get(i + 1)
                        .cloned()
                        .ok_or_else(|| "--arg needs a value".to_string())?,
                );
                i += 2;
            }
            "--env" => {
                let raw = args
                    .get(i + 1)
                    .cloned()
                    .ok_or_else(|| "--env needs KEY=VALUE".to_string())?;
                let (k, v) = raw
                    .split_once('=')
                    .ok_or_else(|| format!("--env expects KEY=VALUE, got {raw:?}"))?;
                env.push((k.to_string(), v.to_string()));
                i += 2;
            }
            "--cwd" => {
                cwd = Some(
                    args.get(i + 1)
                        .cloned()
                        .ok_or_else(|| "--cwd needs a value".to_string())?,
                );
                i += 2;
            }
            "--timeout" => {
                let raw = args
                    .get(i + 1)
                    .ok_or_else(|| "--timeout needs <secs>".to_string())?;
                timeout_secs = raw.parse::<u64>().map_err(|e| format!("--timeout: {e}"))?;
                i += 2;
            }
            other if other.starts_with("--") => {
                return Err(format!("unknown spawn flag: {other}"));
            }
            _ => {
                leftover.push(args[i].clone());
                i += 1;
            }
        }
    }
    let cmd = cmd.ok_or_else(|| "--cmd <executable> required".to_string())?;
    Ok((
        McpSpawnSpec {
            cmd,
            args: child_args,
            env,
            cwd,
            timeout_secs,
        },
        leftover,
    ))
}

/// Spawn an MCP server child, run `init + tools/list`, return the
/// handshake details + tool catalogue. Used to verify a server is
/// reachable and to enumerate what it exposes before wiring it into
/// the agent's tool registry.
fn mcp_probe(args: &[String]) -> Result<Value, String> {
    let (spec, leftover) = parse_mcp_spawn_spec(args)?;
    if !leftover.is_empty() {
        return Err(format!(
            "unexpected positional arg(s) for `mcp probe`: {leftover:?}"
        ));
    }
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| format!("tokio runtime: {e}"))?;
    runtime.block_on(async move {
        let (transport, mut child) = spawn_mcp_child(&spec).await?;
        let client = crate::agent::tools::mcp::client::McpClient::new(transport);
        client.start().await;
        let init_fut = client.initialize(
            crate::agent::tools::mcp::protocol::Implementation {
                name: "cos-agent".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
            },
            crate::agent::tools::mcp::protocol::ClientCapabilities::default(),
        );
        let init =
            tokio::time::timeout(std::time::Duration::from_secs(spec.timeout_secs), init_fut)
                .await
                .map_err(|_| {
                    // Best-effort kill — child holds stdio fds.
                    let _ = child.start_kill();
                    format!(
                        "timed out waiting for initialize after {}s",
                        spec.timeout_secs
                    )
                })?
                .map_err(|e| format!("initialize: {e}"))?;
        // initialized notification — many servers don't gate on it,
        // but spec-correct clients send it.
        let _ = client.notify("notifications/initialized", None).await;
        let tools_fut = client.list_tools();
        let tools_res =
            tokio::time::timeout(std::time::Duration::from_secs(spec.timeout_secs), tools_fut)
                .await;
        let tools_payload = match tools_res {
            Ok(Ok(list)) => json!({
                "ok": true,
                "tools": list.tools,
            }),
            Ok(Err(e)) => json!({
                "ok": false,
                "error": e.to_string(),
            }),
            Err(_) => json!({
                "ok": false,
                "error": format!("timed out after {}s", spec.timeout_secs),
            }),
        };
        // Drop client to abort reader task before killing child.
        drop(client);
        let _ = child.start_kill();
        let _ = child.wait().await;
        Ok::<_, String>(json!({
            "ok": true,
            "command": spec.cmd,
            "args": spec.args,
            "protocol_version": init.protocol_version,
            "server_info": init.server_info,
            "capabilities": init.capabilities,
            "tools_list": tools_payload,
        }))
    })
}

/// Spawn an MCP server child and invoke a single `tools/call`,
/// returning its `CallToolResult`. Useful for ad-hoc inspection or
/// for scripting against a server before the agent gets near it.
fn mcp_call(args: &[String]) -> Result<Value, String> {
    let (spec, leftover) = parse_mcp_spawn_spec(args)?;
    let mut tool_name: Option<String> = None;
    let mut input: Option<serde_json::Value> = None;
    let mut i = 0usize;
    while i < leftover.len() {
        match leftover[i].as_str() {
            "--input" => {
                let raw = leftover
                    .get(i + 1)
                    .cloned()
                    .ok_or_else(|| "--input needs a JSON value".to_string())?;
                input = Some(
                    serde_json::from_str(&raw)
                        .map_err(|e| format!("--input is not valid JSON: {e}"))?,
                );
                i += 2;
            }
            other if other.starts_with("--") => {
                return Err(format!("unknown flag for `mcp call`: {other}"));
            }
            _ => {
                if tool_name.is_some() {
                    return Err(format!(
                        "unexpected extra positional arg: {:?}",
                        leftover[i]
                    ));
                }
                tool_name = Some(leftover[i].clone());
                i += 1;
            }
        }
    }
    let tool = tool_name.ok_or_else(|| "tool name positional required".to_string())?;

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| format!("tokio runtime: {e}"))?;
    runtime.block_on(async move {
        let (transport, mut child) = spawn_mcp_child(&spec).await?;
        let client = crate::agent::tools::mcp::client::McpClient::new(transport);
        client.start().await;
        let init_fut = client.initialize(
            crate::agent::tools::mcp::protocol::Implementation {
                name: "cos-agent".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
            },
            crate::agent::tools::mcp::protocol::ClientCapabilities::default(),
        );
        let _init =
            tokio::time::timeout(std::time::Duration::from_secs(spec.timeout_secs), init_fut)
                .await
                .map_err(|_| {
                    let _ = child.start_kill();
                    format!(
                        "timed out waiting for initialize after {}s",
                        spec.timeout_secs
                    )
                })?
                .map_err(|e| format!("initialize: {e}"))?;
        let _ = client.notify("notifications/initialized", None).await;
        let call_fut = client.call_tool(tool.clone(), input.clone());
        let result =
            tokio::time::timeout(std::time::Duration::from_secs(spec.timeout_secs), call_fut)
                .await
                .map_err(|_| {
                    let _ = child.start_kill();
                    format!("timed out calling {} after {}s", tool, spec.timeout_secs)
                })?
                .map_err(|e| format!("tools/call: {e}"))?;
        drop(client);
        let _ = child.start_kill();
        let _ = child.wait().await;
        Ok::<_, String>(json!({
            "ok": !result.is_error.unwrap_or(false),
            "tool": tool,
            "is_error": result.is_error.unwrap_or(false),
            "content": result.content,
        }))
    })
}

/// Spawn an MCP child process and return a stdio-attached transport
/// alongside the child handle. Caller is responsible for killing the
/// child when done. Stdin/stdout are captured; stderr is inherited
/// so the operator sees server diagnostics directly.
async fn spawn_mcp_child(
    spec: &McpSpawnSpec,
) -> Result<
    (
        crate::agent::tools::mcp::transport::StdioTransport,
        tokio::process::Child,
    ),
    String,
> {
    use std::process::Stdio;
    let mut command = tokio::process::Command::new(&spec.cmd);
    command.args(&spec.args);
    for (k, v) in &spec.env {
        command.env(k, v);
    }
    if let Some(cwd) = &spec.cwd {
        command.current_dir(cwd);
    }
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit());
    let mut child = command
        .spawn()
        .map_err(|e| format!("spawn {}: {e}", spec.cmd))?;
    let stdin = child
        .stdin
        .take()
        .ok_or_else(|| "child stdin unavailable".to_string())?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "child stdout unavailable".to_string())?;
    let transport = crate::agent::tools::mcp::transport::StdioTransport::from_pair(
        Box::new(stdout),
        Box::new(stdin),
    );
    Ok((transport, child))
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/agent/mcp_commands.rs"
    ));
}
