# Worker Isolation Module

## Purpose

`core/src/worker/` is the single definition of how Claw OS runs code it
did not write. Python and polyglot App operations, GUI App surfaces,
App session servers, MCP servers, adapters and model-authored commands
are all launched through it, under the same enforceable namespace,
seccomp, cgroup, filesystem and network policy.

There is no second isolation implementation. `crate::sandbox`
(`cos_sandbox`), `crate::bridge` (Apps, GUI surfaces and App session
servers) and `crate::agent::tools::mcp::integration` (MCP servers and
adapters) are consumers of this module, not peers of it.

## Trust tiers

| Tier | Selected for | Sandbox | Display | Egress |
| --- | --- | --- | --- | --- |
| `AppOperation` | one manifest operation | yes | no | brokered, exact hosts |
| `DesktopSurface` | a manifest `desktop.exec` launch | yes | yes | brokered, exact hosts |
| `McpServer` | configured MCP servers, adapters, App session servers | yes | no | denied |
| `AgentExec` | `cos_sandbox exec` | yes | no | brokered, exact hosts |
| `TrustedDesktopSession` | the fixed vendor App session servers that need the session bus | yes | one exact socket | denied |
| `TrustedNativeHost` | the root-owned `mail-ai` native host | no | yes | host |

The tier is assigned by trusted code from how the worker is
*installed*, never from a manifest field. `TrustedNativeHost` is the
only exemption from the sandbox, it is reachable only through the
kernel-side checks in `bridge::run_native_app_host` (fixed App id,
root-owned package, root-owned interpreter, root-owned entry), and
taking it writes a `worker.sandbox.exempt` audit record.
`TrustedDesktopSession` is *not* an exemption — it is fully sandboxed —
and is reachable only through `trusted_desktop::classify`, which
requires a fixed App id, vendor provenance, a root-owned package tree
and a root-owned executed artifact.

## Responsibilities

- Define the typed launch policy and its audit projection (`policy.rs`).
- Derive a policy from authenticated manifest, operation, capability
  and runtime data (`derive.rs`).
- Enforce it on Linux with bubblewrap, seccomp and a resource governor
  (`provider.rs`, `linux.rs`, `seccomp.rs`, `cgroup.rs`).
- Serve the worker's only route to authority (`broker.rs`) and its only
  route to the network (`net_broker.rs`).
- Run the worker under bounded output, deadline and process-group
  cleanup (`exec.rs`), and record what was enforced (`audit.rs`).

## Key Files

| Path | Role |
| --- | --- |
| `policy.rs` | `LaunchPolicy`, tiers, mounts, limits, digest, audit facts |
| `derive.rs` | Trusted derivation from manifest/caps/runtime |
| `trusted_desktop.rs` | Fixed vendor table for App sessions that need the session bus |
| `migrate.rs` | One-time move of legacy App state into its partition |
| `provider.rs` | `WorkerSandbox` seam, availability, fail-closed `prepare` |
| `linux.rs` | bubblewrap argv, `pre_exec`, rlimits, identity |
| `seccomp.rs` | Hand-built classic-BPF syscall filter |
| `cgroup.rs` | cgroup v2 governor and `cgroup.kill` teardown |
| `broker.rs` | Per-launch narrow broker endpoint |
| `net_broker.rs` | Per-launch HTTP `CONNECT` egress broker |
| `exec.rs` | Bounded run, deadline, descendant cleanup |
| `runtime.rs` | Private per-launch runtime directory |
| `audit.rs` | Typed, path-free and secret-free launch records |

## Policy derivation

Mounts come from the capabilities the authority already granted:

- `fs.read` / `fs.meta` / `fs.watch` / `fs.exec` on a `Path` scope →
  read-only bind;
- `fs.write` / `fs.delete` on a `Path` scope → read-write bind;
- a `*` segment glob is **enumerated**: each matching entry is bound at
  its own depth, because `*` covers one segment and mounting the parent
  would hand over every grandchild the grant does not name. Symlinks,
  sockets, FIFOs and device nodes among the matches are skipped, and the
  expansion is bounded;
