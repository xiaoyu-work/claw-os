# App, Agent, and AI Integration

Claw OS has one public, multi-language developer surface:
[`claw-os-sdk`](../claw-os-sdk/). Python, Rust, Node, and Go consume the
same versioned wire schemas. MCP is an SDK module and the only typed service
contract for Agent-to-App and App-to-App calls; it is not a separate SDK or a
second registration format.

This document explains the architecture. For app authoring steps, see
[`app-development.md`](app-development.md). For package authentication, see
[`extension-provenance.md`](extension-provenance.md).

## 1. One App contract

An App package has one signed `app.json`. `schema_version: 2` and the `mcp`
block define the App Mesh surface:

```json
{
  "id": "notes",
  "version": "1.0.0",
  "schema_version": 2,
  "name": {
    "en": "Notes"
  },
  "runtime": "python",
  "mcp": {
    "entry": "server.py",
    "transport": "stdio",
    "lifecycle": "lazy",
    "access": {
      "system_agent": true,
      "apps": [
        "calendar"
      ],
      "external_agents": false
    },
    "tools": [
      {
        "name": "notes.get",
        "summary": {
          "en": "Read one note."
        },
        "args": [
          {
            "name": "id",
            "kind": "name",
            "required": true
          }
        ],
        "needs": [
          {
            "verb": "data.db.read",
            "scope": {
              "kind": "from-arg",
              "arg": "id"
            },
            "why": {
              "en": "Read the requested note."
            }
          }
        ]
      }
    ]
  }
}
```

The signed manifest is authoritative for:

- App and package identity;
- tool names and model-visible summaries;
- argument schemas, defaults, choices, and conditions;
- caller restrictions;
- target capabilities; and
- service entrypoint and lifecycle.

Runtime code binds implementation functions by tool name only. A server cannot
add a tool, change a schema, widen a capability, or choose its caller identity.
The removed top-level `session` field is rejected rather than translated.

`operations` may still be declared for direct human CLI commands such as
`cos app notes export`. They receive validated argv and are not another typed
App Mesh protocol. Desktop metadata remains in the same manifest and uses the
same App identity.

## 2. Authentication is workload identity

Claw OS does not give Apps API keys and does not trust identity fields supplied
in MCP arguments or environment variables.

`clawd` derives the caller from registered workload state:

- owner UID;
- session id;
- process id and process start time;
- task id and Extension Host ancestry;
- System Agent or App Agent principal kind; and
- the verified App id for an App or App Agent.

For a task-owned Extension Host, the broker signs the binding between the
task, capability generation, caller session, and optional App Agent id. The
host may relay one call from that exact parent, but descendants cannot reuse
the context or downgrade an App Agent to the System Agent.

The Gateway serializes this identity under the reserved MCP metadata key:

```text
_meta["claw-os.dev/call-context"]
```

The versioned context contains `call_id`, `trace_id`, `parent_call_id`,
`depth`, `deadline_unix_ms`, `session_id`, `task_id`, and `caller`. It is
separate from business arguments and is not a bearer capability. The target
App may use it for ownership, partitioning, diagnostics, cancellation, and
progress, but privileged work still requires live daemon authority.

## 3. Authorization has two independent sides

Every App Mesh call checks both the caller and the target:

| Check | Meaning |
| --- | --- |
| Caller invoke authority | May this workload address this exact App tool? |
| Target capability needs | What may the target App do while handling this call? |

The caller needs exact `agent.invoke:<app-id>/<tool-name>` authority. An
App-level name does not cover a tool:

```text
agent.invoke:notes              does not cover notes/notes.get
agent.invoke:notes/notes.get    covers only notes.get
agent.invoke:notes/*            covers the declared tools under notes
```

`mcp.access` only narrows the accepted principal classes. It never grants
invoke authority.

Caller invoke authority is not copied into the target process. After the
caller is authorized, `clawd` derives the target's capabilities from that
tool's signed `needs[]` and effective validated arguments. The resulting
`AppGateway` grant is:

- bound to the target `app-mcp` session and call task;
- limited to the required provider audiences;
- bounded by package provenance;
- capped by the authenticated deadline; and
- revoked when the call completes, fails, times out, is cancelled, or the
  session is torn down.

The reusable App process holds only its at-rest base grant between calls.
Authority from call A cannot be spent by call B.

## 4. End-to-end call flow

