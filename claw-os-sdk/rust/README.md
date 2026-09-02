# claw-os-sdk (Rust)

The official Rust SDK for Claw OS. Use this crate to talk to the
`cos` kernel CLI from a Rust app — typed, documented, audited.

## What's in it

| Module      | Purpose                                                                         |
|-------------|---------------------------------------------------------------------------------|
| `ai`        | Stable `chat` / `chat-untrusted` access through `cos ai chat`.                  |
| `mcp`       | Native App MCP server, tool, protocol, and stdio transport APIs.                 |
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

- **Stable:** `ai::chat`. Setting origin to `"external-content"`
  automatically selects `ai.chat.untrusted`.
- **Compatibility only:** embed, image, vision, audio, and video helpers
  retain their signatures but are deprecated, experimental, and currently
  unsupported. They return `AiError::UnsupportedModality` before invoking
  `cos`.

## Configuration

| Env var          | Effect                                                                |
|------------------|-----------------------------------------------------------------------|
| `CLAW_COS_BIN`   | Path to the `cos` binary. Defaults to looking up `cos` in `$PATH`.    |
| `COS_APP_ID`     | App id, required for `ai::chat` and `tools::call` unless passed via `.app(...)`. |

## Wire protocol

This crate implements wire protocol v1. See
[`../wire/v1/README.md`](../wire/v1/README.md) for the full spec.
Regenerate typed structs with:

```sh
python3 ../wire/codegen.py
```

## History

This crate was previously known as `claw-bridge` (under
`crates/claw-bridge`) and was internal-only. It has been promoted to
the public, multi-language SDK at `claw-os-sdk/rust` and renamed.
There is no compatibility shim — claw-os is pre-1.0 and breaking
changes are allowed.
