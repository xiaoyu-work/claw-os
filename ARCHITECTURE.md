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
| `clawd` broker | Versioned framed Unix-socket RPC, per-message peer identity, declarative route registry, mandatory capability-authority middleware, privileged dispatch, task ownership/lease, worker/extension supervision, and audit hook | `core/src/bin/clawd.rs`, `core/src/clawd/server.rs`, `core/src/clawd/transport/`, `core/src/clawd/routes.rs`, `core/src/clawd/authority/` |
| `claw-agentd` worker | Unprivileged per-task process that runs the model/tool loop after privilege drop; grant-authenticated private job channel | `core/src/bin/claw-agentd.rs`, `core/src/agentd/` |
| `claw-extension-host` | Per-task isolated-UID process that runs dynamic App/MCP code and signed Agent extension observers behind a worker-only control socket; only App/MCP children receive the route-filtered broker proxy | `core/src/bin/claw-extension-host.rs`, `core/src/extension_host/` |
| Agent extension ABI | Explicit authenticated-package registry, provider-attempt/tool observation FIFO, per-extension capability references, and default-deny exact-action mediation | `core/src/agent_extensions/`, `core/src/provenance/`, `core/src/extension_host/abi.rs` |
| Agent runtime | Multi-turn model/tool loop, prompt assembly, hooks, progress, compression, and tool dispatch | `core/src/agent/runtime/` |
| Model-input trust | Closed trust lattice, model-input source registry, labelled segments, and the bounded data fence for non-policy content | `core/src/agent/trust/` |
| LLM abstraction | Provider registry, wire adapters, streaming accumulation, fallback chain, credentials, and usage | `core/src/agent/llm/` |
| Tool/capability layer | Model-visible tool registry, guardrails, MCP attachment, scope checks, and approval boundaries | `core/src/agent/tools/`, `core/src/caps/` |
| Credential service | Validated credential identities, cryptography and master-key ownership, encrypted atomic persistence, authorization, refresh lifecycle, OAuth flows, and stable CLI facade | `core/src/credential/` |
| Memory and sessions | SQLite/FTS memory, semantic recall, session/message persistence, curation, and checkpoints | `core/src/agent/memory/`, `core/src/session/`, `core/src/checkpoint.rs` |
| Session event journal | Root-owned, MAC-chained record of session lifecycle and privileged mutation brackets; the ordering and recovery authority the other session/audit views project from | `core/src/session/journal/`, `core/src/clawd/journal.rs` |
| Audit | Hash-chained JSONL events and agent audit/query commands | `core/src/audit.rs`, `core/src/agent/audit_cli.rs` |
| Notification service | Durable owner-scoped user-attention records, delivery policy, DND, deduplication, retries, and channel leases | `core/src/notifications/`, `core/src/clawd/notifications.rs` |
| Apps and adapters | Declarative operation manifests plus Python, Node, shell, or binary runtime handlers | `apps/`, `adapters/`, `core/src/apps.rs`, `core/src/bridge.rs` |
| Extension provenance | Publisher signing, trust roots, package verification, and the shared bounded installer for Apps, Skills, MCP/adapter packages, and Agent extensions | `core/src/provenance/` |
| Update freshness | Signed release-security manifest, monotonic local security floor, one-use recovery authorizations, and the install/activation/runtime gates that refuse a superseded release | `core/src/update/`, `packaging/release-security/`, `packaging/deb/common/` |
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
- The tool registry caches only immutable descriptors. Every request projects
  them through a typed context built from authenticated session identity,
  effective capabilities, source/presence, execution host, reachable
  transports, enabled extensions, and guardrails.
- Exposure is discoverability, not authorization: dispatch repeats the same
  projection check and tools still validate arguments before exact capability
  enforcement.
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

### Model-input trust provenance

`core/src/agent/trust/` owns a closed provenance model for everything a model
can see. It is deliberately separate from chat role: providers expose only
`system`/`user`/`assistant`/`tool` channels, so role cannot distinguish operator
policy from a `MEMORY.md` note or a remote MCP server's tool description.

