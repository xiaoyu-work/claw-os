# App ↔ AI Integration

> **Scope** — This document covers **only** how third-party Apps integrate
> Large-Language-Model capabilities with Claw OS. It does **not** cover how
> Apps acquire ordinary OS permissions (filesystem, network, exec, …) for
> their own non-AI code. Those are the App's own concern, exactly as they
> are on Linux, macOS, or Windows.

## 1. The Two Worlds

A third-party App on Claw OS lives in **two completely separate worlds**.
Mixing them up is the most common design mistake — keep them apart.

### 1.1 World A — Normal App behavior

The App runs as a regular OS process. It calls `open()`, `socket()`,
`execve()`, spawns helpers, parses files, talks to its own database, draws
a UI. **Claw OS does not interpose, gate, audit, or care** about any of
this.

Whether the App asks the user for filesystem access via its own dialog,
or assumes the user installed it knowing what it does, or ships with a
hard-coded path — **the App author decides**, exactly as on any other OS.

Claw OS contributes nothing to this world beyond a regular Unix kernel.
Nothing in this document applies here.

### 1.2 World B — App using AI

The moment the App wants to:
- call the stable text-chat model surface, **or**
- let a model decide what to do on the computer next,

it must go through Claw OS's **AI subsystem**. The AI subsystem has
exactly two entry points:

| Entry point | Purpose |
|---|---|
| `cos ai chat`  | A single model invocation. Optionally injects a list of Tool schemas so the model can emit `tool_calls`. **Never executes them.** |
| `cos ai tool`  | Execute one Tool by name with explicit JSON arguments. Capability-checked, App-scoped, fully audited. |

Both are audited. Every invocation and policy decision inside the AI subsystem
emits operational audit metadata.

> **Current stable scope:** `ai.chat` and `ai.chat.untrusted` only.
> Embed, image, vision, audio, and video selectors are experimental and
> currently return a language-specific typed unsupported error before
> invoking `cos` or a provider.

## 2. Why The Split

Claw OS exists to give the user a single, complete account of **what
their AI did on their computer**:

- Which models were called, by which App, with what usage and policy result.
- Which kernel Tools were invoked because a model asked for them, under which
  capability decision, and whether they succeeded.
- Which App initiated each of the above.

To deliver that guarantee, the OS only needs to interpose on **AI** —
not on every App syscall. Normal App behavior is the App's
responsibility, and the OS treats it like any other Unix process.

The boundary is precisely the two CLI entry points above. Inside them =
audited AI activity. Outside them = ordinary program behavior.

## 3. App Lifecycle & Identity

Before any of the AI machinery is meaningful, the OS must answer one
question with certainty: **"which App is calling me right now?"** The
answer determines which Tools are unlocked, which scope is enforced,
and which row is written to the audit log.

Claw OS answers this through a deliberate restriction:

> **An App's identity is established only when the kernel itself spawns
> the App process.** A random Linux program cannot become "App X" by
> passing a flag.

This restriction is foundational to the audit guarantee in §2. The
sections that follow (Tool catalog, manifest, CLI, audit) all assume
it.

### 3.1 Lifecycle in one picture

```
   ┌──────────────────────────────────────────────────────┐
   │  Author packages an App directory (manifest + code)  │
   └──────────────────────────────────┬───────────────────┘
                                      │  cos app install <dir>
                                      ▼
   ┌──────────────────────────────────────────────────────┐
   │  Registered under /usr/lib/cos/apps/<id>/            │
   │  Consent UI runs for ai.tools[] entries              │
   └──────────────────────────────────┬───────────────────┘
                                      │  cos app <id> <op>
                                      ▼
   ┌──────────────────────────────────────────────────────┐
   │  Kernel forks the App process                        │
   │  Sets COS_APP_ID=<id> in the child's env             │
   └──────────────────────────────────┬───────────────────┘
                                      │  App code may shell out:
                                      ▼
   ┌──────────────────────────────────────────────────────┐
   │  cos ai chat / cos ai tool                           │
   │  Inherits COS_APP_ID from parent → identity is fixed │
   └──────────────────────────────────────────────────────┘
```

### 3.2 Three invariants

