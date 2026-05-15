# claw-os-sdk (Rust)

[![Crates.io](https://img.shields.io/crates/v/claw-os-sdk.svg)](https://crates.io/crates/claw-os-sdk)

The official Rust SDK for Claw OS. Use this crate to talk to the
`cos` kernel CLI from a Rust app — typed, documented, audited.

## What's in it

| Module    | Purpose                                                                        |
|-----------|--------------------------------------------------------------------------------|
| `ai`      | `cos ai chat / embed / image-generate / vision-analyze / audio-tts / ...`      |
| `policy`  | `cos perms check / grant` — call before every gated side effect.               |
| `tools`   | `cos ai tool <name>` — fulfil catalog tools the model proposed.                |
| `fs`      | `cos app fs ls / read / write / stat / search / recent`                        |
| `exec`    | `cos app exec ...`                                                             |
| `pkg`     | `cos app pkg has / list / install`                                             |
| `notify`  | `cos app notify ...`                                                           |
| `net`     | `cos app net ...`                                                              |
| `envelope`| Wire-v1 envelope adapter; SDKs handle the migration to native v1 transparently.|
| `generated` | Typed structs generated from `wire/v1/*.schema.json`.                        |

## Add it

```toml
[dependencies]
claw-os-sdk = "0.1"
```

## Use it

```rust
use claw_os_sdk::{ai, policy};

fn summarise_email(body: &str) -> Result<String, Box<dyn std::error::Error>> {
    policy::require("ai.chat.untrusted", policy::Scope::Unscoped)?;
    let response = ai::chat(
        body,
        ai::ChatOpts::default()
            .origin("external-content")
            .max_units(2000),
    )?;
    Ok(response.text)
}
```

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