- `TrustClass` is an ordered lattice — `LegacyUnknown`,
  `UntrustedExternalContent`, `ModelGenerated`, `ExtensionMetadata`,
  `UserControlledContext`, `UserInstruction`, `SystemPolicy`.
- `SourceKind` is the closed registry of every way bytes reach a model request.
  One exhaustive `match` declares each source's class, persistence, provider
  projection, and audit strategy, so a new model-visible source cannot compile
  without declaring provenance. An unrecognised source is `LegacyUnknown`.
- `PromptProjection` splits one request into three channels. **Only
  `SystemPolicy` segments reach `system`/`developer`**: the compiled scaffold,
  plus an operator prompt file that ownership verification proved is root-owned
  and not owner-writable. The authenticated user instruction goes to `user`
  verbatim. Everything else — memory notes, `USER.md`, recalled memory, nudges,
  Skill metadata, tool metadata, external and model content, legacy rows —
  becomes separate bounded `user` data messages placed before the turn, in
  assembly order. A provider without a `developer` role may merge policy with
  policy; it can never merge policy with data, because the two never share a
  message.
- `LabeledSegment` carries the bytes, source and class together. Concatenation,
  summarisation, truncation and replay take the least-trusted class of their
  inputs, so trust never rises under transformation. Tool results are labelled
  from the tool's registered identity, which the registry fixes before the model
  call; tool *definitions* stay unfenced valid JSON schemas, bounded and
  marker-stripped at ingestion so no provider schema breaks.
- Labels are constructed only by trusted ingestion adapters naming a source.
  Any label recovered from bytes — a stored row, a serialized payload, an
  envelope header, a database column — is clamped to `UserControlledContext` or
  below, so no stored or model-authored content can deserialize itself into
  policy.
- `trust::envelope` fences each data segment. `encode` guarantees an encoded
  payload contains no `[[` digraph for *any* Unicode input — a fixpoint, not a
  single substitution pass — and `decode` inverts it exactly. `bytes=` is the
  emitted payload length, so a reader verifies the fence instead of trusting it.
- `trust::authority` states in the type system that a label is evidence, never a
  capability, role, approval or policy decision.

Threat statement: this is containment and provenance, not detection. A
malicious web page, MCP server, App or Skill can still persuade the model to
propose any text or any tool call. What labelling guarantees is that untrusted
bytes never enter the policy channel, cannot gain trust by being concatenated,
summarised, stored, replayed or re-serialised, and cannot forge or escape the
fence around themselves. The security boundary remains capabilities,
guardrails, approvals and the sandbox — none of which read a trust label.

### Persistence and observability

Anything inserted into a model request must be reconstructable from session or
audit records. Prompt injections, memory, tool calls/results, provider usage,
approvals, and privileged actions cannot bypass the recording path. The
owner-private message store keeps `trust_class`, `trust_source` and
`trust_lineage` beside each row; a database written before those columns existed
migrates by adding them as nullable, and a `NULL` — or a tampered value — reads
back as `LegacyUnknown`. The busy timeout is armed before the WAL switch and the
migration so the broker, worker and a CLI can open concurrently. Injected prompt
segments carry their `SourceKind` tag, and the Session Journal records a
content-addressed `ContentRef` plus the segment's provenance projected onto
`Origin`/`SegmentKind`/`Trust`; a fence recovered from stored bytes may refine
the recorded segment kind but never widens its trust.