1. **Discovery is mandatory.** The App must live below the active
   `$COS_APPS_DIR` and have a valid manifest. `cos app install <dir>` is the
   normal system-install path; development trees can instead point
   `COS_APPS_DIR` at a user-owned directory.
2. **Kernel-spawn is mandatory.** An App process can only be started by
   `cos app <id> <op>`. The kernel sets `COS_APP_ID` on the child
   (`core/src/bridge.rs`); any `cos ai chat` / `cos ai tool` call inside
   that process tree inherits the env var.
3. **Identity flags are not trusted.** `cos ai chat --app foo.bar` is
   rejected when the caller's `COS_APP_ID` is unset or does not match.
   There is no token, no signed payload, nothing the caller can present
   to claim a different identity.

### 3.3 Runtimes

The kernel launches Apps through `core/src/bridge.rs` according to the
manifest's `runtime` field:

| `runtime`          | How the kernel starts it                     |
|--------------------|----------------------------------------------|
| `python` (default) | Python wrapper + the configured entry file   |
| `node`             | `node <wrapper.js>`                          |
| `shell`            | Shell + the configured entry script         |
| `binary`           | Execute the configured binary entry directly |

In every case, `COS_APP_ID` is set before `exec`. The choice of runtime
is purely about *how* the kernel spawns the App; it has no effect on
identity or on which Tools are reachable.

### 3.4 What this rules out (and why)

| Rejected pattern | Why we rejected it |
|---|---|
| Per-App token in `$COS_APP_TOKEN`; any process can claim identity by presenting a valid token. | Token leakage = silent identity theft. The audit log loses its single most important guarantee. |
| Verify caller via executable path / PID ancestry. | Linux-specific, brittle under containers / namespaces / re-exec, and still weaker than a kernel-set env var. |
| Trust `--app <id>` on faith and audit everything anyway. | Audit becomes "this string showed up", not "this App did X". Defeats the point of having an audit log at all. |

The flexibility cost — "third-party Apps cannot be arbitrary Linux
daemons that wake themselves up at boot" — is acceptable. AI-using Apps
are user-launched assistants, not background services. Apps that *are*
background services do not belong in World B and have no business
talking to `cos ai`.

### 3.5 What "SDK" means under this model

Because identity flows from kernel-spawn, **the SDK is never a system
boundary**. It is a convenience library that runs *inside* an
already-spawned App process and shells out to `cos ai chat` /
`cos ai tool`. A developer can equivalently:

- Use the bundled `claw_os_sdk` Python package (`from claw_os_sdk
  import ai, tools`).
- Write a Node / Go / Rust equivalent — roughly 100 lines each.
- Skip the library and `subprocess.run(["cos", "ai", "chat", …])` by
  hand.

All three are equivalent. The stable contract is the **CLI plus JSON
envelope**, not any particular library.

## 4. The Tool Catalog

A **Tool** is "an operation a model is allowed to invoke on this
computer". The catalog (`core/src/ai/tools.rs::CATALOG`) is the
authoritative list. Each Tool has:

- A stable, dotted name
- A JSON Schema for arguments
- A required capability verb + scope policy
- A localized `why` template used in user-consent UI
- A stability tier (`stable` / `experimental`)

Current catalog:

| Tool | Purpose |
|---|---|
| `fs.read_text` | Read a bounded UTF-8 text file |
| `fs.list` | List one directory level |
| `kv.get` | Read from the calling App's key-value namespace |

What is **not** in the Tool catalog — by design:

- `cos_proc` `cos_ipc` `cos_watch` `cos_trace` `cos_netfilter` `cos_service`
  `cos_cron` `cos_checkpoint` `cos_model` — these are internal kernel
  infrastructure used by Claw OS's own Agent. Third-party App AIs do
  not see them.

## 5. Manifest

An App that uses AI declares it in its `app.json`. The `ai` section is
the **only** part of the manifest this document specifies; the rest of
the manifest (`operations`, `dependencies`, etc.) is unchanged and out of
scope.

```json
{
  "id": "research-assistant",
  "ai": {
    "budget": { "monthly_units": 200000 },
    "safety": "strict",
    "origins": ["trusted", "user-input", "external-content"],
    "tools": ["fs.read_text", "kv.get"]
  }
}
```

`ai.tools[]` lists stable catalog tool names the App may expose to the
model. Names not present in this allowlist are rejected before reaching
the provider.

