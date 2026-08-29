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
| `claw-extension-host` | Task-owned process that runs dynamic App/MCP code behind a worker-only control socket and a broker-owned route-filtered proxy | `core/src/bin/claw-extension-host.rs`, `core/src/extension_host/` |
| Agent runtime | Multi-turn model/tool loop, prompt assembly, hooks, progress, compression, and tool dispatch | `core/src/agent/runtime/` |
| LLM abstraction | Provider registry, wire adapters, streaming accumulation, fallback chain, credentials, and usage | `core/src/agent/llm/` |
| Tool/capability layer | Immutable tool descriptors, session-scoped model-visible projection, guardrails, MCP attachment, scope checks, and approval boundaries | `core/src/agent/tools/`, `core/src/caps/` |
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

### Persistence and observability

Anything inserted into a model request must be reconstructable from session or
audit records. Prompt injections, memory, tool calls/results, provider usage,
approvals, and privileged actions cannot bypass the recording path.

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
  -> claw-agentd (task uid, dedicated cos-extension gid, no supplementary groups, NoNewPrivs)
  -> clawd also spawns claw-extension-host for dynamic App/MCP processes
  -> runtime::loop_
  -> restore the session's versioned content-addressed system prompt,
     or build + freeze it once with the metadata-only Skill catalogue
  -> append due reminders / transient App data to the current request only
  -> load persisted conversation and compress when the configured budget requires it
  -> Provider::chat or Provider::chat_stream
  -> StreamEvent accumulation
  -> user-visible stream projection (tool identity only; evidence markers hidden)
  -> compact tool registry / guardrails / hooks
     -> rebuild session-scoped tool projection from trusted runtime facts
        (never request fields or process environment)
     (Apps default to cos_app_catalog + cos_app_run progressive disclosure)
  -> parallel-safe or serial tool execution
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
Detached `proc spawn` children receive a derived `child-process` source and
never inherit the parent's attended bit; only the signed, expiring task
presence lease can cross a process boundary.

`core/src/agent/runtime/turn.rs` is the main seam where model output, tool
authorization, execution ordering, hooks, and conversation history meet.
The projection in `core/src/agent/runtime/presentation.rs` affects display
events only; complete tool inputs/results remain in the runtime trajectory,
session memory, audit records, and evidence verifier. Canonical prompt snapshots
live in content-addressed memory tables and are restored byte-for-byte across
continuations. Dynamic due/App context is logged separately as injected audit
data and never becomes user-authored history.

Memory recovery treats `messages` as authoritative and FTS, titles, and
session prompt links as validated projections. `cos agent doctor` and
`cos agent sessions health` diagnose SQLite, WAL, schema, FTS, prompt
references, prompt hashes, titles, and repair lifecycle state without rewriting
the database: it inspects a private snapshot of the database/WAL/SHM family.
`cos agent sessions repair` takes an exclusive lifecycle lock shared by every
normal `MemoryDb` handle, brackets mutation in a private metadata-only repair
log, and rebuilds FTS transactionally. Damage that cannot be repaired without
trusting suspect prompt or SQLite bytes is renamed to a restrictive
same-filesystem quarantine before an attempt-bound staged replacement is
installed. A malformed WAL is never replayed during salvage; repair validates a
separate copy of the quarantined main database and retains its checkpointed
authoritative rows when possible. A valid WAL must checkpoint completely before
any rename, and staged replacements are accepted only after their attempt
marker and recovered counts are durable in the standalone main database.
Authoritative messages are scanned without secondary indexes and committed
before rebuilding indexes/FTS or attempting title and prompt projection
recovery.
See [`docs/memory-recovery.md`](docs/memory-recovery.md).

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

The adopted fd 3 is marked close-on-exec before the runtime starts, and its
bootstrap environment hints are removed. Process spawn independently marks
every descriptor above stderr close-on-exec and clears agentd/clawd supervision
hints from the child environment, preserving only the sealed executable memfd.
Agent-started descendants therefore cannot forge worker frames or collide with
the worker's approval traffic.

Dynamic App and MCP code is not loaded into either `clawd` or `claw-agentd`.
For every claimed task, the supervisor creates a private runtime directory,
binds a second broker socket there, and spawns `claw-extension-host` as the task
owner. The host repeats the worker's group drop, `NoNewPrivs`, `0077` umask,
environment/descriptor allowlists, separate session/process group, finite
rlimits, and non-dumpable process state. When the kernel permits it, IPC/UTS
namespaces and a cgroup add isolation plus memory/pid/CPU bounds; cleanup also
walks and kills the descendant tree so a child that called `setsid` cannot
escape.

The signed worker grant includes the extension protocol version, owner, task,
durable session, worker pid/start-time, host pid/start-time, random lease nonce,
deadline, and both socket paths. The host's control listener accepts frames
only from that exact worker identity. Its broker proxy uses per-message
`SCM_CREDENTIALS`: the host may reach only App/MCP lifecycle plus
`permission.status`, while descendants may reach only `Session` or
`PeerSession` routes for their nearest root-maintained App/MCP row. Task,
scheduler, permission-decision, admin, and sibling-session routes are absent.
Accepted provider calls re-enter the normal typed route registry, global
admission limits, capability authority, final provider checks, and audit path.