Long `MemoryDb` conversations use durable compaction projections rather than
rewriting their authoritative transcript. Each per-session attempt records a
monotonic `started` then `completed`/`failed` lifecycle, the exact raw row IDs
and inclusive range plus SHA-256 digest, algorithm version, protected
tail/user IDs and row-identity digests, provider/model identity, frozen-prompt
hash/version, and recovery metadata.
Completed summary text is content-addressed. Continuations verify and load the
latest valid summary plus every uncompacted row; raw rows remain searchable and
exportable. A per-session advisory lock prevents duplicate concurrent
compaction, and reacquiring that lock closes a crash-left `started` attempt
before retry. Plans prepared before the lock carry their observed predecessor;
if another worker wins first, `AlreadyCovered` or `StalePlan` returns that
winner's verified summary and tail for adoption instead of treating the race
as corruption or restoring stale context. The runtime rechecks the complete
compression predicate after every adoption and uses a bounded wait/replan loop;
failure is explicit rather than sending known-over-threshold input. Oversized
old tool results are deterministically stubbed before an LLM summary.
Validation requires the protected tail to begin at the first uncompacted
replayable row, rejects tool-pair splits, and rechecks that a verbatim real-user
anchor and both protected row identities are unchanged.
Winner adoption deterministically merges live messages whose persistence
failed into positions anchored by neighboring raw row IDs. Ephemerals proven
outside keep their exact order, including structured tool results. An
ephemeral between rows collapsed into the winner cannot have been part of the
durable digest, so adoption rejects it rather than silently treating it as
summarized. Covered or otherwise ambiguous placement, orphaned tool pairs, or
a missing real-user anchor fails compression rather than dropping, duplicating,
or reordering active evidence.
Repair reroots a valid descendant around a damaged predecessor and records the
removed lineage; it drops the dependent chain only when no safe root remains.

Curated `MEMORY.md` facts are an append-only history, not a live inventory.
Before persistence, `core/src/agent/memory/ontology.rs` canonicalizes documented
aliases, classifies durable knowledge versus observed environment state, bounds
observation TTLs, and rejects session state or procedures. Each new fact records
validated source session/message provenance (or an explicit unknown/redacted
marker) and confidence. The curator performs its final reread, dedupe, and append
under one file lock. Prompt assembly leaves the human-editable file untouched
while projecting canonical chain tails, suppressing installation/version
contradictions by append order, and excluding expired observations; the complete
history remains inspectable through the memory tool.

Curated `MEMORY.md` facts are an append-only history, not a live inventory.
Before persistence, `core/src/agent/memory/ontology.rs` canonicalizes documented
aliases, classifies durable knowledge versus observed environment state, bounds
observation TTLs, and rejects session state or procedures. Each new fact records
validated source session/message provenance (or an explicit unknown/redacted
marker) and confidence. The curator performs its final reread, dedupe, and append
under one file lock. Prompt assembly leaves the human-editable file untouched
while projecting canonical chain tails, suppressing installation/version
contradictions by append order, and excluding expired observations; the complete
history remains inspectable through the memory tool.

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
- Authenticity and freshness are separate properties. Repository and package
  signatures prove the publisher; a monotonic, root-owned local security floor
  under `/var/lib/cos/security` proves the release is not superseded. Every
  package carries its own canonical signed release-security manifest,
  `preinst`, `prerm`, the APT pre-install hook and the binaries themselves all
  decide against that floor, and the floor survives package removal.
  Unprivileged processes enforce against a minimal root-owned projection in
  `/var/lib/cos-security` that the privileged commit publishes; the private
  tree is never widened. Local root and whole-state replacement remain outside
  its reach; this is not hardware anti-rollback.

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
  -> cli_catalog + cli_help definitions (help/schema)
     OR primitive module or clawd client (execution)
  -> capability/policy check
  -> structured JSON result
  -> requested output formatter
```

Hidden router bridges such as `__policy`, `__memory`, `__package`, and
`__systemd` are internal protocol surfaces used by bundled apps and services.

The same public command catalogue drives progressive model discovery:

```text
model
  -> cos_help with path=[]
  -> one public namespace
  -> one public command
  -> the named model tool
  -> normal guardrail, approval, capability, and audit path