`ai.budget` caps unit spend per month. Apps never select a model; the
machine owner configures the provider and model.

An App that does **not** use AI omits the `ai` section entirely.

## 6. CLI

### 6.1 `cos ai chat`

Single inference. Returns the model's message plus any `tool_calls` it
emitted. Does **not** execute the tool calls.

```
cos ai chat --app <id>
            [--prompt <text> | --prompt-file <path>]
            [--system <text>]
            [--tools <name>,<name>,…]
            [--origin trusted|user-input|external-content]
            [--max-units <N>]
```

The returned envelope:

```json
{
  "text": "...",
  "model": "configured-model",
  "provider": "configured-provider",
  "verb": "ai.chat",
  "tool_calls": [
    { "id": "call_1", "name": "fs.read_text",
      "input": { "path": "/home/user/notes.txt" } }
  ],
  "usage": { "input_tokens": 1234, "output_tokens": 567, "units": 1801 },
  "budget": { "period": "2026-05", "units_used": 1801, "units_cap": 200000 },
  "review": { "safety": "strict", "prompt_redacted": false }
}
```

`--tools` accepts only names already listed in the App's manifest. Any
name outside the manifest is rejected before reaching the model.

### 6.2 `cos ai tool`

Execute one Tool. The kernel validates capability, narrows scope to the
App's namespace, runs the operation, and emits an audit row.

```
cos ai tool <name> --app <id> --args <json>
```

On success:

```json
{ "tool": "fs.read_text", "app_id": "research-assistant",
  "status": "ok", "result": { "text": "..." } }
```

On failure, the command exits non-zero through the normal `cos` CLI error
path; SDK callers receive the corresponding exception/error payload. A failed
call does not return the success envelope above.

## 7. In-Process SDK

This SDK runs *inside* an App process the kernel has already spawned
(see §3). It is **not** a way for an arbitrary Linux program to obtain
App identity — that path does not exist by design.

App code uses two helpers, both shipped with Claw OS as the
`claw_os_sdk` Python package (lives at
`claw-os-sdk/python/src/claw_os_sdk/` in-repo, installed to
`/usr/lib/cos/python/claw_os_sdk/`). Capability gating and the COW
snapshot helper live in the sibling **`cos_runtime`** package — see
[`cos-runtime/README.md`](../cos-runtime/README.md). Third-party apps
that don't gate themselves (i.e. don't touch fs/exec/pkg/net) need
only `claw_os_sdk`.

```python
from claw_os_sdk import ai, tools

resp = ai.chat(
    prompt="Summarise the latest notes.",
    tools=["fs.read_text"],
    origin="user-input",
)

for call in resp.tool_calls:
    result = tools.call(call.name, call.input)
    # The App decides how to use the explicit tool result in a later
    # prompt; the kernel never executes or loops tool calls inline.
    print(result.value)

print(resp.text)
```

`ai.chat()` and `tools.call()` shell out to `cos ai chat` and `cos ai
tool` respectively. **The App writes its own agent loop** — the OS never
loops on its behalf. This is the deliberate dividing line between the
*kernel Agent* (`cos agent`, which does loop, hold memory, and run
skills) and third-party App AIs (which get only the two primitives and
build whatever they want on top).

Equivalent SDKs in Node, Go and Rust are straightforward thin wrappers
around the same two CLI calls; `cos ai tools` dumps the live catalog.

## 8. Audit Surface

`cos ai chat` and `cos ai tool` append `LlmRunRecord` rows to the same
`ai.jsonl` stream. The current schema records operational metadata such as:

- timestamp plus trace/span/session identifiers,
- provider, model, and local engine identity,
- duration, input/output token usage, cache usage, and finish reason,
- status/error plus allow/deny decision and denial reason,
- calling App id and the derived capability verb.

Tool executions use `provider="kernel"` and `model="tool:<name>"`, so they
remain distinguishable from model calls while sharing one query surface.

The run log intentionally does **not** currently include prompt/response text,
prompt/response hashes, origin, or the complete offered-tool list. It records
AI-gate activity and kernel Tool execution, not every action performed by
ordinary App code.

## 9. What This Design Does **Not** Audit

By stated design:

- **App's non-AI code paths.** An App can `open("/tmp/x")` from its own
  Python without any audit row. That is intentional: this is not a
  syscall-level sandbox; it is an AI-accountability layer.

- **Prompt and response content.** The `ai.jsonl` run log stores neither the
  literal content nor content hashes. Other session or App-owned stores may
  persist conversation data independently.

- **Down-stream effects of AI output.** If a model says "you should
  write file X" and the App's normal code obeys via `open()`, that
  write is App behavior, not AI behavior — and so is not in the AI
  audit log. The model invocation has an operational run-log row, but
  the output text that suggested the write is not stored there.

If a deployment wants stricter coverage — e.g. "the App must not be
able to do anything we cannot audit" — that is a separate isolation
problem (process sandboxing, mandatory access control) and is **not**
addressed by this document.

## 10. Relationship to the Kernel Agent

Claw OS ships with its own Agent (`cos agent`, see `core/src/agent/`).
That Agent:

- Has its own internal tool catalog (`core/src/agent/tools/cos_proxy/
  PRIMITIVES`) that includes low-level kernel infrastructure
  third-party App AIs do not see.
- Runs full loops with memory, skills, hooks, todos, recall, etc.
- Is the user's "default AI" — the assistant the desktop chrome talks
  to.

The kernel Agent and third-party App AIs share the same capability model but
use **separate registries**:

- the kernel Agent uses `core/src/agent/tools/registry.rs`,
- App-facing AI uses the bounded catalog in `core/src/ai/tools.rs`.

An App AI sees only names declared in its manifest and allowed by the
App-facing catalog; the kernel Agent has its own broader tool surface.

