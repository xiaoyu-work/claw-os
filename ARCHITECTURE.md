# Claw OS Architecture

## Purpose

Claw OS is a Debian-based operating environment built around a system-level AI
agent. The `cos` command exposes structured system primitives and agent
workflows; `clawd` brokers privileged, session-scoped work; manifests and
capabilities describe what apps and tools may do; image tooling packages the
same composed rootfs for WSL, Docker, VM, ISO, Azure, and desktop targets.

This document maps the source architecture. Image identity and packaging detail
remain in [`docs/image-architecture.md`](docs/image-architecture.md).

## System Context

```text
User / desktop / external MCP client
                 |
                 v
        cos CLI / local bridge
                 |
       +---------+----------+
       |                    |
       v                    v
 structured primitives   agent runtime
       |                    |
       +-------> clawd <-----+
                  |
          capability + audit boundary
                  |
       system services / apps / adapters
```

The model never owns system authority. It sees only tools admitted by the tool
registry and capability/guardrail layers. Privileged execution crosses the
`clawd` broker or a policy-enforced primitive.

## Components

| Component | Responsibility | Primary source |
| --- | --- | --- |
| `cos` CLI and router | Parse output format, dispatch primitives, apps, hidden bridges, and `cos agent` subcommands | `core/src/main.rs`, `core/src/router.rs` |
| `clawd` broker | Unix-socket RPC, peer/session identity, privileged dispatch, task worker, and audit hook | `core/src/bin/clawd.rs`, `core/src/clawd/server.rs` |
| Agent runtime | Multi-turn model/tool loop, prompt assembly, hooks, progress, compression, and tool dispatch | `core/src/agent/runtime/` |
| LLM abstraction | Provider registry, wire adapters, streaming accumulation, fallback chain, credentials, and usage | `core/src/agent/llm/` |
| Tool/capability layer | Model-visible tool registry, guardrails, MCP attachment, scope checks, and approval boundaries | `core/src/agent/tools/`, `core/src/caps/` |
| Memory and sessions | SQLite/FTS memory, semantic recall, session/message persistence, curation, and checkpoints | `core/src/agent/memory/`, `core/src/session/`, `core/src/checkpoint.rs` |
| Audit | Hash-chained JSONL events and agent audit/query commands | `core/src/audit.rs`, `core/src/agent/audit_cli.rs` |
| Apps and adapters | Declarative operation manifests plus Python runtime handlers | `apps/`, `adapters/`, `core/src/apps.rs` |
| SDK/runtime | Public app SDKs and internal bundled-app policy helpers | `claw-os-sdk/`, `cos-runtime/` |
| Browser and semantic services | Obscura browser stack, `cos-browser`, embedding and semantic-search services | `crates/obscura-*`, `crates/cos-browser`, `crates/claw-*` |
| Desktop | Product desktop fork and native UI clients communicating through stable OS boundaries | `desktop/` |
| Image composition | Reusable rootfs features and profile definitions | `rootfs/`, `scripts/lib/image-profiles.sh` |
| Website | Framework-free marketing site composed into the GitHub Pages/APT artifact | `web/`, `packaging/apt-repo/build-repo.sh` |
| Distribution | WSL/Docker/VM/ISO/Azure packaging, Debian packages, signed APT repo, releases | `targets/`, `packaging/`, `.github/workflows/` |

## Dependency Rules

Dependencies point from entry points and orchestration toward stable
definitions, then to providers; authorization and persistence remain explicit
cross-cutting boundaries rather than hidden implementation details.

### Capability seams

A replaceable system capability has three roles:

1. **Definition** — a trait or explicit interface in a stable module.
2. **Provider** — one or more implementations behind that definition.
3. **Consumer** — a tool or subsystem depending on the definition.

Consumers do not import around the definition to reach concrete providers.
This keeps local, sandboxed, and remote implementations interchangeable.

### Authority and policy

- `clawd` is the privileged broker boundary.
- The tool registry filters tools before model exposure.
- Runtime dispatch runs guardrails and pre/post hooks around tool calls.
- App manifests declare capability needs; bundled apps enforce them through
  `cos_runtime.policy`.
- A model response is data, never authorization.

### AI ownership

Apps use the Claw OS SDK/agent gate rather than provider SDKs. Provider choice,
credentials, consent, budgets, model-visible logging, and fallback behavior
remain owned by the core agent.

### Persistence and observability

Anything inserted into a model request must be reconstructable from session or
audit records. Prompt injections, memory, tool calls/results, provider usage,
approvals, and privileged actions cannot bypass the recording path.

### Build and distribution

- Rootfs features are reusable capability units.
- `scripts/lib/image-profiles.sh` is the profile source of truth.
- Targets package a profile; they do not independently redefine OS contents.
- Debian packages are assembled from built binaries and source-tree files and
  do not require a rootfs build.
- Docker and WSL share one rootfs per architecture in their combined workflow.

### Vendored boundaries

- `desktop/` is a product fork originating from COSMIC. Component licenses and
  `desktop/PROVENANCE.md` remain authoritative.