```

`cos_help` is structural and read-only: its path contains command names, never
flags or operands, and it cannot dispatch a CLI operation or address hidden
`__*` routes. Command discovery therefore does not become a generic shell or a
way around per-tool policy. Installed Apps use the parallel
`cos_app_catalog`/`cos_app_run` path.

Token usage follows the same owner boundary as Agent execution. A model call to
`cos_usage` reads the current routed owner's log. A direct
`cos agent usage ...` client calls the typed `agent.usage` broker route; clawd
derives the owner UID from kernel peer credentials and selects that UID's log,
so neither path accepts a caller-supplied owner identifier.

### Broker request admission

```text
client (cos / bridge / rollback / approval helper)
  -> one length-prefixed v2 envelope in a CBK1 frame on /run/cos/clawd.sock
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
  -> composition snapshots Arc<CosConfig>, RegistryDeps, and RuntimeDeps
  -> runtime::loop_
  -> restore the session's frozen content-addressed *policy* prompt, or
     build + freeze it once (compiled scaffold, plus a root-owned
     operator policy file when ownership verification passes)
  -> rebuild the request prelude per turn: Skill catalogue, memory notes,
     owner-writable prompt file, due reminders, transient App data —
     each a separate fenced user data message before the owner's turn
  -> load persisted conversation and compress when the configured budget requires it
  -> Provider::chat or Provider::chat_stream
  -> StreamEvent accumulation
  -> user-visible stream projection (tool identity only; evidence markers hidden)
  -> session-stable tool projection:
       core tools stay direct
       App schemas become cos_tool_search / cos_tool_describe / cos_tool_call
       MCP descriptors remain behind fixed mcp_catalog / mcp_invoke tools
  -> compact tool registry / guardrails / hooks
  -> bridge envelopes resolve to the underlying tool before policy and scheduling
  -> parallel-safe or serial tool execution
  -> denied capability files an approval and releases the worker
  -> task waits durably in waiting_approval
  -> approval resumes the same task/session; denial ends it
  -> tool results appended to conversation
  -> repeat until final response or max_turns
  -> final no-tools synthesis when the work limit is reached
  -> stream/progress/audit/result frames back to clawd over the job channel
  -> clawd persists usage/session/audit records and finishes the task
