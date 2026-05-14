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
- call a language model, embedder, image / audio / video model, **or**
- let a model decide what to do on the computer next,

it must go through Claw OS's **AI subsystem**. The AI subsystem has
exactly two entry points:

| Entry point | Purpose |
|---|---|
| `cos ai chat`  | A single model invocation. Optionally injects a list of Tool schemas so the model can emit `tool_calls`. **Never executes them.** |
| `cos ai tool`  | Execute one Tool by name with explicit JSON arguments. Capability-checked, App-scoped, fully audited. |

Both are audited. Everything inside the AI subsystem is on the record.

## 2. Why The Split

Claw OS exists to give the user a single, complete account of **what
their AI did on their computer**:

- Which models were called, with what prompts, returning what responses.
- Which computer operations were invoked because a model asked for them,
  with what arguments and what results.
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
   │  Registered under /var/lib/cos/apps/<id>/            │
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

1. **Install is mandatory.** `cos app install <dir>` is the only way to
   register an App. Until installation, a directory has no identity and
   no Tools.
2. **Kernel-spawn is mandatory.** An App process can only be started by
   `cos app <id> <op>`. The kernel sets `COS_APP_ID` on the child
   (`core/src/bridge.rs`); any `cos ai chat` / `cos ai tool` call inside
   that process tree inherits the env var.
3. **Identity flags are not trusted.** `cos ai chat --app foo.bar` is
   rejected when the caller's `COS_APP_ID` is unset or does not match.
   There is no token, no signed payload, nothing the caller can present
   to claim a different identity.

### 3.3 Runtimes

The kernel forks Apps through `core/src/bridge.rs`. Today only Python
is supported. As third-party Apps land, the bridge will dispatch on
the manifest's `runtime` field:

| `runtime`          | How the kernel starts it                     |
|--------------------|----------------------------------------------|
| `python` (default) | `python3 -c <wrapper>` — current behaviour   |
| `node`             | `node <wrapper.js>`                          |
| `binary`           | `exec /var/lib/cos/apps/<id>/bin/main`       |

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

- Use the bundled `apps/_lib/{ai,tools}.py`.
- Write a Node / Go / Rust equivalent — roughly 100 lines each.
- Skip the library and `subprocess.run(["cos", "ai", "chat", …])` by
  hand.

All three are equivalent. The stable contract is the **CLI plus JSON
envelope**, not any particular library.

## 4. The Tool Catalog

A **Tool** is "an operation a model is allowed to invoke on this
computer". The catalog (`core/src/ai/tools.rs::CATALOG`) is the
authoritative list. Each Tool has:

- A stable, dotted name (`fs.read`, `web.fetch`, `sandbox.exec`, …)
- A JSON Schema for arguments
- A required capability verb + scope policy
- A localized `why` template used in user-consent UI
- A stability tier (`stable` / `experimental` / `internal`)

Initial catalog (subject to change before 1.0):

| Tool | Purpose |
|---|---|
| `fs.read` `fs.write` `fs.search` `fs.stat` | File operations |
| `web.fetch` `web.search` | Web access |
| `sandbox.exec` | Run untrusted code inside namespace+seccomp |
| `doc.parse` | Parse PDF / DOCX / XLSX / PPTX / CSV |
| `mail.send` `cal.create` `cal.list` | Communications |
| `cred.load` | Load a secret from the App's own credential namespace |
| `ai.chat` `ai.embed` | Recursive / sub-agent model calls |

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
  "id": "third.party.research",
  "ai": {
    "budget": { "monthly_units": 200000 },
    "models": ["claude-*"],
    "tools": [
      {
        "name":  "fs.read",
        "scope": { "kind": "subtree", "value": "$APP_DATA" },
        "why":   { "en": "Read your research notes." }
      },
      {
        "name":  "web.fetch",
        "why":   { "en": "Look things up online to answer your question." }
      },
      {
        "name":  "sandbox.exec",
        "why":   { "en": "Run small scripts safely while answering." }
      }
    ]
  }
}
```

`ai.tools[]` lists every Tool the App's AI may call. At install time the
user sees one consent dialog containing one row per entry, populated from
the `why` text. Tools not granted are absent from the schema list sent to
the model — the model cannot reach them.

`ai.budget` caps token spend per month. `ai.models` constrains which
models the App is allowed to invoke.

An App that does **not** use AI omits the `ai` section entirely.

## 6. CLI

### 6.1 `cos ai chat`

Single inference. Returns the model's message plus any `tool_calls` it
emitted. Does **not** execute the tool calls.

```
cos ai chat --app <id>
            [--prompt <text> | --prompt-file <path>]
            [--system <text>]
            [--model <name>]
            [--tools <name>,<name>,…]
            [--origin trusted|user-input|external-content]
            [--max-units <N>]
