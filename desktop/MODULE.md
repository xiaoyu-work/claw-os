# Desktop Module

## Purpose

`desktop/` is the Claw OS desktop product fork, composed of independent COSMIC
component crates plus Claw-specific agent bridges and applets.

## Responsibilities

- Provide compositor, session, panel, launcher, settings, desktop apps, and UI
  toolkit components.
- Preserve component licensing and provenance while evolving the product fork.
- Keep privileged/AI implementation behind stable core process boundaries.
- Build each component through its own Cargo or just workspace.

## Key Files

| Path | Role |
| --- | --- |
| `README.md` | Component map, build instructions, product-fork boundary |
| `PROVENANCE.md` | Upstream origin and revision per component |
| `justfile` | Desktop build/install orchestration |
| `agent/` | Native agent bridge and UI |
| `agent/protocol/` | Versioned desktop Agent HTTP/SSE presentation contract |
| `comp/`, `session/`, `panel/` | Shell/compositor/session surfaces |
| `settings/`, `settings-daemon/` | System settings UI and services |
| `toolkit/`, `text/`, `theme/` | Shared UI/rendering foundations |

## Dependencies

Desktop processes communicate with core through stable CLI, HTTP/SSE, DBus,
Wayland, SDK, or MCP boundaries. Preserve licenses and avoid pulling privileged
agent logic into GPL desktop processes. Component workspaces remain independent
of the root Rust workspace.

The Agent UI and bridge share `agent/protocol/`; endpoint and stream DTOs must
not be duplicated in either binary. Core/clawd models terminate at the bridge's
anti-corruption translation layer and never become UI state.

## Tests

Each desktop crate stores private-access unit-test bodies under its own
`test/unit/` tree, mirroring `src/`. Cargo integration tests remain in the
standard crate-level `tests/` directory.

Use the owning component/workspace manifest:

```bash
cargo test --manifest-path desktop/<component>/Cargo.toml -- --test-threads=1
```