```

The `clawd` task record snapshots broker-derived source and locality, but never
durable attendance. A short, in-memory presence lease binds an attended
submission to the authenticated client's uid, pid, process start time, and a
deadline. The lease is installed before the pending job becomes visible under
the same lock used to claim jobs, and failed publication removes it. It is
consumed when a worker is claimed and disappears on delay, client exit,
cancellation, recovery, or daemon restart. `clawd` signs that
lease, the client metadata, and a content-addressed capability generation into
the `claw-agentd` job grant; the worker verifies them before constructing
`ToolExposureContext` and rechecks presence when projecting or executing a tool.
Direct CLI, authenticated web and external MCP surfaces construct the same
context from their verified process session and trusted entry-point facts.
There is no process-global authorization or availability cache; concurrent
sessions project independently even when they share one descriptor registry.
The extension-schema budget is evaluated after that projection. Discovery and
bridge dispatch resolve the original registry entry again, so guardrails,
capabilities, approval, hooks, audit, timeouts, cancellation, and untrusted
result wrapping remain attached to the real tool identity.
Detached `proc spawn` children receive a derived `child-process` source and
never inherit the parent's attended bit; only the signed, expiring task
presence lease can cross a process boundary.

`core/src/agent/runtime/turn.rs` is the main seam where model output, tool
authorization, execution ordering, hooks, and conversation history meet.
Composition roots resolve environment-backed paths and open optional
memory/semantic stores once. `RegistryDeps` makes registry assembly
side-effect-free, while `RuntimeDeps` carries the scoped hook registry, clock,
semantic indexer, notes store, and prompt/audit paths into the unified
lifecycle. LLM composition follows the same snapshot rule:
`ProviderBuildContext` injects a `CredentialSource` and one shared
`HttpTransport` into `llm::registry`; the registry alone maps immutable
`AgentConfig` values into provider-specific settings. Credential-store then
environment precedence is resolved once before provider construction, and
credential pools receive only pre-resolved entries. Every concrete provider
slot is wrapped by an injected `ProviderAttemptObserver` beneath capability and
budget gates but above the provider invocation. It emits one paired id for each
real buffered or streaming attempt, including runtime retries and fallback
slots; terminal observation happens before tool dispatch and reports only
provider/model identity, model-only latency, usage, and a stable error class.
The live audit observer is assembled with the audit path and request/session
metadata outside the chain, retaining warn-only audit-write failure semantics.
Provider modules therefore own only authentication headers, wire
serialization/parsing/streaming, and upstream error classification, with no
config, environment, credential-store, or audit discovery.

Copilot's context-aware path keeps the same transport through GitHub-token
exchange, rejected-token refresh, live model-catalog negotiation, and the
final chat/Responses request. Process-backed constructors and auth functions
remain source-compatible legacy composition boundaries; production registry
and fallback assembly use only injected variants.

Deferred calls retain their provider-facing bridge envelope in the live
trajectory, while progress, hooks, evidence, and persisted/searchable history
use the resolved underlying tool identity. The provider tool array therefore
stays stable for prompt caching without weakening capability or approval
checks.

Standalone and `claw-agentd` audit hooks are installed into that
exact registry, and delegated children inherit it. App-session tools retain
their discovered App root; Skill roots retain their trust origin. Legacy
`config::get()` and static `with_override` callers remain source compatible,
while all production core code uses Arc-owned
`current_snapshot`/`with_snapshot`; a source inventory test enforces that
separation. Detached curation and web request composition reinstall the
captured snapshot before gated work. Detached curation also reinstalls a typed
`RoutedPathContext` containing the owner home, owner UID, and routed-job marker,
so budget, run-log, notes, credentials, and other path resolvers remain in the
owner partition after `tokio::spawn`. The curation log path itself is resolved
at composition and passed to `AutoCurator`, so its initial durable run bracket
never targets process-global state. Legacy direct-library agent adapters
retain compatibility contexts, but production CLI, web, and worker flows use
`runtime::loop_::run_with_deps`.
The projection in `core/src/agent/runtime/presentation.rs` affects display
events only; complete tool inputs/results remain in the runtime trajectory,
session memory, audit records, and evidence verifier. Canonical prompt snapshots
live in content-addressed memory tables and are restored byte-for-byte across
continuations. Dynamic due/App context is logged separately as injected audit
data and never becomes user-authored history.

The embedded Agent Web app uses this same queue rather than running an
in-process model loop. Chat submits and streams durable tasks, Tasks lists and
cancels those records and creates explicit retries, and Approvals can wake a
waiting task without requiring the user to repeat the request. The user-owned
Web process reads session lists and history from the owner-partitioned memory
database; decisions cross the existing `pkexec` approval helper rather than
granting the Web process direct decision authority. The Inbox is the
Notification Service projection; raw `context.event` records remain available
separately as System Events.

Closing or losing a Web SSE connection detaches the viewer but does not cancel
the durable task. The Chat Stop control uses the task id returned at submission
and calls `task.cancel` explicitly; completion remains observable through Tasks,
session history, and notifications after a reconnect.

Pre-queue Web conversations remain readable from their user-owned memory
database, while new durable conversations use the owner task partition. Root
`clawd` never opens the user-home compatibility database.

### Proactive notification

```text
cron / triggers / Agent task lifecycle / heartbeat / due nudges
  -> bounded NotificationDraft after the source transition is durable
  -> core notification service
  -> SQLite notification + change + per-channel delivery state
  -> owner-scoped clawd list / subscribe / acknowledge / dismiss routes
       -> authenticated Agent Web SSE + browser notifications
       -> user-session cos-agent-bridge -> Freedesktop notification D-Bus
  -> daemon delivery lease
       -> opt-in ntfy adapter
