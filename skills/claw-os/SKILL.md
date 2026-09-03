---
name: claw-os
description: "Discover and operate the Claw system-agent layer. Use its progressive cos command tree before concluding that an OS, App, diagnostic, permission, or session capability is unavailable."
---

# Claw System Agent

You are operating through the Claw system-agent layer. The host may be a full
Claw OS image or another Linux distribution with `claw-os-agent` installed.
Use `cos_sysinfo` before naming the host distribution. Commands return
structured JSON.

This `SKILL.md` is the instruction layer. Read a linked child document through
`cos_skill` with `command=resource` only when the current task needs that
specific detail; do not preload every resource.

## Progressive CLI discovery

Do not memorize or guess the complete CLI. Discover only the branch needed for
the current task:

1. Call `cos_help` with `path=[]` to inspect top-level `cos` namespaces.
2. Follow one returned name at a time, for example `path=["agent"]`.
3. Inspect the selected command, for example
   `path=["agent","usage"]`.
4. Execute only through the returned `model_tool` or another named,
   capability-gated tool. `cos_help` never executes commands.

For installed App services, search the permitted MCP catalog with
`cos_tool_search`, inspect the selected schema with `cos_tool_describe`, and
invoke it through `cos_tool_call`. `path=["app"]` describes the human CLI and
does not make an operation model-callable. Before saying Claw lacks a
capability, inspect the relevant command-tree branch.

## Agent-only tools

Call these directly through the tool interface; they are not public CLI
namespaces:

| Tool | Purpose |
|---|---|
| `cos_sandbox` | Run untrusted code in a Linux-namespace sandbox ([details](sandbox.md)) |
| `cos_proc` | Spawn and manage processes by session ([details](process.md)) |
| `cos_ipc` | Messages, locks, barriers, streaming pipes ([details](ipc.md)) |
| `cos_watch` | Event-driven file/process/service watching ([details](watch.md)) |
| `cos_netfilter` | Outbound firewall and rate limiting ([details](network.md)) |
| `cos_trace` | Execution tracing — tree-structured observability ([details](trace.md)) |
| `cos_browser` | Standalone CDP server lifecycle for external Puppeteer/Playwright clients (the `web` app already uses cos-browser per-request and does not need this) |
| `cos_diagnose` | Structured system diagnosis with bounded probes, evidence IDs, confidence, and recommendations ([diagnostic protocol](diagnostics.md)) |

## System Diagnosis

For system-level symptoms, call `cos_diagnose` before proposing a cause or
mutation. Then follow the matching playbook:

| Symptom | Playbook |
|---|---|
| Slow, frozen, high CPU or memory | [Performance](diagnostics-performance.md) |
| Offline, slow network, DNS or connectivity | [Network](diagnostics-network.md) |
| Disk full, storage latency, missing mount | [Storage](diagnostics-storage.md) |
| Crash, OOM kill, segmentation fault | [Crash](diagnostics-crash.md) |
| Failed or unhealthy service | [Service](diagnostics-service.md) |
| Heat, fan, battery or throttling | [Thermal](diagnostics-thermal.md) |
| Suspicious login, denial or exposed port | [Security](diagnostics-security.md) |

Never present a system-state claim without naming the evidence that supports
it. Read-only investigation comes first; capability approval and a recovery
plan come before mutation.

Permission roles and App capability gates are documented in
[permissions.md](permissions.md). Detailed App semantics remain available in
[apps.md](apps.md), but the live catalogue is authoritative.

All errors include a `code` field for programmatic handling ([error codes](errors.md)).

## Provider fallback

The system agent supports an ordered cross-provider fallback chain in
`~/.config/cos/config.json`:

```json
{
  "agent": {
    "provider": "anthropic",
    "model": "claude-sonnet-4-6",
    "provider_fallbacks": [
      {"provider": "openai", "model": "gpt-4.1", "api_key_env": "OPENAI_API_KEY"},
      {"provider": "llama_local", "model": "local-default"}
    ]
  }
}
```

Fallback occurs only for transport, authentication, quota/rate-limit, stream,
or malformed-upstream failures. Invalid requests and internal policy/budget
errors do not switch providers. Each fallback model is independently checked
by `ai.chat` capability and budget gates. Switches are emitted to the chained
Agent audit log and surfaced as degraded-mode metadata. Streaming only switches
before a provider stream is established; it never mixes providers after output
has begun.

## User isolation

`cos agent serve` runs as one non-root Unix uid per instance. It refuses root,
wrong-owner, or symlinked state roots and tightens its per-user data directory
to mode `0700`. Web task lifecycle, approval resolution, memory sessions, and
context-event inbox reads are filtered to that uid; legacy records without an
owner are hidden from the web surface. Privileged clawd APIs retain an explicit
all-owner view only for root callers.

## Web TLS

Loopback `cos agent serve` may use HTTP. Any non-loopback bind is rejected
unless both `--tls-cert /absolute/cert.pem` and
`--tls-key /absolute/key.pem` are supplied. The built-in server uses rustls;
the private key must be owned by the serving uid with no group/other
permissions. API query-string tokens are never accepted; the loopback `?t=`
value is consumed only by the frontend bootstrap exchange. A TLS reverse proxy
can instead terminate HTTPS while `cos agent serve` remains bound to
`127.0.0.1`.

`serve.token` is only a bootstrap secret. The browser exchanges it at
`POST /api/auth/token` for a uid-bound HMAC-SHA256 access token valid for at
most one hour; normal APIs reject the persistent bootstrap secret. Run
`cos agent serve --rotate-token` to rotate both bootstrap and signing secrets,
immediately invalidating every issued access token.

For detailed usage of any feature, read the corresponding doc linked above.
