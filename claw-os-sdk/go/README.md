# claw-os-sdk (Go)

Official **Go** SDK for [Claw OS](https://github.com/xiaoyu-work/claw-os).

This module is the public surface a Go App uses to expose MCP tools, reach the
kernel's stable chat and fixed tool catalog, and bootstrap a desktop GUI.
It does not allow App-to-App invocation. Like the Python, Rust, and Node SDKs,
its outbound transport is a **thin client over
wire protocol v1**: supported operations shell out to the `cos` binary,
which enforces capabilities, prompt-origin allowlists, monthly budget,
the safety pipeline, and audit before any model or computer operation
runs.

## Install

```sh
go get github.com/xiaoyu-work/claw-os/claw-os-sdk/go@v0.1.0
```

```go
import clawossdk "github.com/xiaoyu-work/claw-os/claw-os-sdk/go"
```

## Surface

| Area          | Functions                                                          | Equivalent CLI                    |
|---------------|--------------------------------------------------------------------|-----------------------------------|
| AI            | `Chat` / chat-untrusted access; `Budget`                         | `cos ai chat --app <id>`          |
| Tools         | `CallTool`, `Catalog`, `ForChat`                                   | `cos ai tool <name> --app <id>`   |
| MCP service   | `LoadMCPApp`, `(*MCPApp).Bind`, `Serve`, `ServeStdio`               | App Host private stdio transport  |
| GUI           | `IsGUILaunch`, `Context`, `(*GuiContext).OpenAgentOverlay`         | launched via `cos app <id> --gui` |
| Objects       | `FormatReference`, `ParseReference`                               | Pure identifiers; no transport |
| Transport     | `CosBinary`, error types (advanced)                                | —                                 |
| (generated)   | Typed structs from `wire/v1/*.schema.json` (`generated.go`)        | —                                 |

## Usage

```go
package main

import clawossdk "github.com/xiaoyu-work/claw-os/claw-os-sdk/go"

// A single entry serves both the one-shot operation and the GUI window.
func run(command string, args map[string]any) (any, error) {
	if clawossdk.IsGUILaunch() {
		ctx, err := clawossdk.Context(nil)
		if err != nil {
			return nil, err
		}
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

## MCP App service

Go Apps expose Agent-facing tools only through the authoritative
`app.json.mcp.tools` declaration. `LoadMCPApp("")` loads
`$COS_APP_MANIFEST`, or `./app.json` during direct development.
`Bind` only accepts declared names, and `Serve` refuses to start until every
declared tool has exactly one handler.

```go
app, err := clawossdk.LoadMCPApp("")
if err != nil {
	return err
}

err = app.Bind("notes.get", func(
	args map[string]any,
	call *clawossdk.MCPCall,
) (any, error) {
	authenticated := call.Authenticated()
	if err := call.CheckCancelled(); err != nil {
		return nil, err
	}

	if call.ProgressRequested() {
		if err := call.ReportProgress(1, clawossdk.MCPProgress{
			Message: "Loading note",
		}); err != nil {
			return nil, err
		}
	}

	return clawossdk.StructuredMCPResult(map[string]any{
		"id":     args["id"],
		"caller": authenticated.Caller.Id,
	}, "Note loaded")
})
if err != nil {
	return err
}
return app.ServeStdio()
```

`MCPCall` implements `context.Context`, so handlers can select on
`call.Done()`, inspect deadlines, and pass the call to context-aware APIs.
Authenticated caller and lineage data comes only from the Gateway-injected
`McpCallContext`; never derive identity from tool arguments.

Handlers may return ordinary values or explicit results:

- `TextMCPResult(text)` for successful text.
- `ErrorMCPResult(message)` for an MCP tool error.
- `StructuredMCPResult(object, text)` for `structuredContent` plus text.

`Serve(reader, writer)` is available for embedding and tests. It speaks
newline-delimited MCP JSON-RPC 2.0, serializes writes, supports progress,
cancellation, and authenticated deadlines, and returns fatal transport errors.

## App object references

```go
func objectReferenceExample() error {
	ref := clawossdk.ObjectRef{
		AppId: "notes", ObjectType: "note", ObjectId: "draft/1",
	}
	uri, err := clawossdk.FormatReference(ref)
	if err != nil {
		return err
	}
	_, err = clawossdk.ParseReference(uri) // app://notes/note?id=draft%2F1
	return err
}
```

`ObjectRef` is generated; `Revision` is `*string` so absence differs from a
present empty revision. Invalid references return `*ObjectRefError`.
Formatting and parsing preserve opaque IDs, reject invalid UTF-8 and
noncanonical URIs, and perform no discovery, filesystem access, or dispatch.
A reference does not assert existence, freshness, or permission.

Use generated `ValidateObjectRef` on decoded JSON before materializing the
struct. See [the shared contract](../wire/v1/object-references.md) for bounds
and the optional App manifest `objects` resolver declaration.

## AI support

`Chat` is the public model API. Setting `Origin: "external-content"`
automatically selects `ai.chat.untrusted`. Unsupported modalities are not
published as placeholder APIs.

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
- `*ToolDeniedError` — capability / unknown-tool / arg-shape refusal.
- `*ObjectRefError` — pure object-reference validation; no operation was invoked.

## Binary resolution

The SDK runs `cos` from `$PATH`. Override with `CLAW_COS_BIN` for
tests and dev setups.

## Develop

```sh
go vet ./...
go test -count=1 ./...  # fake cos; also reloads shared wire vectors outside this module
```

Generated types in `generated.go` are produced by
`python3 ../wire/codegen.py` — do not hand-edit them.
