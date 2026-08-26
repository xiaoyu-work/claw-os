# @claw-os/sdk

Official **Node.js** SDK for [Claw OS](https://github.com/xiaoyu-work/claw-os).

This package is the AI-facing surface a Node app uses to reach the
kernel's stable chat features, call other apps' tools, and bootstrap a
desktop GUI. Like the Python and Rust SDKs, it is a **thin client over
wire protocol v1**: supported operations shell out to the `cos` binary,
which enforces capabilities, prompt-origin allowlists, monthly budget,
the safety pipeline, and audit before any model or computer operation
runs.

## Install

```sh
npm install \
  https://github.com/xiaoyu-work/claw-os/releases/download/sdk-v0.1.0/claw-os-sdk-node-0.1.0.tgz
```

## Modules

| Module       | What                                                          | Equivalent CLI                       |
|--------------|---------------------------------------------------------------|--------------------------------------|
| `ai`         | Stable gated `chat` / `chat-untrusted` access                | `cos ai chat --app <id>`             |
| `tools`      | Call catalog tools the model proposes (`call`, `catalog`)     | `cos ai tool <name> --app <id>`      |
| `gui`        | Desktop GUI bootstrap (kernel context, agent overlay)         | launched via `cos app <id> --gui`    |
| `transport`  | The subprocess transport + error types (advanced)             | —                                    |
| (root)       | Typed structs generated from `wire/v1/*.schema.json`          | —                                    |

## Usage

```ts
import { ai, tools, gui } from "@claw-os/sdk";

// A single entry serves both the one-shot operation and the GUI window.
export function run(command: string, args: Record<string, unknown>) {
  if (gui.isGuiLaunch(command)) {
    const ctx = gui.context();          // ctx.appId, ctx.files, ctx.ai, ctx.tools
    startMyWindow(ctx);                 // your toolkit, your event loop
    return;
  }

  // Headless operation: summarise some untrusted text.
  const res = ai.chat(String(args.body), {
    origin: "external-content",         // strict safety pipeline for 3rd-party text
    maxUnits: 2000,
    tools: tools.forChat("fs.read_text"),
  });

  for (const proposed of res.toolCalls) {
    const out = tools.call(proposed.name, proposed.input);
    // ...feed out.value into the next ai.chat turn however you like
  }

  return { summary: res.text, usage: res.usage };
}
```

## AI support

- **Stable:** `ai.chat`. Setting `origin: "external-content"`
  automatically selects `ai.chat.untrusted`.
- **Compatibility only:** embed, image, vision, audio, and video helpers
  retain their signatures but are deprecated, experimental, and currently
  unsupported. They throw `ai.AiUnsupported` before invoking `cos`.

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
- `ai.AiUnsupported` — a multimodal compatibility shim was called.
- `tools.ToolDenied` — capability / unknown-tool / arg-shape refusal.

## Binary resolution

The SDK runs `cos` from `$PATH`. Override with `CLAW_COS_BIN` (or
`COS_BIN`) — used by tests and dev setups.

## Develop

```sh
npm install
npm run build      # tsc → dist/ (shipped; no test files)
npm test           # compiles + runs node:test against a fake `cos`
```

Generated types in `src/generated.ts` are produced by
`python3 ../wire/codegen.py` — do not hand-edit them.
