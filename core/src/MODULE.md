# Core Source Module

## Purpose

`core/src/` contains the implementation behind every core binary and the
library API exported by `core/src/lib.rs`.

## Responsibilities

- Route CLI and internal bridge commands.
- Implement system primitives, capability enforcement, audit, and sessions.
- Host the agent runtime and `clawd` broker.
- Connect apps, packages, services, browser, process, and model subsystems.

## Key Files

| Path | Role |
| --- | --- |
| `main.rs`, `router.rs` | `cos` entry and top-level dispatch |
| `router/app_commands.rs` | App lint/install/create/tool/consent management |
| `router/help.rs` | User help, built-in catalog, and command schemas |
| `../test/unit/router.rs` | Router help/schema/app/dispatch regression tests |
| `lib.rs` | Library module surface |
| `bin/` | `clawd` and helper binary entries |
| `agent/` | Agent runtime and AI-facing tools |
| `clawd/` | Privileged broker services |
| `caps/` | Capability model and enforcement |
| `model/` | Local/cloud model tasks and engines |
| `session/` | Session persistence |
| `notifications/` | Durable owner-scoped notification model, store, policy, and external delivery adapters |
| `apps.rs`, `bridge.rs` | App discovery and subprocess bridge |
| `service.rs`, `../test/unit/service.rs` | Managed service lifecycle and regressions |

## Dependencies

Read [`../MODULE.md`](../MODULE.md) for the core boundary and
[`../../ARCHITECTURE.md`](../../ARCHITECTURE.md) for project-wide dependency
rules. Top-level dispatch may depend on subsystems; subsystems must not depend
on CLI presentation.

## Tests

Unit test bodies mirror source paths under `core/test/unit/`. Source modules
include them only when `cfg(test)` is active:

```bash
cargo test -p cos <module-or-test-filter> -- --test-threads=1
```
