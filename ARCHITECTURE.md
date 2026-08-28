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
| `clawd` broker | Versioned framed Unix-socket RPC, per-message peer identity, declarative route registry, mandatory capability-authority middleware, privileged dispatch, task ownership/lease, agent-worker supervision, and audit hook | `core/src/bin/clawd.rs`, `core/src/clawd/server.rs`, `core/src/clawd/transport/`, `core/src/clawd/routes.rs`, `core/src/clawd/authority/` |
| `claw-agentd` worker | Unprivileged per-task process that runs the model/tool loop after privilege drop; grant-authenticated private job channel | `core/src/bin/claw-agentd.rs`, `core/src/agentd/` |
| Agent runtime | Multi-turn model/tool loop, prompt assembly, hooks, progress, compression, and tool dispatch | `core/src/agent/runtime/` |
| LLM abstraction | Provider registry, wire adapters, streaming accumulation, fallback chain, credentials, and usage | `core/src/agent/llm/` |
| Tool/capability layer | Model-visible tool registry, guardrails, MCP attachment, scope checks, and approval boundaries | `core/src/agent/tools/`, `core/src/caps/` |
| Memory and sessions | SQLite/FTS memory, semantic recall, session/message persistence, curation, and checkpoints | `core/src/agent/memory/`, `core/src/session/`, `core/src/checkpoint.rs` |
| Audit | Hash-chained JSONL events and agent audit/query commands | `core/src/audit.rs`, `core/src/agent/audit_cli.rs` |
| Apps and adapters | Declarative operation manifests plus Python, Node, shell, or binary runtime handlers | `apps/`, `adapters/`, `core/src/apps.rs`, `core/src/bridge.rs` |
| SDK/runtime | Public app SDKs and internal bundled-app policy helpers | `claw-os-sdk/`, `cos-runtime/` |
| Browser and semantic services | Obscura browser stack, `cos-browser`, embedding and semantic-search services | `crates/obscura-*`, `crates/cos-browser`, `crates/claw-*` |
| Desktop | Product desktop fork and native UI clients communicating through stable OS boundaries; the Agent UI and bridge share a versioned presentation protocol | `desktop/`, `desktop/agent/protocol/` |
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

Bundled desktop apps launch Ask Claw through `cos_runtime::ask_claw`. Their
thin `claw_glue` adapters define only typed, app-specific context fields; the
runtime owns bounded JSON serialization, anonymous process-bound stdin
handoff, executable selection, readiness and write deadlines, exact-child
reaping, and the activation contract consumed by the Agent UI. Context payloads
never enter process argv, D-Bus, audit entries, a process registry, environment,
or filesystem. Context-bearing requests use isolated transient overlay
processes; only context-free activation uses the well-known D-Bus
single-instance path.

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

### Broker request admission

```text
client (cos / bridge / rollback / approval helper)
  -> one length-prefixed v1 frame on /run/cos/clawd.sock
  -> connection slot for the peer's accounting bucket
  -> recvmsg: kernel SCM_CREDENTIALS, SCM_RIGHTS closed and refused
  -> /proc re-verification of the sending process
  -> versioned envelope parse (no legacy fallback)
  -> route lookup -> typed deny_unknown_fields decode
  -> access class -> global / per-principal / per-route slots
  -> duplicate check for mutations
  -> capability authority: resolve the route's grant, verify the principal,
     audience and subject, spend the required capabilities
  -> handler under the route's budget, re-checking through the same decision
  -> refuse the answer if the route owed a capability check and skipped it
  -> typed boundary error mapping and route-owned audit projection
  -> one bounded response frame, then close
```

Every message on the broker socket is one frame: a fixed 10-byte header of
magic, kind, flags and a big-endian length, then that many bytes of UTF-8 JSON.
The length is checked against the direction's ceiling before anything is
allocated, and a short read is a truncation rather than a record the daemon
waits on. `core/src/clawd/wire/` owns the envelope and the bounded field types;
`core/src/clawd/routes.rs` is the single table that ties a wire command name to
its typed body, access class, mutation kind, concurrency and time budget, safe
audit fields, authorization descriptor, and handler. It also drives stable
unknown, malformed, unauthorized, unavailable and execution-failure responses;
`core/src/clawd/transport/` owns framing, per-message credentials and admission
control; `core/src/clawd/authority/` owns the grants every privileged route is
authorized against.

