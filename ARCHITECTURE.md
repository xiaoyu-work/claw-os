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
| Apps and adapters | Declarative operation manifests plus Python, Node, shell, or binary runtime handlers | `apps/`, `adapters/`, `core/src/apps.rs`, `core/src/bridge.rs` |
| SDK/runtime | Public app SDKs and internal bundled-app policy helpers | `claw-os-sdk/`, `cos-runtime/` |
| Browser and semantic services | Obscura browser stack, `cos-browser`, embedding and semantic-search services | `crates/obscura-*`, `crates/cos-browser`, `crates/claw-*` |
| Desktop | Product desktop fork and native UI clients communicating through stable OS boundaries | `desktop/` |
| Image composition | Reusable rootfs features and profile definitions | `rootfs/`, `scripts/lib/image-profiles.sh` |
| Web desktop | React/Vite Linux desktop whose browser opens the embedded marketing site; independently built before Pages composition | `web/`, `.github/workflows/publish-website.yml` |
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

Semantic primitives have a one-way dependency boundary: `claw-embed` owns
embedding, extraction, chunking, walking, and storage contracts;
`claw-semantic` depends on those contracts and owns only filesystem daemon
lifecycle, configuration, service orchestration, and user-facing commands.

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
  -> traced system prompt + metadata-only Skill catalogue + persisted conversation
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

### Agent-initiated account authorization

```text
bundled App returns auth_required + constrained setup.agent_action
  -> system Agent calls dedicated cos_oauth_login tool
  -> exact secret.write scopes are checked before provider interaction
  -> trusted system browser handles user login and consent
  -> access/refresh tokens go directly to the encrypted credential store
  -> model receives authorization status and scopes, never token values
  -> system Agent retries the original App operation once
```

The public CLI route accepts interactive OAuth only from a same-process Admin
session running the direct credential command. The model route is a separate,
strictly shaped built-in tool available only to attended local `agent ask`,
`agent live`, and `agent chat` sessions; its token destination is fixed to the
default credential namespace and exact credential capability checks still
apply. Delegate children and inbound MCP servers exclude the tool. OAuth client
registration remains runtime system configuration and is never embedded in
packages or entered through model chat.

### Progressive Agent Skill disclosure

```text
/usr/lib/cos/skills (read-only trusted vendor Skills)
  + per-user data/agent/skills (user-installed Skills)
  -> layered loader; built-in ids cannot be silently shadowed
  -> metadata-only catalogue injected into and recorded with the system prompt
  -> cos_skill read: disclose one matching SKILL.md instruction body
  -> cos_skill resource: disclose one explicitly requested child resource
  -> normal tool trajectory, session logging, and Skill usage record
```

Built-in Skills from the package-owned default root do not require third-party
provenance approval. A `COS_SYSTEM_SKILLS_DIR` override is treated as local
content rather than promoted to built-in trust. User-installed Skills continue
through the existing non-vendor disclosure guard; signature policy remains
enforced when bundles are installed. Metadata pages, instruction bodies, and
child resources are size-bounded. Child resource reads accept only visible,
regular UTF-8 files beneath the selected Skill directory; absolute paths,
parent traversal, symlinks, hidden files, and oversized resources are rejected.

### App invocation

```text
apps/<id>/app.json
  -> core app discovery and manifest validation
  -> operation schema / validated default binding / capability derivation
  -> app session registration
  -> declared Python / Node / shell / binary entrypoint with effective args
  -> policy-enforced SDK/runtime calls
  -> structured result
```

Manifest/schema discovery must remain side-effect free and must not execute the
app entrypoint.

App identity and capabilities are issued by the session authority, never
asserted by the launcher. For an unprivileged launch the request names only the
App, launch kind, operation, and arguments; `clawd` re-reads the installed
manifest, derives capabilities from the requested operation plus the validated
arguments, and bounds them by the launcher authority it resolves from the peer's
process ancestry. A launcher the daemon cannot tie to a registered session gets
an unprivileged, home-bounded policy ceiling instead: machine-mutating verbs and
global filesystem authority require either an authenticated parent session or an
approved permission grant bound to that exact peer process. A launch that needs
consent is answered with the ids of the requests the daemon filed; the launcher
process waits on `permission.status` with a bounded timeout and retries over the
same connection, and grants are settled all-or-none and retired on first use.
Registration returns an opaque, launcher-bound, single-use launch handle that
authorises the pid bind, per-call capability updates, and teardown for that one
session; the launched App never receives it.

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
  -> signed APT repository artifact

web source
  -> npm run build
  -> independent web artifact

signed APT repository artifact + web artifact
  -> Pages composition
  -> GitHub Pages
```

`test.yml` runs on pull requests and remains manually dispatchable/reusable.
Publication workflows are manually dispatched; `release.yml` runs tests and
fans out to the combined Docker/WSL channel and the independent APT channel.

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
| Web desktop | `web/src/App.tsx` | Browser-based Linux desktop composition |
| Website publication | `.github/workflows/publish-website.yml` | Independent web build and Pages composition |
| CI test workflow | `.github/workflows/test.yml` | Pull-request/manual/reusable test workflow |
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