```

Notification delivery is deterministic system behavior and never depends on
the model deciding to call a notification tool. The service stores no full
prompt, tool arguments, credentials, or raw task result. `context.event`
remains an automation input and `system-operations.jsonl` remains immutable
audit evidence; notification read, acknowledgement, dismissal, retry, and DND
state live independently in `notifications.db`.

The system daemon never opens a user's session D-Bus. The user-session Agent
bridge claims desktop deliveries from `clawd`, posts them through
`org.freedesktop.Notifications`, and reports success or retryable failure.
External ntfy delivery is disabled by default and is enabled through
owner-scoped preferences.

A daemon-backed task no longer runs inside root `clawd`. The broker claims the
task, derives its capabilities, and hands the work to a `claw-agentd` process
that starts as root only long enough to `exec`: `core/src/agentd/spawn.rs`
clears supplementary groups (including `sudo`) before dropping uid to the task
owner and gid to the package-created `cos-extension` group, re-reads every id
from the kernel, sets `PR_SET_NO_NEW_PRIVS`,
gives the runtime its own session and process group, applies a `0077` umask,
rebuilds the environment from an allowlist and closes every inherited
descriptor except a private `socketpair(2)` on fd 3. A task owned by root is
refused at `task.submit` and again before a worker could be forked: there is no
lesser account to drop to, so running one would put the model back in a root
process.

Before every spawn, `clawd` pins the actual primary socket inode and every
canonical ancestor, verifies that neither the task uid nor `cos-extension` gid
can replace the path, then requires an actual post-drop `connect(2)` to fail
with `EACCES`/`EPERM` before `exec`. The worker therefore cannot reach
`/run/cos/clawd.sock` even when the task account's passwd primary group is the
broker's socket group. Its only authority is the grant in
`core/src/agentd/grant.rs`: HMAC-signed with a per-broker-process key and bound
to owner uid, isolated gid, worker pid plus kernel start time, task and session
id, a lease deadline, and the route allowlist in
`core/src/agentd/protocol.rs`. No
admin, App-session, scheduler or permission-decision route exists on that
channel. `SO_PEERCRED` is not used to authenticate it: the socket pair predates
the fork, so the kernel stamps it with the broker's own identity.

Consent still works. `core/src/caps/approval_gateway.rs` is the seam
`caps::require` consults instead of the root-owned approvals store: the worker
names the exact verb and canonical scope it was denied and nothing else, and
`clawd` supplies owner, session and task from the verified grant, spends an
exactly-matching approved grant one-shot or files a deduped pending request
under that identity. A filed request interrupts the current turn, moves the
task from `running/` to `waiting/`, and releases the worker lease. The
supervisor watches the durable decision: approval requeues the same task and
session with a reconstructable continuation prompt, while denial or a missing
request terminates it. The approval remains bound to the exact durable request,
task, owner, session, capability, scope, risk and operation digest; redemption
accepts a replacement worker only when that request appears in the requeued
task's `resumed_after_approval` set, then mints authority for the replacement's
fresh live PID/start-time/lease. Undecided requests expire after eight hours.
There is no worker route to decide a request or obtain a reusable capability.

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
authenticated task/session delegation or an exact, bounded approval settled at
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
`core/src/approvals.rs` binds every new record to the exact owner, session,
capability, catalog risk, and consent context, then stamps an approval with a
wall-clock deadline, use budget, revocation generation, and keyed audit
reference. Matching is exact rather than scope-covering, and spending is atomic
under a store-wide lock, so `once` cannot be double-spent and
`session`/`forever` still expire. Revocation lives in
`core/src/approvals/generations.rs`: a monotonic counter in root-owned state
that a binding captures at approval time and every load compares against, so
retiring authority is an increment no restored backup can undo. `permission.revoke`
is the root-only route that performs it; owner-wide revocation advances beyond
the highest per-session generation. Session finish, task cancellation and
worker-lease teardown revoke the session they tear down. A record with
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

OAuth flows depend on the credential store interface in
`core/src/credential/domain.rs`; they do not know file locations, lock files,
encryption keys, keyring syscalls, or persistent record encoding. The CLI
facade preserves command flags and output while coordinating typed credential
identifiers and store requests. Cryptography and master-key persistence are
below the store boundary and cannot be imported by OAuth or CLI callers.

### Progressive Agent Skill disclosure

```text
/usr/lib/cos/skills (read-only trusted vendor Skills)
  + per-user data/agent/skills (user-installed Skills)
  -> layered loader; built-in ids cannot be silently shadowed
  -> metadata-only catalogue captured and recorded when a session freezes its
     canonical system prompt
  -> cos_skill read: disclose one matching SKILL.md instruction body
  -> cos_help: disclose one level of the public CLI command tree
  -> cos_skill resource: disclose one explicitly requested child resource
  -> normal tool trajectory, session logging, and Skill usage record