- `crates/obscura-*` are vendored browser-engine internals used through the
  workspace crates.
- Generated bindings and build outputs are regenerated, not edited directly.

## Data Flow

The following flows identify where input enters, where authority is checked,
and where results become persistent or externally visible.

### Structured CLI primitive

```text
core/src/main.rs
  -> router::dispatch
  -> primitive module or clawd client
  -> capability/policy check
  -> structured JSON result
  -> requested output formatter
```

Hidden router bridges such as `__policy`, `__memory`, `__package`, and
`__systemd` are internal protocol surfaces used by bundled apps and services.

### Agent ask/chat turn

```text
CLI / web UI / bridge
  -> clawd agent task client (for daemon-backed work)
  -> runtime::loop_
  -> traced system prompt + persisted conversation
  -> Provider::chat or Provider::chat_stream
  -> StreamEvent accumulation
  -> user-visible stream projection (tool identity only; evidence markers hidden)
  -> tool registry / guardrails / hooks
  -> parallel-safe or serial tool execution
  -> tool results appended to conversation
  -> repeat until final response or max_turns
  -> final no-tools synthesis when the work limit is reached
  -> usage/session/audit records
```

`core/src/agent/runtime/turn.rs` is the main seam where model output, tool
authorization, execution ordering, hooks, and conversation history meet.
The projection in `core/src/agent/runtime/presentation.rs` affects display
events only; complete tool inputs/results remain in the runtime trajectory,
session memory, audit records, and evidence verifier.

### App invocation

```text
apps/<id>/app.json
  -> core app discovery and manifest validation
  -> operation schema / capability derivation
  -> app session registration
  -> Python main.py run(command, args)
  -> policy-enforced SDK/runtime calls
  -> structured result
```

Manifest/schema discovery must remain side-effect free and must not execute the
app entrypoint.

### MCP attachment

```text
config or discovered agent-API sidecar
  -> MCP transport/client initialization
  -> tools/list
  -> prefixed tool registration
  -> normal tool registry + guardrail dispatch
```

An MCP server is optional. Failure to attach one is logged and skipped rather
than preventing the core agent from starting.

### Image and package publication

```text
source + compiled binaries
  + image profile
  -> rootfs/build.sh
  -> shared rootfs
  -> Docker image + WSL package

source + compiled binaries
  -> packaging/deb/build-debs.sh
  -> amd64/arm64 .deb artifacts
  -> signed APT repository
  -> GitHub Pages
```

Workflows are manually dispatched. `release.yml` runs tests and fans out to the
combined Docker/WSL channel and the independent APT channel.

## Entry Points

| Entry point | Path | Notes |
| --- | --- | --- |
| `cos` | `core/src/main.rs` | User-facing CLI and structured primitive router |
| `clawd` | `core/src/bin/clawd.rs` | System daemon and privileged broker |
| `cos agent ...` | `core/src/agent/mod.rs` | Agent CLI command family |
| Agent loop | `core/src/agent/runtime/loop_.rs` | Multi-turn orchestration |
| One agent turn | `core/src/agent/runtime/turn.rs` | Provider call and tool execution |
| App discovery | `core/src/apps.rs` | `app.json` loading and schema generation |
| App runtime | `apps/<id>/main.py` | Bundled operation implementation |
| Rootfs build | `rootfs/build.sh` | Feature composition |
| Profile definitions | `scripts/lib/image-profiles.sh` | Target feature sets |
| Top-level target build | `build.sh` | WSL/Docker/VM/ISO/Azure dispatcher |
| Debian package build | `packaging/deb/build-debs.sh` | Installed package artifacts |
| APT repository build | `packaging/apt-repo/build-repo.sh` | Signed repository assembly |
| CI test workflow | `.github/workflows/test.yml` | Manual/reusable test workflow |
| Full release workflow | `.github/workflows/release.yml` | Manual umbrella publication |

## Cross-Cutting Constraints

- Core tests share process-global environment variables; combined runs use one
  test thread.
- Capability checks validate untrusted identifiers and paths before privileged
  execution.
- Provider streaming and non-streaming paths must preserve equivalent text,
  tools, reasoning state, usage, and errors.
- Credential values never enter config files, logs, model prompts, or error
  messages.
- Memory and audit stores are append/transaction oriented; schema and recovery
  behavior require regression tests.
- Rootfs/image builds require Linux filesystem semantics and root privileges.
- Windows case-insensitive checkouts cannot faithfully represent every
  case-colliding desktop symlink.
- Public SDK wire changes regenerate every language binding and retain
  backwards-compatible serialization unless explicitly versioned.

## Related Design Documents

- [`docs/app-development.md`](docs/app-development.md)
- [`docs/image-architecture.md`](docs/image-architecture.md)
- [`docs/semantic-search-design.md`](docs/semantic-search-design.md)
- [`docs/browser-attached-design.md`](docs/browser-attached-design.md)
- [`packaging/README.md`](packaging/README.md)
- [`docs/updating.md`](docs/updating.md)
- [`desktop/README.md`](desktop/README.md)
