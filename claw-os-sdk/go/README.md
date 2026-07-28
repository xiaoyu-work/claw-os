# claw-os-sdk (Go)

Official **Go** SDK for [Claw OS](https://github.com/xiaoyu-work/claw-os).

This module is the AI-facing surface a Go app uses to reach the kernel's
stable chat features, call other apps' tools, and bootstrap a desktop
GUI. Like the Python, Rust, and Node SDKs, it is a **thin client over
wire protocol v1**: supported operations shell out to the `cos` binary,
which enforces capabilities, prompt-origin allowlists, monthly budget,
the safety pipeline, and audit before any model or computer operation
runs.

## Install

```sh
go get github.com/xiaoyu-work/claw-os-sdk/go
```

```go
import clawossdk "github.com/xiaoyu-work/claw-os-sdk/go"
```

## Surface

| Area          | Functions                                                          | Equivalent CLI                    |
|---------------|--------------------------------------------------------------------|-----------------------------------|
| AI            | Stable `Chat` / chat-untrusted access; multimodal compatibility shims; `Budget` | `cos ai chat --app <id>`          |
| Tools         | `CallTool`, `Catalog`, `ForChat`                                   | `cos ai tool <name> --app <id>`   |
| GUI           | `IsGUILaunch`, `Context`, `(*GuiContext).OpenAgentOverlay`         | launched via `cos app <id> --gui` |
| Transport     | `CosBinary`, error types (advanced)                                | —                                 |
| (generated)   | Typed structs from `wire/v1/*.schema.json` (`generated.go`)        | —                                 |

## Usage

```go
package main

import clawossdk "github.com/xiaoyu-work/claw-os-sdk/go"

// A single entry serves both the one-shot operation and the GUI window.
func run(command string, args map[string]any) (any, error) {
	if clawossdk.IsGUILaunch(command) {
		ctx := clawossdk.Context(nil) // ctx.AppID, ctx.Files
		startMyWindow(ctx)            // your toolkit, your event loop
		return nil, nil
	}

	// Headless operation: summarise some untrusted text.
	res, err := clawossdk.Chat(args["body"].(string), clawossdk.ChatOptions{
		Origin:   "external-content", // strict safety pipeline for 3rd-party text
		MaxUnits: 2000,
		Tools:    clawossdk.ForChat("fs.read_text"),
	})
	if err != nil {
		return nil, err
	}

	for _, proposed := range res.ToolCalls {
		out, err := clawossdk.CallTool(proposed.Name, proposed.Input, "")
		if err != nil {
			return nil, err
		}
		_ = out.Value // feed into the next Chat turn however you like
	}

	return map[string]any{"summary": res.Text, "usage": res.Usage}, nil
}
```

## AI support

- **Stable:** `Chat`. Setting `Origin: "external-content"`
  automatically selects `ai.chat.untrusted`.
- **Compatibility only:** embed, image, vision, audio, and video helpers
  retain their signatures but are deprecated, experimental, and currently
  unsupported. They return `*AiUnsupportedError` before invoking `cos`.

### What you never do

- **Name a verb or pick a model.** You describe what you want; the gate
  picks the caps verb, and the machine owner configures the one
  provider/model in `/etc/cos/agent.toml`.
- **Import a provider SDK** (`openai`, `anthropic`, …). `cos app lint`
  refuses apps that do; this module is the only sanctioned route to a
  model.
- **Call `cos agent`.** That is the kernel's own Agent product. Apps use
  `cos ai` (the App-facing primitive) — which is what this SDK wraps.

## Errors

Each domain returns typed errors you can switch on:

- `*AiDeniedError` / `*AiBudgetExceededError` / `*AiSafetyViolationError`
  — a gate refused the call; `.Payload` carries the structured kernel
  envelope.
- `*AiUnavailableError` / `*ToolUnavailableError` — transport failure
  (binary missing, timeout, non-JSON output).
- `*AiUnsupportedError` — a multimodal compatibility shim was called.
- `*ToolDeniedError` — capability / unknown-tool / arg-shape refusal.

## Binary resolution

The SDK runs `cos` from `$PATH`. Override with `CLAW_COS_BIN` (or
`COS_BIN`) — used by tests and dev setups.

## Develop

```sh
go vet ./...
go test ./...     # runs against a fake `cos` (no kernel required)
```

Generated types in `generated.go` are produced by
`python3 ../wire/codegen.py` — do not hand-edit them.