```

Every Skill package is authenticated by `core/src/provenance/` before it is
loaded. Skills under the root-owned package root inherit vendor trust with a
pinned content digest; a `COS_SYSTEM_SKILLS_DIR` override is treated as local
content rather than promoted to built-in trust. User-installed Skills require a
valid signature from a trusted, non-revoked publisher key — there is no
environment variable that relaxes this — and layered shadowing compares the
verified publisher key id rather than directory precedence. User-installed
Skills still pass the non-vendor disclosure guard. Metadata pages, instruction
bodies, and child resources are size-bounded and read from the verified
snapshot: a file changed after the catalogue was built fails disclosure instead
of injecting new model text. Child resource reads accept only visible, regular
UTF-8 files beneath the selected Skill directory; absolute paths, parent
traversal, symlinks, hidden files, and oversized resources are rejected.

### Extension provenance

```text
publisher key -> claw.provenance/v1 envelope (kind, id, version, manifest
                 schema/path, entrypoints, resources, complete file tree,
                 content digest)
  -> trust roots: /usr/lib/cos/trust/publishers.d, /etc/cos/trust/publishers.d,
     ~/.config/cos/trust/publishers.d, ~/.config/cos/trust/developer.d
     (root/owner-owned, non-symlink, not group/world-writable; never
     environment-derived)
  -> install: bounded untrusted staging -> hostile-shape rejection -> signature
     and digest verification -> content-addressed artifact retention ->
     atomic activation
  -> use: verified snapshot bound to an open directory descriptor; manifest,
     executable, skill body/resources and MCP command all re-checked against
     their signed digests before launch or disclosure
```

Apps, Skills and MCP/adapter packages share one envelope format, one trust
store and one installer. An unverified manifest can never influence capability
grants or package identity: discovery quarantines the package with an
actionable diagnostic and every authority-bearing caller refuses it. Revoking a
key or an artifact digest moves the trust store's generation, which invalidates
cached verifications so later launches, disclosures and attachments stop
immediately. See [`docs/extension-provenance.md`](docs/extension-provenance.md).

### App invocation

```text
apps/<id>/app.json
  -> core app discovery and manifest validation
  -> operation schema / validated default binding / capability derivation
  -> app session registration
  -> hostile-worker launch policy derived from the granted capabilities
  -> declared Python / Node / shell / binary entrypoint with effective args,
     inside a namespace/seccomp/cgroup sandbox
  -> policy-enforced SDK/runtime calls through the per-launch broker endpoint
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

Inside a supervised task, the tool registry sends one-shot App operations and
stateful session calls over the extension-host control channel. The host
re-reads the installed manifest, launches the declared entrypoint, and uses its
private broker proxy for registration, binding, transient call scopes, and
teardown. Returned stdout, stderr-derived failures, MCP descriptors, and tool
results are bounded and treated as untrusted model data. A missing or crashed
host fails dynamic execution closed; there is no fallback that runs App code
inside `claw-agentd`.

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

### Worker isolation

Every process Claw OS did not write — an App operation, a GUI App
surface, an App session server, an MCP server, an adapter, a
model-authored command — runs under one shared launch policy defined in
`core/src/worker/`. The definition is typed and derived by trusted code
from authenticated manifest, operation and capability data; the Linux
provider enforces it with user/mount/PID/IPC/UTS/network namespaces, all
capabilities dropped, a seccomp filter, a resource governor, a read-only
root and an explicit mount list. There is no second, weaker launch path:
the App bridge, the App session bridge, the MCP attach path and the
agent sandbox tool are consumers of the same provider, and a host that
cannot enforce the policy refuses the launch instead of running the
worker unsandboxed.

Only a kernel-allowlisted, root-owned native host is exempt, and taking
that exemption is recorded. Read
[`core/src/worker/MODULE.md`](core/src/worker/MODULE.md) before
changing anything a worker can observe.

### MCP attachment

```text
config or discovered agent-API sidecar
  -> hostile-worker launch policy (network denied, no App data, no host paths)
  -> MCP transport/client initialization
  -> tools/list
  -> strict structural-schema sanitization + canonical descriptor-set digest
  -> remote catalogue returned only as wrapped untrusted data
  -> opaque owner/session/task/generation-bound handle
  -> fixed local mcp_catalog / mcp_invoke registration
  -> internal policy identity + shared registry exposure/guardrail/approval dispatch
  -> attachment liveness + generation recheck
  -> relist/digest verification
  -> hosted tools/call -> extension host -> bounded untrusted result
```

