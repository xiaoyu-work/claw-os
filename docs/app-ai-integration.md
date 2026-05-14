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

## 3. The Tool Catalog

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

## 4. Manifest

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

## 5. CLI

### 5.1 `cos ai chat`

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

### 5.2 `cos ai tool`

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

## 6. SDK (Python)

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

## 7. Audit Surface

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

## 8. What This Design Does **Not** Audit

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

## 9. Relationship to the Kernel Agent

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

## 10. Implementation Phases

This document is the contract. Concrete work falls into roughly:

1. Build `ai_tools::CATALOG` — the shared registry, capabilities, scopes.
2. Implement `cos ai tool <name> --app <id> --args <json>`.
3. Implement `cos ai chat --tools …` (extend the existing one-shot path
   to inject Tool schemas and return `tool_calls`).
4. Extend `_lib/ai.py` and add `_lib/tools.py`.
5. Extend manifest validator to accept and verify the `ai.tools[]`
   section.
6. Extend installer's consent UI to render `why` text per Tool.
7. Define audit row shape and wire it into `cos app log`.
8. Write `docs/app-ai-tool-catalog.md` with the per-Tool reference.

Each phase is independently shippable.
