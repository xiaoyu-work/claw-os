use serde_json::{json, Value};

use super::client;
use super::config;
use super::protocol::{Request, Response};

const ASK_WAIT_TIMEOUT_MS: u64 = 24 * 60 * 60 * 1_000;

pub fn daemon_status() -> Result<Value, String> {
    send("daemon.status", Value::Null)
}

pub fn ask(prompt: &str, stream: bool) -> Result<Value, String> {
    let submitted = send("task.submit", json!({ "prompt": prompt }))?;
    let task_id = required_field(&submitted, "id")?;
    let result = send(
        if stream { "task.stream" } else { "task.result" },
        json!({
            "id": task_id,
            "timeout_ms": ASK_WAIT_TIMEOUT_MS,
        }),
    )?;

    task_result_to_ask_response(result, stream)
}

pub fn service_cmd(args: &[String]) -> Result<Value, String> {
    let sub = args.first().map(|s| s.as_str()).unwrap_or("");
    let rest = if args.is_empty() { &[] } else { &args[1..] };

    match sub {
        "" | "help" | "-h" | "--help" => Ok(service_help()),
        "submit" => service_submit(rest),
        "list" => service_list(rest),
        "status" => {
            if rest.is_empty() {
                send("daemon.status", Value::Null)
            } else {
                send("task.get", json!({ "id": rest[0] }))
            }
        }
        "result" => service_result(rest),
        "cancel" => service_cancel(rest),
        "context" => send("context.snapshot", Value::Null),
        "operations" => service_operations(rest),
        "work" => Err("agent workers are owned by system clawd.service".to_string()),
        "prune" => Err("agent queue pruning is owned by clawd".to_string()),
        other => Err(format!(
            "unknown agent service subcommand: {other}. try: submit | list | status | result | cancel | context | operations"
        )),
    }
}

fn service_help() -> Value {
    json!({
        "backend": "clawd",
        "subcommands": [
            "submit  \"<prompt>\" [--session ID] [--max-turns N]",
            "list    [--status pending|running|ok|error|cancelled] [--limit N]",
            "status  [<task_id>]",
            "result  <task_id> [--wait-secs N]",
            "cancel  <task_id>",
            "context",
            "operations [--limit N] [--source SOURCE]",
        ],
    })
}

fn service_submit(args: &[String]) -> Result<Value, String> {
    let mut prompt: Option<String> = None;
    let mut session_id: Option<String> = None;
    let mut max_turns: Option<u32> = None;
    let mut i = 0usize;
    while i < args.len() {
        match args[i].as_str() {
            "--session" => {
                let v = args.get(i + 1).ok_or("--session needs a value")?.clone();
                session_id = Some(v);
                i += 2;
            }
            "--max-turns" => {
                let v = args.get(i + 1).ok_or("--max-turns needs a value")?;
                max_turns = Some(v.parse().map_err(|e| format!("--max-turns: {e}"))?);
                i += 2;
            }
            s if s.starts_with("--") => return Err(format!("unknown flag: {s}")),
            _ => {
                if prompt.is_none() {
                    prompt = Some(args[i].clone());
                } else {
                    return Err("submit takes exactly one positional prompt argument".into());
                }
                i += 1;
            }
        }
    }

    let prompt = prompt
        .filter(|s| !s.trim().is_empty())
        .ok_or("usage: cos agent service submit \"<prompt>\" [--session ID] [--max-turns N]")?;
    let mut params = json!({ "prompt": prompt });
    if let Some(session_id) = session_id {
        params["session_id"] = json!(session_id);
    }
    if let Some(max_turns) = max_turns {
        params["max_turns"] = json!(max_turns);
    }

    let job = send("task.submit", params)?;
    Ok(json!({
        "status": "submitted",
        "backend": "clawd",
        "job_id": required_field(&job, "id")?,
        "job": job,
    }))
}