```text
System Agent, App Agent, or App
  -> trusted ToolExposureContext
  -> exact agent.invoke:<app>/<tool> authorization
  -> task-owned Extension Host control channel
  -> fresh verification of the installed signed App package
  -> app-mcp session registration and exact process binding
  -> manifest argument validation and default materialization
  -> manifest access policy and capability-generation checks
  -> deadline-bound AppGateway target grant
  -> MCP tools/call with authenticated _meta call context
  -> App handler
  -> structured result and audit projection
  -> transient grant clear/revocation
```

Important properties:

1. The Extension Host accepts only typed App id, tool name, and arguments from
   the worker. It does not expose the real `/run/cos/clawd.sock`.
2. `clawd` independently verifies the package reference, signed manifest,
   exact declared tool, caller principal, capability generation, and deadline.
3. Authority-time package lookup bypasses the discovery cache. Replacing a
   signed child file after registration invalidates the next call.
4. Ordinary operation or GUI sessions use group `app`; MCP services use
   `app-mcp`. Only `app-mcp` sessions may receive per-call authority.
5. The model cannot open or close App services. First authenticated use starts
   a lazy service, and task or host teardown owns cleanup.
6. There is no unsandboxed fallback. Failure to prove identity, provenance,
   process binding, or sandbox enforcement refuses the call.

## 5. SDK runtime

### Python

```python
from claw_os_sdk.mcp import App, current_context

app = App.from_manifest()

@app.tool("notes.get")
def get_note(id: str) -> dict:
    call = current_context()
    call.raise_if_cancelled()
    return {
        "id": id,
        "caller": call.caller.id,
    }

app.serve()
```

Direct `App(...)` construction is rejected. `@app.tool` accepts only a
manifest-declared name. Every `tools/call` requires authenticated Gateway
context.

### Rust

```rust,no_run
use std::sync::Arc;

use async_trait::async_trait;
use claw_os_sdk::mcp::{App, CallContext, Tool, ToolResult};
use serde_json::Value;

struct GetNote;

#[async_trait]
impl Tool for GetNote {
    fn name(&self) -> &str {
        "notes.get"
    }

    async fn handle(&self, args: Value, call: CallContext) -> ToolResult {
        if let Err(error) = call.check_cancelled() {
            return ToolResult::error(error.to_string());
        }
        ToolResult::structured(args)
    }
}

# async fn serve() -> Result<(), Box<dyn std::error::Error>> {
let mut app = App::from_environment()?;
app.bind(Arc::new(GetNote))?;
app.serve_stdio().await?;
# Ok(())
# }
```

Node uses `mcp.App.fromManifest()` plus `app.tool(name, handler)`. Go uses
`LoadMCPApp`, `Bind`, and `ServeStdio`. All four runtimes:

- derive descriptors from `app.json.mcp.tools[]`;
- reject undeclared or duplicate handlers;
- require authenticated call context;
- validate effective arguments;
- support structured tool results;
- support progress and cooperative cancellation; and
- reserve stdout for newline-delimited MCP JSON-RPC.

## 6. App-to-App and App-to-Agent

App-to-App and Agent-to-App use the same Gateway. The only differences are the
authenticated caller principal and the target manifest's `mcp.access` rule.
Developers do not implement service credentials, token rotation, signature
checking, or policy middleware.

An App that needs to call another App declares the exact target
`agent.invoke` need in its own signed manifest. `clawd` checks that declaration
against the App's live authority, creates the child call context with increased
depth and linked trace ids, and applies the target App's independent access and
capability policy.

An App can invoke the OS Agent through the public SDK's AI/Agent gates when its
manifest declares the corresponding AI capability. Provider credentials,
model selection, budgets, prompt-origin policy, and model-visible logging stay
inside the core Agent.

## 7. Audit and failure semantics

The audit trail binds:

- caller principal, owner, session, and task;
- target App, package id, content digest, and tool;
- capability generation and exact derived needs;
- call/trace lineage and deadline;
- Gateway grant issue, spend, clear, and revocation; and
- bounded result or structured refusal.

Missing context, expired deadlines, package substitution, file replacement,
principal substitution, stale capability generations, undeclared tools, and
ordinary App sessions all fail closed before the handler receives the call.

After a mutating handler reports success, late cancellation must not rewrite
that success into a retryable cancellation. Read-only handlers may use
cooperative cancellation throughout their work.

## 8. External MCP servers

Configured third-party MCP servers remain a separate attachment boundary.
Their untrusted descriptors stay behind `mcp_catalog` and `mcp_invoke`, opaque
session/task/generation-bound handles, structural sanitization, and explicit
approval. They do not become Apps, receive App identity, or bypass the signed
`app.json` App Mesh contract.