- a `**` scope covers a subtree and is bound as its literal prefix, but
  only after that subtree has been checked for a forbidden descendant —
  `$HOME/**` is refused rather than quietly exposing `~/.ssh`;
- a write grant may name an exact path or a `**` subtree; a `*` or
  partial glob is refused, because there is no single target to create
  into. A write grant for a file that does not exist yet mounts its
  parent;
- `Scope::Wild`, `/`-rooted scopes and deep or partial globs (`a/*/b`,
  `**/c`, `*.txt`) mount nothing, so a capability check can pass and the
  path still does not exist;
- a scope resolving into a kernel-owned root or a credential store
  (`/proc`, `/sys`, `/run/cos`, `/var/lib/cos`, `~/.ssh`, `~/.gnupg`, …)
  fails the launch instead of being silently skipped.

Granted paths are mounted at the *same absolute path* they have on the
host, so the argument the App receives, the scope the authority granted,
and the path inside the sandbox are one string.

An operation also gets a writable **App data partition**: `COS_DATA_DIR`
is `<owner-data-root>/apps/<app-id>`, created `0700` and bound at that
same path. The owner's data root itself — credentials, sessions, the
journal, every other App's partition — is never mounted, and the
read-only `_shared` library directories the bundled Apps import are the
only other part of the apps tree a launch receives.

State a bundled App wrote before isolation is brought forward once, by
`migrate.rs`, before its first sandboxed launch. The paths are a fixed
table keyed by App id — no manifest field and no argument selects them —
and the move is a `renameat` between two no-follow directory
descriptors rooted at the owner data root: same filesystem, atomic, no
copy. A symlinked, hardlinked, special or foreign-owned source is
refused, a populated destination is reported rather than merged, and the
version marker is written durably only after every entry has moved, so
an interrupted run is simply finished by the next launch. Two of the
table's directories are shared with the kernel — `proc/` holds the
session and capability registry and `apps/` holds every partition — so
those entries name individual files and leave everything beside them
where it is. A guard runs over the *effective source path* of every
entry, of either shape, and refuses a shared kernel root, a protected
file such as `proc/registry.json` with its lock and staging siblings,
or a prefix wide enough to reach one; it runs in the tests and again
before the first rename, so a future table edit fails the launch rather
than the review.

Two owner-private stores stay outside on purpose and are reached through
the broker instead of a bind:

- the cross-App **agent memory** database. `cos __memory` inside a
  sandbox forwards to the launch endpoint, which re-parses the call with
  the bridge's own typed argument handling and decides it from the
  launch's live capability set, so `memory.write` still has to be scoped
  `self:<source>`. A bind would have handed every other source's rows to
  hostile code;
- the session registry, which a worker has no route to at all.

Every bind source is then **pinned**: opened `O_PATH | O_NOFOLLOW`,
re-checked against the `st_dev`/`st_ino` validation saw, and handed to
bubblewrap as `/proc/self/fd/N`. A path swapped for a symlink, another
directory or a fresh mount between validation and setup fails the launch
rather than being followed.

## Networking

A worker has no route: its network namespace is empty and the seccomp
filter admits only `AF_UNIX` sockets, so `AF_INET`, `AF_INET6`,
`AF_NETLINK` and `AF_PACKET` cannot even be created. There is no proxy
environment variable to honour and no direct fallback to take. `AF_UNIX`
is allowed because a language runtime's event loop needs a self-pipe and
because it is how the SDK reaches the broker; the domain is checked in
the BPF program, on both words of the argument.

An operation granted exact hosts receives one Unix-domain socket at
`COS_EGRESS_SOCKET`. `cos_runtime.egress` is its only client: it issues
a bounded `CONNECT`, and the broker — outside the sandbox — matches the
request against the grant, resolves the name itself, refuses every
answer that is not globally routable, and connects to the address it
resolved. The caller runs TLS over the returned stream against the
hostname it asked for, so the broker pins the transport and TLS pins the
identity. A redirect is a new `CONNECT`, authorized from scratch.