```

The returned envelope:

```json
{
  "message":   { "role": "assistant", "content": "..." },
  "tool_calls": [
    { "id": "call_1", "name": "web.fetch",
      "arguments": { "url": "https://example.com" } }
  ],
  "usage":     { "input_units": 1234, "output_units": 567 }
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

Returns:

```json
{ "ok": true, "result": { ... }, "verb": "fs.read", "scope": { ... } }
```

or on failure:

```json
{ "ok": false, "error": "permission_denied", "tool": "fs.write",
  "reason": "scope $APP_DATA does not include /etc/passwd" }
```

## 7. In-Process SDK

This SDK runs *inside* an App process the kernel has already spawned
(see §3). It is **not** a way for an arbitrary Linux program to obtain
App identity — that path does not exist by design.

App code uses two helpers, both shipped with Claw OS at `apps/_lib/`.

```python
from _lib import ai, tools

messages = [{"role": "user", "content": "Summarise the latest notes."}]

while True:
    resp = ai.chat(
        messages=messages,
        tools=["fs.read", "web.fetch"],
        origin="user-input",
    )
    messages.append(resp.message)

    if not resp.tool_calls:
        break

    for call in resp.tool_calls:
        result = tools.invoke(call.name, call.arguments)
        messages.append({
            "role":         "tool",
            "tool_call_id": call.id,
            "content":      result,
        })

return resp.message.content
```

`ai.chat()` and `tools.invoke()` shell out to `cos ai chat` and `cos ai
tool` respectively. **The App writes its own agent loop** — the OS never
loops on its behalf. This is the deliberate dividing line between the
*kernel Agent* (`cos agent`, which does loop, hold memory, and run
skills) and third-party App AIs (which get only the two primitives and
build whatever they want on top).

Equivalent SDKs in Node, Go and Rust are straightforward thin wrappers
around the same two CLI calls; they need not duplicate the schemas
(`cos ai catalog --json` dumps the live registry).

## 8. Audit Surface

Every call to `cos ai chat` writes one row:

```json
{
  "ts":            "2026-05-13T22:00:00Z",
  "kind":          "ai.chat",
  "app":           "third.party.research",
  "model":         "claude-3-5-sonnet",
  "prompt_hash":   "sha256:…",
  "response_hash": "sha256:…",
  "tools_offered": ["fs.read", "web.fetch"],
  "tool_calls":    [{ "name": "web.fetch", "args_hash": "sha256:…" }],
  "usage":         { "input": 1234, "output": 567 },
  "origin":        "user-input",
  "session":       "…"
}
```

Every call to `cos ai tool` writes one row:

```json
{
  "ts":          "2026-05-13T22:00:01Z",
  "kind":        "ai.tool",
  "app":         "third.party.research",
  "tool":        "web.fetch",
  "verb":        "net.dial",
  "scope":       { "kind": "wild" },
  "args_hash":   "sha256:…",
  "result_hash": "sha256:…",
  "ok":          true,
  "session":     "…"
}
```

Together these two streams fully describe every action AI took on the
computer through Claw OS, scoped to whichever App initiated it.

## 9. What This Design Does **Not** Audit

By stated design:

- **App's non-AI code paths.** An App can `open("/tmp/x")` from its own
  Python without any audit row. That is intentional: this is not a
  syscall-level sandbox; it is an AI-accountability layer.

- **Data the App passes to a model.** `ai.chat` records a *hash* of the
  prompt, not the literal text. (Full text storage is an opt-in
  policy, not the default, for PII reasons.)

- **Down-stream effects of AI output.** If a model says "you should
  write file X" and the App's normal code obeys via `open()`, that
  write is App behavior, not AI behavior — and so is not in the AI
  audit log. The model output that *suggested* it is.

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

The kernel Agent and third-party App AIs share **the same Tool registry
implementation** (`core/src/ai/tools.rs`) but with **different views**:
the kernel Agent sees the full superset; an App AI sees only the Tools
declared in its own manifest, scope-narrowed to its own namespace.

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
   on the manifest `runtime` field (`python` / `node` / `binary`) and
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
7. **In-process SDK.** ✅ Done for Python. `apps/_lib/ai.py` exposes
   `ai.chat(..., tools=[...])` and an `AiResponse.tool_calls` field;
   `apps/_lib/tools.py` exposes `tools.call`, `tools.catalog`, and
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
