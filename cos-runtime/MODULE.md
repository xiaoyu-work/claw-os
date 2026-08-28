# Bundled App Runtime Module

## Purpose

`cos-runtime/` provides internal runtime and policy helpers for apps bundled
with Claw OS.

## Responsibilities

- Perform capability policy checks against the hidden core bridge.
- Provide internal runtime/session helpers consistently across languages.
- Own the typed, bounded Ask Claw context and desktop overlay launch contract.
- Keep bundled-app conveniences separate from the public SDK.

## Key Files

| Path | Role |
| --- | --- |
| `python/src/cos_runtime/` | Python policy/runtime helpers |
| `rust/` | Rust internal runtime crate |
| `rust/src/ask_claw.rs` | Typed context serialization, private read-once file handoff, activation arguments, executable selection, and supervised overlay launch |
| `README.md` | Boundary and usage |

## Dependencies

Only bundled/trusted apps depend on `cos-runtime`. Third-party apps use
`claw-os-sdk`. Policy helper errors are surfaced; missing `cos` or a denied
decision never silently becomes allow.

Inherited desktop apps keep app-specific `Serialize` context structs in their
small `claw_glue` modules and call `cos_runtime::ask_claw::launch`. They do not
name the Agent UI executable, construct activation flags, or assemble JSON.
The Agent UI consumes the same runtime-owned activation type, so launch and
single-instance D-Bus activation cannot drift.

Context payloads are capped at 32 KiB and staged as `0600` files beneath a
private `XDG_RUNTIME_DIR` child. Only the path crosses argv/audit/process
registry boundaries. The UI validates, unlinks, and reads the file once;
failed launches clean up immediately and later launches reap files older than
ten minutes.

## Tests

Rust runtime unit tests live under `rust/test/unit/`.

```bash
PYTHONPATH=claw-os-sdk/python/src:cos-runtime/python/src \
  python3 -m pytest -q cos-runtime/python/src
cargo test -p cos-runtime
```