Bundled consumers are migrated: `apps/_shared/safe_http.py`,
`apps/gateway/_shared/safe_egress.py`, the `calendar`, `search` and
`email` HTTP paths, and both SMTP senders (`apps/email`,
`apps/gateway/email`) via `cos_runtime.smtp`. Each uses the broker
inside a sandbox and its ordinary pinned dial outside one.
`netdiag`'s raw TCP probe reports the restriction explicitly rather than
returning a connection error that reads like the host being down.
`apps/test_no_direct_network.py` fails the build if a sandboxed bundled
operation grows a new direct dial.

## Failing closed

`prepare` refuses — never downgrades — when bubblewrap is missing or
older than 0.8 (no `--disable-userns`), unprivileged user namespaces
are disabled, seccomp is unavailable, no resource governor can be
established, the policy does not validate, or a mount source is a
symlink, socket, FIFO or device node. `cos` on a non-Linux host refuses
every hostile-worker launch outright.

Resource governance prefers a per-launch cgroup v2 scope, which gives
kernel-enforced memory/CPU/task ceilings and atomic `cgroup.kill`
teardown. Where the launcher has no delegated cgroup subtree
(containers, WSL, CI runners) it falls back to POSIX rlimits plus a
launcher-owned wall clock and process-group kill. Which one ran is
recorded in the launch facts; there is no third option where nothing
is enforced.

## Authority

A sandboxed worker never sees `/run/cos/clawd.sock`. It sees the
per-launch broker endpoint bind-mounted at that path, which:

- accepts only connections whose `SO_PEERCRED` uid is the worker's;
- answers `worker.policy.check` itself from the launch's **live**
  capability set — read from the routed registry row at call time, so a
  transient capability set for one MCP tool call appears and disappears
  with that call — which is what `cos __policy check`, and therefore
  `cos_runtime.policy`, needs inside the sandbox;
- refuses identity, consent, journal and scheduler routes outright;
- refuses every route with no admission rule, and prechecks the rest
  against a verb the launch actually holds;
- relays what is left through `clawd`'s `app_session.relay` route, one
  request per connection, with a bounded frame and explicit deadlines.

**The endpoint's checks authorize nothing.** They are a cheap early
refusal so an obviously unauthorized call costs no round trip. Every
relayed call is decided by `clawd`: the inner route's typed body is
decoded by the one route registry, the *live* App session grant is
resolved, the route's own authority decision is taken, and the owning
provider spends the exact capability before any effect happens.

The launcher can present that session grant because `clawd` issued it a
**relay grant** when the session was bound — no capabilities of its own,
`Process`-bound to the launcher, naming one session, in the `AppRelay`
audience alone, derived from the launch grant so `deregister` revokes it
with everything else. `app_session.relay` accepts only a
`Session`-subject `SystemService` inner route and refuses root, peer,
peer-session, handle-addressed, identity, consent, journal, scheduler
and recursive calls. The handle never enters the sandbox, never appears
in a response and never reaches an audit record; a same-uid sibling, a
process that received it over a socket, and the worker itself all fail
its `Process` binding.

## Dependencies

Depends on `crate::caps` for the capability vocabulary, `crate::clawd`
for the broker wire format, `crate::paths` for kernel-owned locations
and `crate::audit` for the decision log. Nothing in `worker/` depends
on a consumer.

## Tests

```bash
# Policy, derivation, argv, seccomp, brokers
cargo test -p cos worker:: -- --test-threads=1

# Adversarial tests against real processes, namespaces and mounts
cargo test -p cos --test worker_isolation -- --test-threads=1
```

The adversarial suite skips itself, loudly, on a host that cannot
enforce the policy; `missing_isolation_facilities_fail_closed` covers
what such a host must do instead.