A third-party App does not, and cannot, invoke `cos agent chat`. That
namespace is for the system's own Agent. Apps speak only to `cos ai
chat` and `cos ai tool`.

## 11. Implementation Phases

This document is the contract. Concrete work falls into roughly:

1. **App registry & installer.** ✅ Done. `cos app install <source>`
   validates the manifest (including `ai.tools[]` against the live
   catalog), copies the App tree under `apps_dir()/<id>/`, and runs
   the AI consent prompt. Flags: `--yes` auto-grants consent,
   `--no-consent` defers it, `--force` overwrites an existing install.
2. **Multi-runtime bridge.** ✅ Done. `core/src/bridge.rs` dispatches
   on the manifest `runtime` field (`python` / `node` / `shell` / `binary`) and
   sets `COS_APP_ID` before `exec`.
3. **Identity enforcement.** ✅ Done for `cos ai chat`. The CLI rejects any
   call where `COS_APP_ID` is unset (caller not kernel-spawned) or where
   `--app <id>` disagrees with the env value (cross-App impersonation
   attempt). Lives in `core/src/ai/chat.rs::enforce_identity`.
   `cos ai tool` shares the same helper via `enforce_identity_for`.
4. **Tool registry.** ✅ Done. `core/src/ai/tools.rs::CATALOG` —
   shared Tool definitions, capability verbs, scope policies,
   stability tiers. Starter set: `fs.read_text`, `fs.list`, `kv.get`.
5. **`cos ai tool` command.** ✅ Done. Single-tool executor; routes
   through the capability layer; emits one audit row per call. Plus
   `cos ai tools` to print the catalog.
6. **`cos ai chat` command.** ✅ Done. Namespace migration plus
   `--tools <comma-list>` flag: each name is resolved against the
   kernel catalog and exposed to the model as a callable. The gate
   **never** executes the proposed calls — it surfaces them in
   `tool_calls[]` and lets the App fulfil whichever it chooses via
   `cos ai tool <name>`.
7. **In-process SDK.** ✅ Done for Python. `claw_os_sdk.ai` exposes
   `ai.chat(..., tools=[...])` and an `AiResponse.tool_calls` field;
   `claw_os_sdk.tools` exposes `tools.call`, `tools.catalog`, and
   `tools.for_chat`. Node / Rust / Go shapes are described in §7
   above.
8. **Manifest validator.** ✅ Done. `app.json` schema accepts an
   `ai.tools[]` allowlist; duplicate entries are rejected by the
   shape-only `validate()` and unknown names are rejected by
   `validate_tools_against_catalog(&[&str])`. `bridge.rs` runs the
   catalog check at App launch so a typo'd allowlist fails fast.
9. **Audit rows.** ✅ Done. Both `cos ai chat` and `cos ai tool`
   write to the same `<log_dir>/ai.jsonl` stream via
   `LlmRunRecord`. Tool rows use `provider="kernel"`,
   `model="tool:<name>"`, and the derived caps verb so dashboards
   that group by `verb` continue to work. See `core/src/agent/llm/run_log.rs`
   for the row shape.
10. **Tool reference doc.** ✅ Done. See
    [`docs/app-ai-tool-catalog.md`](./app-ai-tool-catalog.md) for the
    per-Tool reference and the "How to add a new Tool" appendix.

Each phase is independently shippable.

## 12. App Session Tools (Apps as MCP Servers)

§§1–11 describe how an App **consumes** AI. This section describes the
inverse: how an App **exposes** AI-callable Tools that the kernel agent
can hold a stateful Session with, alongside its built-in tools.

### 12.1 Why a second surface

`cos_app_run` gives the kernel agent a one-shot gateway into an App's CLI
verbs. The model discovers app ids and operations through `cos_app_catalog`,
then dispatches through the generic runner so installing more Apps does not
add one schema per App to every model request. Every call is a fresh
`bridge::run_python_app` spawn; the App has no way to keep state between calls
or to run a background task while the agent does something else.

For a real multi-step task — "scan the calendar, then for every busy
day pull the doc, then summarise" — the agent needs:

1. tools with **strict input schemas** the model can introspect,
2. **session state** across calls (cached parses, in-flight tasks,
   prefetched data),
3. the ability to **span multiple Apps in one conversation**, with each
   App contributing tools the kernel can interleave freely.

We get all three by wrapping every App as an [MCP][mcp] server. The
kernel already speaks MCP (`core/src/agent/tools/mcp/`); the App side
needs only a thin SDK.

[mcp]: https://modelcontextprotocol.io/

### 12.2 Manifest: the `session` block

App authors declare their session surface in `app.json` next to
`operations`:

```jsonc
{
  "id": "kv",
  "version": "0.1.0",
  // ...existing operations...
  "session": {
    "transport": "stdio",                       // default
    "entry": "server.py",                       // default per runtime
    "tools": [
      {
        "name": "kv.get",
        "summary": {"en": "Read a value."},
        "args": [
          {"name": "key", "kind": "name", "required": true}
        ],
        "needs": [
          {
            "verb": "data.kv.read",
            "scope": {"kind": "from-arg", "arg": "key"},
            "why": {"en": "Read by key."}
          }
        ]
      }
    ]
  }
}
```

The same `needs[]` grammar from §5 applies. Every `from-arg` scope must
reference a declared `arg`; the manifest parser rejects mismatches at
install time.

### 12.3 Python SDK: `claw_os_sdk.serve`

App authors write:

```python
from claw_os_sdk.serve import App

app = App()

@app.tool("kv.get", args={"key": "string"}, required=["key"])
def kv_get(key: str) -> str:
    return _load().get(key, "")