An MCP server is optional. Failure to attach one is logged and skipped rather
than preventing the core agent from starting. `ChatRequest.tools` contains no
remote identifier, description, or property name; those values exist only in
the wrapped `mcp_catalog` result. Structural descriptor drift, guessed handles,
reconnect replay, owner/session/generation mismatch, hidden exposure,
auto-deny, or missing approval blocks invocation.
Dropping an attachment handle marks every internal policy entry unavailable
and advances the shared catalogue generation before the child or hosted
transport is detached, so a previously disclosed handle cannot race teardown.
The registry's general `cos_tool_search` / `cos_tool_describe` /
`cos_tool_call` schema-budget path remains available to other extension
descriptors; MCP uses only its stricter opaque two-tool gateway and never
creates a parallel raw-tool execution path.
Every resolved hosted invocation emits correlated gateway and host lifecycle
records containing the internal policy identity, server, handle/descriptor
digests, capability generation, and signed binding/lease references. Remote
display text is represented only by an untrusted keyed digest.
Lifecycle outcome categories are carried on the versioned host protocol.
Transport connect/timeout, host crash, protocol, and remote-call failures are
assigned by trusted code paths; remote text never selects an audit action.

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
| `cos agent ...` | `core/src/agent/mod.rs`, `core/src/agent/*_commands.rs` | Top-level routing plus responsibility-specific command handlers |
| Agent loop | `core/src/agent/runtime/loop_.rs` | Multi-turn orchestration |
| One agent turn | `core/src/agent/runtime/turn.rs` | Provider call and tool execution |
| Notification service | `core/src/notifications/`, `core/src/clawd/notifications.rs` | Durable notification state, broker routes, and channel dispatch |
| App discovery | `core/src/apps.rs` | Provenance-gated `app.json` loading and schema generation |
| Extension provenance | `core/src/provenance/` | Package envelope, trust roots, verification, bounded install |
| Update downgrade protection | `core/src/update/`, `core/src/bin/claw-security-floor.rs` | Signed release manifest, security floor, recovery authorizations |
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
- Recoverable credential, provider-infrastructure, daemon-state, and command
  failures retain typed operation/source context until the CLI or broker wire
  boundary. The broker maps state corruption to the stable `unavailable` code;
  compatibility string APIs render only after typed ownership has ended.
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
- The session event journal is evidence, never authority: replaying any of its
  events creates no grant and no approval. Only root `clawd` appends, under a
  single writer lease and a monotonic epoch; a worker may request tool and turn
  lifecycle events only, and the event-type ACL is structural.
- Every `Kind::Mutation` broker route appends and fsyncs a `MutationStarted`
  record before any side effect. A start that cannot be recorded refuses the
  request; a completion that cannot be recorded is answered as `indeterminate`
  rather than as success or ordinary failure.
- A mutation whose outcome is unknown keeps refusing its own replay across
  restarts. Durable operation identity is owner uid plus canonical route plus
  the caller's operation key — never pid or process start time — and only the
  root-only `journal.mutation.resolve` route retires it.
- Journal read routes derive session ownership from the root-owned session
  record and require it to equal the authenticated caller uid before opening a
  partition. Request fields select a lookup, never an owner, and a foreign,
  missing or malformed session id returns one indistinguishable refusal with no
  read, alarm or quarantine side effect.
- Journal capacity is classed by event kind, not by writer: only records that
  retire, flag or recover a mutation may use the reserve, and that reserve is
  computed from the number of open brackets.
- Journal anti-rollback covers a local unprivileged attacker. Root or physical
  restoration of a consistent key + chain + anchor snapshot is out of scope
  until a TPM or remote anchor exists.
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
- [`docs/memory-recovery.md`](docs/memory-recovery.md)
- [`docs/extension-host-isolation.md`](docs/extension-host-isolation.md)
- [`docs/extension-abi.md`](docs/extension-abi.md)
- [`docs/extension-provenance.md`](docs/extension-provenance.md)
- [`docs/semantic-search-design.md`](docs/semantic-search-design.md)
- [`docs/browser-attached-design.md`](docs/browser-attached-design.md)
- [`packaging/README.md`](packaging/README.md)
- [`docs/updating.md`](docs/updating.md)
- [`desktop/README.md`](desktop/README.md)
