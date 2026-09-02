# @claw-os/sdk

Official **Node.js** SDK for [Claw OS](https://github.com/xiaoyu-work/claw-os).

This package is the public Node.js surface for building Claw OS Apps.
It provides a manifest-bound MCP service runtime, gated AI and catalog
tool clients, and desktop GUI bootstrap. Tool schemas come exclusively
from the App's verified `app.json`; Node code binds implementations to
those declared names.

## Install

```sh
npm install \
  https://github.com/xiaoyu-work/claw-os/releases/download/sdk-v0.1.0/claw-os-sdk-node-0.1.0.tgz
```

## Modules

| Module       | What                                                          | Equivalent CLI                       |
|--------------|---------------------------------------------------------------|--------------------------------------|
| `mcp`        | Manifest-bound MCP App service runtime over private stdio      | launched by the Claw App Host        |
| `ai`         | Stable gated `chat` / `chat-untrusted` access                 | `cos ai chat --app <id>`             |
| `tools`      | Call catalog tools the model proposes (`call`, `catalog`)      | `cos ai tool <name> --app <id>`      |
| `gui`        | Desktop GUI bootstrap (kernel context, agent overlay)          | launched via `cos app <id> --gui`    |
| `transport`  | The subprocess transport + error types (advanced)              | —                                    |
| (root)       | Typed structs generated from `wire/v1/*.schema.json`           | —                                    |

## MCP App service

```ts
import { mcp } from "@claw-os/sdk";

const app = mcp.App.fromManifest();

app.tool("notes.get", async ({ id }, call) => {
  call.throwIfCancelled();
  await call.reportProgress(1, { total: 2, message: "Loading note" });
  return { id, body: await notes.load(String(id)) };
});

await app.serve();
```

`App.fromManifest(path?)` reads one immutable manifest snapshot. An explicit
path wins, followed by `COS_APP_MANIFEST`, then `./app.json` for direct
development. Every name in `app.json.mcp.tools` must be bound before serving.
Descriptions, argument schemas, defaults, choices, and conditional requirements
remain manifest-owned.

Every call carries a Gateway-authenticated `mcp.CallContext`. Handlers receive
it as their second argument and can also use `mcp.currentContext()`. The context
provides immutable caller and lineage fields, an `AbortSignal`, deadline-aware
`throwIfCancelled()`, and progress reporting when the caller supplied a progress
token.

## AI and catalog tools

`ai` and `tools` are thin clients over wire protocol v1. They invoke `cos`,
which enforces capabilities, prompt-origin allowlists, monthly budget, safety,
and audit before model or computer operations run.

## AI support

- **Stable:** `ai.chat`. Setting `origin: "external-content"`
  automatically selects `ai.chat.untrusted`.
- Embed, image, vision, audio, and video helpers are currently unsupported.
  They throw `ai.AiUnsupported` before invoking `cos`.

### What you never do

- **Name a verb or pick a model.** You describe what you want; the gate
  picks the caps verb, and the machine owner configures the one
  provider/model in `/etc/cos/agent.toml`.
- **Import a provider SDK** (`openai`, `@anthropic-ai/sdk`, …). `cos app
  lint` refuses apps that do; this module is the only sanctioned route
  to a model.
- **Call `cos agent`.** That is the kernel's own Agent product. Apps use
  `cos ai` (the App-facing primitive) — which is what this SDK wraps.

## Errors

All errors extend `transport.BridgeError`:

- `ai.AiDenied` / `ai.AiBudgetExceeded` / `ai.AiSafetyViolation` — a gate
  refused the call; `.payload` carries the structured kernel envelope.
- `ai.AiUnavailable` / `tools.ToolUnavailable` — transport failure
  (binary missing, timeout, non-JSON output).
- `ai.AiUnsupported` — an unavailable multimodal helper was called.
- `tools.ToolDenied` — capability / unknown-tool / arg-shape refusal.

## Binary resolution

The SDK runs `cos` from `$PATH`. Override with `CLAW_COS_BIN` (or
`COS_BIN`) — used by tests and dev setups.

## Develop

```sh
npm install
npm run build      # tsc → dist/ (shipped; no test files)
npm test           # compiles + runs node:test, including in-memory MCP transport tests
```

Generated types in `src/generated.ts` are produced by
`python3 ../wire/codegen.py` — do not hand-edit them.