app.serve()  # blocking; reads MCP frames on stdin, writes on stdout
```

The SDK is stdlib-only. It speaks the MCP `initialize` / `tools/list`
/ `tools/call` triplet and handles `notifications/initialized` plus
`ping`. Return values are stringified (`str` / `dict` / `list` →
`text` content); raised exceptions become `isError: true` responses
the model can react to.

### 12.4 Kernel bring-up: `core/src/agent/tools/cos_apps_session.rs`

At agent boot the kernel walks `$COS_APPS_DIR`, reads every
`app.json`, and for each `session.tools[]` registers one
[`AppSessionTool`](../core/src/agent/tools/cos_apps_session.rs) in the
agent's `ToolRegistry`. Registry names are
`app_<id>__<tool_with_dots_to_underscores>` (e.g.
`app_kv__kv_get`).

The MCP server itself is **not** started here. The first time any of
its tools is called — or the agent explicitly invokes
`cos_app_session_open` — `bring_up_app` spawns the server, runs the
MCP handshake, and stores the live handle in a process-wide
`SessionManager`. Subsequent calls reuse it; `cos_app_session_close`
drops the handle and terminates the child. **Hybrid attach.**

Per-call flow inside `AppSessionTool::exec`:

1. `Manifest::resolve_session_tool_needs(tool_name, args)` turns
   manifest `needs[]` into concrete `Cap`s using the call's actual
   arguments.
2. `crate::caps::require(verb, scope)` checks each. A denial
   short-circuits the call — the App's server never sees the request.
3. `client.call_tool(name, args)` forwards over MCP stdio.
4. One `LlmRunRecord` row is appended to `ai.jsonl` with
   `provider="app:<id>"` and `model="tool:<name>"`, distinguishing
   App-session calls from kernel-catalog calls.

The App MCP server, therefore, can trust every call its `tool_call`
handler receives — the kernel has already authorised it. Defensive
re-checks inside the App are still allowed but never required.

### 12.5 Meta-tools the model sees

Two extra tools live alongside every registered App tool:

| Name | Purpose |
| --- | --- |
| `cos_app_session_open` | Bring an App's session server up. Idempotent. Returns the list of registered tools so the model knows which names to use next. |
| `cos_app_session_close` | Terminate an App's session server. In-memory state is lost; persistent state on disk is preserved. The next call to any of its tools transparently re-opens the server. |

Combined with `cos_app_catalog` (the existing one-shot discovery
surface), the model can: discover what's installed → open the ones it
needs → call interleaved tools across multiple Apps → close.

### 12.6 CLI surface

- `cos app tool list` — print every session tool every installed App
  declares. Useful for sanity-checking what the agent will see.
- `cos app tool list <app>` — same, scoped to one App.
- `cos app lint [<app>]` — now also reports
  `session.entry-missing` if the manifest declares a `session` block
  whose entry script isn't on disk.

### 12.7 Relationship to existing surfaces

| Surface | Stateful? | Strict schema? | Authored in |
| --- | --- | --- | --- |
| `cos_app_catalog` + `cos_app_run` | No — fresh spawn per call | No — CLI string args, manifest discovered on demand | App `main.py` `operations[]` |
| Typed `cos_app_<id>` compatibility proxy | No | Command enum only; not in the default registry | App manifest, explicit internal registration |
| `app_<id>__<tool>` (§12) | Yes — long-lived server | Yes — JSON Schema | App `server.py` (Python) or the App's own binary (Rust) + manifest `session.tools[]` |

The default registry keeps App count out of the provider schema through the
catalog/run pair. Typed compatibility proxies remain constructible for
specialized callers and tests. Session tools are additive and appear only for
Apps that declare a stateful session surface.

### 12.8 Rust SDK: `crates/cos-mcp-serve`

Desktop Apps that ship as native `libcosmic` GUI binaries (under
`desktop/*`) cannot exec a Python `server.py` — they already own a
running event loop. Instead, the **same binary** flips into MCP
server mode when launched with `COS_MCP_SERVER=1` in the environment
(the kernel agent sets it automatically; see `bring_up_app`).

`crates/cos-mcp-serve` is the Rust counterpart to `claw_os_sdk.serve`
(`claw-os-sdk/python/src/claw_os_sdk/serve.py`). It exposes a tiny
builder API:

```rust
use cos_mcp_serve::{Server, Tool, ToolResult};
use std::sync::Arc;

if std::env::var("COS_MCP_SERVER").as_deref() == Ok("1") {
    Server::new("my-app", env!("CARGO_PKG_VERSION"))
        .tool(Arc::new(MyTool))
        .serve_stdio().await?;
    return;
}
// ... normal GUI main() below ...
```

The wire format and method surface are identical to the Python SDK
(`initialize` / `tools/list` / `tools/call` / `ping`); the kernel's
MCP client cannot tell which one is on the other end.

The manifest for a binary-runtime App sets `runtime: "binary"` and
points `session.entry` at the **installed** absolute path of the
binary (e.g. `/usr/bin/cosmic-screenshot`), since `Command::new`
runs it directly with no interpreter. See
`apps/cosmic-screenshot/app.json` for the canonical example.

### 12.9 Out of scope (this iteration)

- MCP progress notifications for long-running tools. Apps that need
  background work should use the `start_task` / `poll` pattern (one
  tool kicks off work and returns a handle; another polls).
- Confidentiality between Apps in a shared agent session. The agent is
  the orchestrator and sees every value crossing the boundary.