Consent remains inside the capability boundary.
`core/src/caps/approval_gateway.rs` is the seam `caps::require` consults instead
of the root-owned approvals store: after validated arguments produce an exact
verb and canonical scope, the worker names only those values. `clawd` supplies
owner, session, task, worker identity, attended/unattended context, and catalog
risk from trusted state. Attended denials may file one exact request;
unattended denials fail closed and must rely on authority delegated when the
automation was created. The request captures the worker pid/start time, a
broker-only lease nonce and deadline, and the revocation generation current at
request time; a stale decision, replacement worker, or concurrent task cannot
rebind it.

Approval correlation counters are not trusted as authenticators. Every worker
ask carries a fresh random nonce and the broker echoes that nonce plus the
complete ask. The worker resolves a waiter only when correlation id, nonce,
ask kind, verb, canonical scope, and operation digest all match; substituted
or replayed replies remain pending and cannot open a capability gate.

In-process Agent surfaces use the same binding model without a process-global
identity. Each invocation installs a Tokio task-local identity derived from its
actual task or conversation-turn identifier plus a fresh nonce. Invocation
completion, cancellation, and web-client disconnect durably revoke that
identity and retire its pending and approved records, so another conversation
or a later turn in the same conversation cannot redeem them.

An approved record is durable consent evidence, not ambient session authority.
At execution `clawd` atomically spends the exact record, then mints and
immediately exercises an in-memory `Issuer::Approval` capability grant bound to
the owner, session, task, worker pid/start time, verb, scope, approval expiry,
use budget, and revocation generation. Only then does the execution-time
`caps::require` return success. There is no worker route to decide a request or
obtain a reusable capability. See
[`docs/capability-consent.md`](docs/capability-consent.md).

Tool-name policy is not authority. `auto_deny_tools` remains a hard
pre-dispatch block, while `dangerous_tools` is retained for tools whose
complete command surface has not yet been mapped to exact capabilities.
`cos_proc` and `cos_sysinfo`, for example, stay on that compatibility path.
Process spawn also canonicalizes its executable before consent, requires both
`proc.spawn:self:children` and `fs.exec:path:<executable>`, and binds approval
matching across the worker protocol and authority redemption to a digest of
the validated executable, argv, working directory, and child-security
options. On Linux `core/src/proc.rs` opens the canonical executable without
following a final symlink, validates its owner and mode, snapshots and seals
its exact bytes in a memfd, and retains that descriptor through both
capability checks. The digest includes source file identity and content hash.
The cwd is also held by descriptor and selected with `fchdir`; execution uses
the snapshot's `/proc/self/fd` descriptor path, so pathname, inode, symlink,
in-place content, and cwd replacement races cannot change what runs.
The accepted image must be a static, fixed-address ELF for the host
architecture with no interpreter, dynamic segment, or executable stack.
It must additionally match the root-owned versioned manifest at
`/etc/cos/proc-spawn-allowlist.json` by canonical path and SHA-256. Each
manifest entry fixes the exact argv, explicitly identifies output-path
positions, and fixes a root-owned non-writable cwd. Non-output arguments cannot
name filesystem objects or contain paths, and the manifest hash is included in
the approval digest. Scripts, shells, language runtimes, unknown/renamed static
executables, mutable package or project directories, dynamically linked
executables, and file arguments are rejected before consent and routed to
`cos_sandbox`. Non-Linux process spawn fails closed rather than using a weaker
pathname-based fallback.

Allowlisted outputs are descriptor capabilities rather than path capabilities.
`openat2` pins a root- or execution-user-owned, non-group/world-writable parent
with `RESOLVE_BENEATH | RESOLVE_NO_SYMLINKS`, then exclusively creates the
output with `O_NOFOLLOW` or pins an explicitly permitted existing single-link
regular file. FIFO, socket, device, directory, symlink, and attacker-writable
parents are rejected. Parent/output identities and descriptor roles are bound
into consent, and the child receives only `/proc/self/fd/<fd>` for each output.
`cos_sysinfo` additionally requires
`secret.read:name:environment` before honoring `env --include-secrets`.

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

### MCP attachment

```text
config or discovered agent-API sidecar
  -> direct process: MCP transport/client initialization
  -> supervised task: claw-extension-host attach/ready
  -> tools/list
  -> prefixed local or host-backed proxy registration
  -> normal tool registry + guardrail dispatch
  -> hosted tools/call -> extension host -> bounded untrusted result
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
| `claw-extension-host` | `core/src/bin/claw-extension-host.rs` | Task-owned dynamic App/MCP host, spawned per task by `clawd` |
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
- `cos` and `clawd` speak one broker protocol version and are replaced
  together. A mismatched pair fails closed with a named protocol error; there
  is no permissive dual-stack listener.
- Memory and audit stores are append/transaction oriented; schema and recovery
  behavior require regression tests.
- Memory repair never deletes quarantined evidence, never restores a prompt
  blob without verifying its SHA-256 address, and never replaces a database
  while a cooperating reader or writer still holds its lifecycle lock.
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
- [`docs/semantic-search-design.md`](docs/semantic-search-design.md)
- [`docs/browser-attached-design.md`](docs/browser-attached-design.md)
- [`packaging/README.md`](packaging/README.md)
- [`docs/updating.md`](docs/updating.md)
- [`desktop/README.md`](desktop/README.md)