fn service_list(args: &[String]) -> Result<Value, String> {
    let mut params = json!({});
    let mut i = 0usize;
    while i < args.len() {
        match args[i].as_str() {
            "--status" => {
                let v = args.get(i + 1).ok_or("--status needs a value")?;
                params["status"] = json!(v);
                i += 2;
            }
            "--limit" => {
                let v = args.get(i + 1).ok_or("--limit needs a value")?;
                params["limit"] = json!(v.parse::<u64>().map_err(|e| format!("--limit: {e}"))?);
                i += 2;
            }
            "--all" => {
                params["limit"] = json!(u64::MAX);
                i += 1;
            }
            s => return Err(format!("unknown flag: {s}")),
        }
    }
    let mut result = send("task.list", params)?;
    result["backend"] = json!("clawd");
    Ok(result)
}

fn service_result(args: &[String]) -> Result<Value, String> {
    let id = args
        .first()
        .filter(|s| !s.trim().is_empty())
        .ok_or("usage: cos agent service result <task_id> [--wait-secs N]")?
        .clone();
    let mut wait_secs: u64 = 0;
    let mut i = 1usize;
    while i < args.len() {
        match args[i].as_str() {
            "--wait-secs" => {
                let v = args.get(i + 1).ok_or("--wait-secs needs a value")?;
                wait_secs = v.parse().map_err(|e| format!("--wait-secs: {e}"))?;
                i += 2;
            }
            s => return Err(format!("unknown flag: {s}")),
        }
    }
    send(
        "task.result",
        json!({
            "id": id,
            "timeout_ms": wait_secs.saturating_mul(1_000),
        }),
    )
}

fn service_cancel(args: &[String]) -> Result<Value, String> {
    let id = args
        .first()
        .filter(|s| !s.trim().is_empty())
        .ok_or("usage: cos agent service cancel <task_id>")?;
    send("task.cancel", json!({ "id": id }))
}

fn service_operations(args: &[String]) -> Result<Value, String> {
    let mut params = json!({});
    let mut i = 0usize;
    while i < args.len() {
        match args[i].as_str() {
            "--limit" => {
                let v = args.get(i + 1).ok_or("--limit needs a value")?;
                params["limit"] = json!(v.parse::<u64>().map_err(|e| format!("--limit: {e}"))?);
                i += 2;
            }
            "--source" => {
                let v = args.get(i + 1).ok_or("--source needs a value")?;
                params["source"] = json!(v);
                i += 2;
            }
            s => return Err(format!("unknown flag: {s}")),
        }
    }
    send("system.operations", params)
}

fn task_result_to_ask_response(job: Value, stream_requested: bool) -> Result<Value, String> {
    let status = job
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    match status {
        "ok" => Ok(json!({
            "answer": job.get("response").cloned().unwrap_or(Value::Null),
            "turns": job.get("turns_used").cloned().unwrap_or(Value::Null),
            "provider": job.get("provider").cloned().unwrap_or(Value::Null),
            "model": job.get("model").cloned().unwrap_or(Value::Null),
            "session_id": job.get("session_id").cloned().unwrap_or(Value::Null),
            "task_id": job.get("id").cloned().unwrap_or(Value::Null),
            "backend": "clawd",
            "stream_requested": stream_requested,
        })),
        "error" => Err(job
            .get("error")
            .and_then(Value::as_str)
            .unwrap_or("agent task failed")
            .to_string()),
        "cancelled" => Err("agent task cancelled".to_string()),
        other => Err(format!("agent task did not finish (status={other})")),
    }
}

fn send(command: &str, params: Value) -> Result<Value, String> {
    let request = Request {
        id: None,
        command: command.to_string(),
        params,
    };
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|err| format!("tokio runtime: {err}"))?;
    let response = runtime.block_on(client::request(config::socket_path(), request))?;
    response_result(response)
}

fn response_result(response: Response) -> Result<Value, String> {
    if response.ok {
        return Ok(response.result.unwrap_or(Value::Null));
    }

    let message = response
        .error
        .map(|err| format!("{}: {}", err.code, err.message))
        .unwrap_or_else(|| "clawd request failed".to_string());
    Err(message)
}

fn required_field(value: &Value, key: &str) -> Result<String, String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
        .ok_or_else(|| format!("clawd response missing string field: {key}"))
}
