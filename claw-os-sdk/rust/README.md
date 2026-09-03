# claw-os-sdk (Rust)

The official Rust SDK for Claw OS. Use this crate to talk to the
`cos` kernel CLI from a Rust app — typed, documented, audited.

## What's in it

| Module      | Purpose                                                                         |
|-------------|---------------------------------------------------------------------------------|
| `ai`        | Stable `chat` / `chat-untrusted` access through `cos ai chat`.                  |
| `mcp`       | Manifest-bound App MCP runtime, call context, and stdio transport.                |
| `tools`     | `cos ai tool <name>` — fulfil catalog tools the model proposed.                 |
| `gui`       | Desktop GUI bootstrap and kernel-provided launch context.                       |
| `envelope`  | Wire-v1 envelope adapter; SDKs handle the migration to native v1 transparently. |
| `generated` | Typed structs generated from `wire/v1/*.schema.json`.                          |

The crate does not export `policy`, `fs`, `exec`, `pkg`, `notify`, or
`net`. Those helpers belong to the unpublished, OS-internal
`cos-runtime` crate and are unavailable to third-party SDK consumers.
The `cos` kernel performs capability checks when public SDK operations run.
Native Apps expose agent-callable tools through `claw_os_sdk::mcp`; MCP is a
module of this SDK rather than a separate developer package.

## Expose manifest-declared MCP tools

The App manifest is the only source of identity, summaries, and input schemas:

```json
{
  "schema_version": 2,
  "id": "echo_app",
  "version": "1.0.0",
  "name": {"en": "Echo"},
  "mcp": {
    "transport": "stdio",
    "tools": [{
      "name": "echo",
      "summary": {"en": "Echo text"},
      "args": [{"name": "text", "kind": "text", "required": true}]
    }]
  }
}
```

Rust binds only the implementation:

```rust,no_run
use std::sync::Arc;

use async_trait::async_trait;
use claw_os_sdk::mcp::{App, CallContext, Tool, ToolResult};
use serde_json::Value;

struct Echo;

#[async_trait]
impl Tool for Echo {
    fn name(&self) -> &str {
        "echo"
    }

    async fn handle(&self, args: Value, context: CallContext) -> ToolResult {
        if let Err(cancelled) = context.check_cancelled() {
            return ToolResult::error(cancelled.to_string());
        }
        match args["text"].as_str() {
            Some(text) => ToolResult::text(text),
            None => ToolResult::error("validated text argument was unavailable"),
        }
    }
}

# async fn run() -> Result<(), Box<dyn std::error::Error>> {
let mut app = App::from_environment()?;
app.bind(Arc::new(Echo))?;
app.serve_stdio().await?;
# Ok(())
# }
```

`App::from_environment()` reads `COS_APP_MANIFEST` and falls back to
`app.json`. Calls require the Gateway-authenticated context under
`_meta["claw-os.dev/call-context"]`; handlers receive it as an immutable
`CallContext` snapshot with lineage, deadlines, cooperative cancellation, and
optional progress reporting. `CallContext::deadline_unix_ms()` exposes the
authenticated deadline exactly as the wire `u64`; `cancelled()` and
`check_cancelled()` enforce it without converting it to a platform system-time
range.

## Add it

```toml
[dependencies]
claw-os-sdk = {
    git = "https://github.com/xiaoyu-work/claw-os.git",
    tag = "sdk-v0.1.0",
}
```

## Use it

```rust
use claw_os_sdk::ai;

fn summarise_email(body: &str) -> Result<String, Box<dyn std::error::Error>> {
    let response = ai::chat(
        body,
        ai::ChatOpts::default()
            .origin("external-content")
            .max_units(2000),
    )?;
    Ok(response.text)
}
```

## AI support

`ai::chat` is the public model API. Setting origin to
`"external-content"` automatically selects `ai.chat.untrusted`. Unsupported
modalities are not published as placeholder APIs.

## Configuration

| Env var          | Effect                                                                |
|------------------|-----------------------------------------------------------------------|
| `CLAW_COS_BIN`   | Path to the `cos` binary. Defaults to looking up `cos` in `$PATH`.    |
| `COS_APP_ID`     | App id, required for `ai::chat` and `tools::call` unless passed via `.app(...)`. |
| `COS_APP_MANIFEST` | Authoritative manifest path used by `mcp::App::from_environment()`. |

## Wire protocol

This crate implements wire protocol v1. See
[`../wire/v1/README.md`](../wire/v1/README.md) for the full spec.
Regenerate typed structs with:

```sh
python3 ../wire/codegen.py
```
