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
| `rust/src/ask_claw.rs` | Typed context serialization, authenticated/readiness-gated Unix sockets, process isolation, and asynchronous child supervision |
| `README.md` | Boundary and usage |

## Dependencies

Only bundled/trusted apps depend on `cos-runtime`. Third-party apps use
`claw-os-sdk`. Policy helper errors are surfaced; missing `cos` or a denied
decision never silently becomes allow.

Inherited desktop apps keep app-specific `Serialize` context structs in their
small `claw_glue` modules and call `cos_runtime::ask_claw::launch`. They do not
name the Agent UI executable, construct activation flags, or assemble JSON.
The Agent UI consumes the same runtime-owned activation type; context-bearing
launches remain transient while context-free activation may use D-Bus.

Context payloads are capped at 32 KiB and serialized inside a typed activation.
The runtime directly spawns a transient UI and waits for readiness on an
inherited Unix socketpair before writing a bounded frame. Public SDKs use the
packaged helper's abstract listener, authenticated to the direct parent with
`SO_PEERCRED`. No payload crosses argv, pipes, D-Bus,
audit, registry, environment, or filesystem boundaries. The handoff requires
Yama ptrace isolation and marks parent and child non-dumpable; startup failure
or timeout kills and reaps the exact child, while the launcher thread reaps a
successfully activated UI when it exits.

The production executable is fixed at `/usr/local/bin/cos-agent-ui` and must
pass regular-file, root-owner, executable, and non-writable checks. Test
injection is private to the runtime's `cfg(test)` module.

## Tests

Rust runtime unit tests live under `rust/test/unit/`.

```bash
PYTHONPATH=claw-os-sdk/python/src:cos-runtime/python/src \
  python3 -m pytest -q cos-runtime/python/src
cargo test -p cos-runtime
```