Identity comes from the credentials Linux stamps onto each message when
`SO_PASSCRED` is set on the listener — not from `SO_PEERCRED` captured at
connect, and never from a request field. A peer that attaches descriptors has
them closed and its request refused. One request is served per connection, so
responses cannot be crossed and a replayed frame cannot chain a second
privileged call behind an authenticated one.

### Capability authority

Authority lives in one place and is held rather than described.
`core/src/clawd/authority/` issues **grants**: daemon-owned records that bind an
authenticated principal (uid, pid, that pid's start time, cgroup), a subject
(session, App, task), an audience, an exact `CapSet`, an issuer, issue and
expiry instants, a use budget, revocation state, and lineage.

```text
register / approve / delegate
  -> Authority::issue          (root grant, bound to the launcher process)
  -> opaque handle to the one legitimate holder
       |
       +-- Authority::attenuate  (child ⊆ parent in caps, audience,
       |                          expiry, use budget; depth/count bounded)
       |
       +-- authority::authorize  (per request: kernel credentials, audience,
                                  subject, capability spend) -> Decision
                                    -> handler re-checks through the Decision
```

Two things are deliberately *not* authority. A handle is not a bearer token:
possession is necessary and insufficient, because every resolve re-checks the
principal against the credentials the kernel stamped on that message, so a
same-uid sibling, an fd recipient, a recycled pid or a re-`exec`ed process
cannot use it. A session id is not authority either: it is an index into the
store, and naming somebody else's session fails exactly the way naming a session
that does not exist does.

The store is in memory and dies with the daemon, so ephemeral grants fail closed
across a restart. Scheduled work that must outlive one is re-issued from
root-owned durable provenance through `core/src/clawd/session_scope.rs` and the
narrow delegation policy in `core/src/clawd/system_caps.rs` — never from a
serialized handle or a promoted `CapSet`. Grants are bounded per owner, session
and process, and swept when a process exits, a session finishes or is cancelled,
a worker lease lapses, or a deadline passes.

`core/src/caps/` remains the vocabulary: verbs, scopes, catalog, manifests and
the `require` gate. It describes authority; the broker authority decides it.

### Agent ask/chat turn

```text
CLI / web UI / bridge
  -> clawd agent task client (for daemon-backed work)
  -> clawd claims the task, derives session capabilities, spawns claw-agentd
  -> claw-agentd (task owner, no supplementary groups, NoNewPrivs)
  -> runtime::loop_
  -> restore the session's versioned content-addressed system prompt,
     or build + freeze it once with the metadata-only Skill catalogue
  -> append due reminders / transient App data to the current request only
  -> load persisted conversation and compress when the configured budget requires it
  -> Provider::chat or Provider::chat_stream
  -> StreamEvent accumulation
  -> user-visible stream projection (tool identity only; evidence markers hidden)
  -> compact tool registry / guardrails / hooks
     (Apps default to cos_app_catalog + cos_app_run progressive disclosure)
  -> parallel-safe or serial tool execution
  -> tool results appended to conversation
  -> repeat until final response or max_turns
  -> final no-tools synthesis when the work limit is reached
  -> stream/progress/audit/result frames back to clawd over the job channel
  -> clawd persists usage/session/audit records and finishes the task
```

`core/src/agent/runtime/turn.rs` is the main seam where model output, tool
authorization, execution ordering, hooks, and conversation history meet.
The projection in `core/src/agent/runtime/presentation.rs` affects display
events only; complete tool inputs/results remain in the runtime trajectory,
session memory, audit records, and evidence verifier. Canonical prompt snapshots
live in content-addressed memory tables and are restored byte-for-byte across
continuations. Dynamic due/App context is logged separately as injected audit
data and never becomes user-authored history.

A daemon-backed task no longer runs inside root `clawd`. The broker claims the
task, derives its capabilities, and hands the work to a `claw-agentd` process
that starts as root only long enough to `exec`: `core/src/agentd/spawn.rs`
clears supplementary groups (including `sudo`) before dropping gid and uid to
the task owner, re-reads every id from the kernel, sets `PR_SET_NO_NEW_PRIVS`,
gives the runtime its own session and process group, applies a `0077` umask,
rebuilds the environment from an allowlist and closes every inherited
descriptor except a private `socketpair(2)` on fd 3. A task owned by root is
refused at `task.submit` and again before a worker could be forked: there is no
lesser account to drop to, so running one would put the model back in a root
process.

Because the worker leaves the `sudo` group, `/run/cos/clawd.sock` (`0660
root:sudo`) is unreachable from it, and the only authority it holds is the
grant in `core/src/agentd/grant.rs`: HMAC-signed with a per-broker-process key
and bound to owner uid, worker pid plus kernel start time, task and session id,
a lease deadline, and the route allowlist in `core/src/agentd/protocol.rs`. No
admin, App-session, scheduler or permission-decision route exists on that
channel. `SO_PEERCRED` is not used to authenticate it: the socket pair predates
the fork, so the kernel stamps it with the broker's own identity.

Consent still works. `core/src/caps/approval_gateway.rs` is the seam
`caps::require` consults instead of the root-owned approvals store: the worker
names the exact verb and canonical scope it was denied and nothing else, and
`clawd` supplies owner, session and task from the verified grant, spends an
exactly-matching approved grant one-shot or files a deduped pending request
under that identity. There is no route to decide a request or to obtain a
reusable capability.

The owner's baseline authority is still daemon policy rather than a consequence
of process context. `core/src/clawd/system_caps.rs` records one
explicit decision per catalog verb — risk is an input, not the rule — and the
default set is only what an owner-scoped conversation needs: the owner's own
path roots, its own memory and process-registry rows, read-only status of the
owner's own device, the owner-partitioned data stores, the model, and verbs that
carry no resource. Global filesystem access, arbitrary hosts and browser
navigation, process spawn/exec, credentials, system/package/service/identity/
storage/power mutation, cron persistence, device control, cross-user local
channels, and observation domains that describe another principal, another
account's units, or the machine's security posture are denied and require an
authenticated task/session delegation or an exact, one-shot approval settled at
the gate. Resource-addressing verbs never receive an untyped wildcard, and a
verb absent from the table is denied.

Because the runtime now executes as the owner, the per-owner agent state it
writes — conversation memory, notes, todos, AI budget counters and run records
under `<data>/users/<uid>/` — is owned by that account with mode `0700`, and
`<data>` plus `<data>/users` are `0711`: traversable, never listable. Every
other subtree stays `0700 root`. The residual boundary is honest and documented
in `core/src/agentd/MODULE.md`: a compromised worker holds the authority of the
account that submitted the task, and nothing beyond it. A task owned by root is
refused rather than run, so single-account container and WSL images must give
the agent its own unprivileged account.

Provenance decides which policy a trusted-session override is clamped by.
`SessionMeta::origin` is a typed marker written only by the daemon-side issuer
that already authorised the work, never copied from a request, and believed only
when the session record is root-owned — on a `0700` session directory that means
only `clawd` could have written it. An ambient task is clamped to the baseline.
A `clawd`-issued scheduler snapshot additionally keeps the one executor verb its
subsystem proved at creation (`cos cron` → `proc.spawn`, `cos triggers` →
`agent.spawn`) plus credentials named exactly, re-admitted verbatim from the
snapshot so nothing widens, and bounded by the same owner home the scheduler
applied when it stored the job. Everything else the creating session happened to
hold is dropped, so unattended work keeps running without persisting privileged
system authority. Owner homes for every one of these derivations come from
`paths::verified_home_for_uid`: canonical, existing, and owned by that uid, with
no fallback.

An approval is likewise a bounded decision rather than a licence.
`core/src/approvals.rs` stamps every approved record with a wall-clock deadline,
a use budget, a revocation generation and a keyed audit reference, and spends it
atomically under a store-wide lock, so `once` cannot be double-spent and
`session`/`forever` still expire. Revocation lives in
`core/src/approvals/generations.rs`: a monotonic counter in root-owned state
that a binding captures at approval time and every load compares against, so
retiring authority is an increment no restored backup can undo. `permission.revoke`
is the root-only route that performs it, and session finish, task cancellation
and worker-lease teardown call it for the session they tear down. A record with
no such provenance — one written before the binding existed — is evidence that a
decision happened, not authority: it authorises nothing until the owner is asked
again.

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
  -> metadata-only catalogue captured and recorded when a session freezes its
     canonical system prompt
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
Registration returns an opaque handle to a launch grant the capability authority
holds. The grant is bound to the launching process, expires, and authorises the
pid bind, per-call capability updates, and teardown for that one session; the
launched App never receives it. Binding derives a strictly narrower session
grant — launch authority dropped, bound to the App's own process tree — which is
what every privileged provider route the App later calls is authorized against.
Deregistration revokes the launch grant, and the session grant with it.

### Proactive scheduling

`cos cron` and `cos triggers` act on the root-owned job store the `clawd`
heartbeat drives, so a non-root CLI reaches it through the `scheduler.run`
broker route. The daemon validates the subsystem, command, arguments, and the
job/rule identifier before authorising anything, then resolves the caller's
authority the same way App launches do: from the peer's uid, pid, and process
start time, and from the nearest session `clawd` itself registered in the
root-owned routed registry. How the caller happens to be running — terminal,
`NoNewPrivs`, executable path — is never authority, and an App session may not
manage proactive jobs at all.

Listing, inspecting, or retiring rows the authenticated owner already owns is
answered with the single capability that subsystem's gate requires. Creating a
job, re-arming one, or running one now delegates authority that outlives the
call, so it must be covered by capabilities the peer holds or by one-shot grants
approved through the privileged approval helper and bound to that exact peer,
verb, and scope; the caller waits on `permission.status` and retries over the
same connection. A stored job never carries more than its creator could prove,
bounded by the same home-scoped ceiling the executor applies before it runs.

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
| `claw-agentd` | `core/src/bin/claw-agentd.rs` | Unprivileged agent worker, spawned per task by `clawd` |
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
- Broker protocol refusals carry only `&'static str` text and a stable class;
  refused frames and their ancillary data are never recorded, in any form.
- Desktop broker consumers use the unprivileged `crates/clawd-client` library
  for socket discovery, v1 framing, correlation, deadlines, bounds, and stable
  errors; the library depends on neither desktop UI nor privileged core logic.
- The desktop Agent UI and HTTP bridge compile against
  `desktop/agent/protocol`; the bridge translates clawd/core models into this
  versioned presentation contract and rejects incompatible versions explicitly.
- Desktop Ask Claw launchers and the Agent UI share the
  `cos_runtime::ask_claw` activation contract; host reducers never name the
  Agent UI executable or hand-build context JSON. The runtime directly spawns
  a transient UI with an inherited AF_UNIX socketpair and withholds the bounded payload until
  the child signals that Yama isolation, non-dumpable state, and private
  overlay mode are ready. The exact child is killed/reaped on startup failure
  and reaped after successful use. Public SDKs enter the same implementation
  through a packaged helper that authenticates its direct parent with
  `SO_PEERCRED` on an abstract Unix socket.
- Production resolves no executable input: the validated package target is
  fixed at `/usr/local/bin/cos-agent-ui`, and tests inject binaries only through
  a private compile-time test seam.
- `cos` and `clawd` speak one broker protocol version and are replaced
  together. A mismatched pair fails closed with a named protocol error; there
  is no permissive dual-stack listener.
- Memory and audit stores are append/transaction oriented; schema and recovery
  behavior require regression tests.
- Rootfs/image builds require Linux filesystem semantics and root privileges.
- Windows case-insensitive checkouts cannot faithfully represent every
  case-colliding desktop symlink.
- Public SDK wire changes regenerate every language binding and retain
  backwards-compatible serialization unless explicitly versioned.
- SDK response validation and first-party MCP JSON-RPC error codes are
  generated together from `claw-os-sdk/wire/v1/`.

## Related Design Documents

- [`docs/app-development.md`](docs/app-development.md)
- [`docs/image-architecture.md`](docs/image-architecture.md)
- [`docs/semantic-search-design.md`](docs/semantic-search-design.md)
- [`docs/browser-attached-design.md`](docs/browser-attached-design.md)
- [`packaging/README.md`](packaging/README.md)
- [`docs/updating.md`](docs/updating.md)
- [`desktop/README.md`](desktop/README.md)
