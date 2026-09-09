# `cos-runtime` — internal OS runtime for claw-os bundled apps

> **Not a developer SDK.** Third-party Linux apps written for claw-os do not
> need this crate / package. If you only want to **call** the system LLM or
> expose tools to the agent, use [`claw-os-sdk`](../claw-os-sdk/) instead.

This directory holds OS-owned helpers for bundled clients, including clients
whose complete source now lives in `clawos-app`. The split exists because the
public SDK is independently published, whereas this runtime is distributed
only with the OS and supported only for its bundled/trusted clients:

| Module (Rust) | Module (Python) | Purpose |
|---|---|---|
| `cos_runtime::policy` | `cos_runtime.policy` | Shell out to the hidden policy bridge for self-gating capability enforcement |
| (not applicable) | `cos_runtime.memory` | Bounded wire-v1 requests to OS-owned memory with exact source-scope authorization; no App-local memory provider |
| `cos_runtime::fs` | (not applicable) | Route every `fs.*` op through `cos app fs <verb>` so audit / snapshots / caps apply |
| `cos_runtime::exec` | (not applicable) | Route every `exec.*` op through `cos app exec <verb>` |
| `cos_runtime::pkg` | (not applicable) | Route `pkg.*` ops similarly |
| `cos_runtime::notify` | (not applicable) | Route `notify.*` ops similarly |
| `cos_runtime::net` | (not applicable) | Route `net.*` ops similarly |
| `cos_runtime::ask_claw` | (not applicable) | Serialize bounded typed desktop context and directly supervise a transient Agent overlay |
| (not applicable) | `cos_runtime.snapshot` | Copy-on-write before every gated fs mutation |
| (not applicable) | `cos_runtime.browser_bridge` | Send attached-browser actions to the daemon-owned typed provider over a bounded private stdin bridge |
| (not applicable) | `cos_runtime.network_diagnostics` | Send host-network inspection and bounded probe requests to the daemon-owned typed provider |

These modules talk wire-v1 to OS authority. Their consumers are bundled
clients, not arbitrary third-party apps; repository location does not confer
permission to copy providers or bypass the broker.

## Bundled-client contract

DB imports the existing Python `cos_runtime.policy.require` export for
exact-name or wildcard capability checks. It returns `None` on allow and
raises `PermissionDenied` or `PolicyUnavailable` on refusal or transport
failure. The helper uses the wire-v1 policy decision envelope; the inherited
OS session and process ancestry, not caller-supplied labels, determine
authority. The client must not turn these errors into allow.

That bundled export is a dependency boundary, not permission to import its
private helpers or core implementation. Refactors behind the export and wire
contract should not require DB changes; changing the export is an interface
change that must be coordinated and validated against its consumers. DB's
cross-repository dispatch fixtures use the public manifest/MCP stdio contract,
not the SDK's private request handlers.

Summarize consumes the existing `cos_runtime.memory.remember` export, alongside
policy and public SDK AI. `source="summarize"` requests its own
`memory.write:self:summarize` namespace; the OS authenticates the session and
checks that scope rather than trusting the label. Text is bounded to 32 KiB
by the memory contract. The client records only a bounded first-line note and
never mounts or implements the owner memory database. Denials raise
`PermissionDenied`, transport/invalid-response failures raise `MemoryUnavailable`,
and the App must not return success when the memory request failed.
This is the existing bundled transport, not a newly public third-party memory
SDK or permission to import private runtime/provider implementation.

The App repository currently selects these libraries by an exact platform
source revision. This ensures reproducibility, not an independently published
runtime package, general third-party compatibility promise, or complete
release decoupling. Development tooling still knows the exported source
directories, and installed delivery still uses the OS package. See
[`packaging/README.md`](../packaging/README.md) for that distribution boundary.

## Why a separate crate / package

- **`publish = false`** for the Rust crate; no `pyproject.toml` for the Python
  package. Neither shows up on crates.io or PyPI.
- The OS installs both packages to `/usr/lib/cos/python/cos_runtime/` and
  embeds the Rust crate via `path = ` in workspace deps. There is no scenario
  where someone installs `cos-runtime` separately.
- Importing `cos_runtime` from a non-claw-os process **will fail loudly**
  (`cos` binary not on PATH, no `COS_SESSION` env, etc.) — by design.

## Ask Claw desktop integration

Bundled desktop apps implement `cos_runtime::ask_claw::Context` on a narrow
app-local `Serialize` type, then call `ask_claw::launch`. The runtime inserts
the app identity, serializes with `serde_json`, rejects non-object/reserved or
larger-than-32-KiB contexts, wraps it in a typed activation, and sends it over
an inherited AF_UNIX socketpair directly to a transient Agent UI. The call runs on a
dedicated launcher/reaper thread. A readiness channel prevents the parent from
writing any payload until the child has configured private-overlay mode,
verified process isolation, and become non-dumpable. Startup and writes share
a five-second deadline; failures kill and reap the exact child. No context
content enters argv, D-Bus, audit records, a process registry, the environment,
or the filesystem.

Launcher-style surfaces that already own a user query call
`ask_claw::launch_query`; the same typed activation, bounds, background worker,
and private socket handoff apply. Reducers never construct an Agent command.

Production launches use only the packaged absolute
`/usr/local/bin/cos-agent-ui`. The runtime rejects missing, symlinked,
non-regular, non-executable, non-root-owned, or group/other-writable targets.
There is no environment or `PATH` override and no shell evaluation.

The Agent UI imports the same activation type and CLI parser from this module.
It reads the inherited socket only when `--context-socket --activation-fd` is explicitly present, enforces the
activation and context bounds, validates the typed activation and embedded
context, and closes the socket. Context-bearing overlays deliberately run as
independent transient instances rather than forwarding plaintext through the
unauthenticated well-known D-Bus name; context-free global shortcut activation
continues to use the single instance. Payload-bearing `--context` and `--query`
arguments are rejected; the inherited socket is the only supported private input
path, and combining context sources rejects the entire activation.

Public SDKs use the packaged `/usr/local/bin/cos-ask-claw-launcher`, which
publishes an abstract Unix endpoint and accepts only the captured direct parent
PID and UID verified with `SO_PEERCRED`. Anonymous handoff fails closed unless Linux Yama
`kernel.yama.ptrace_scope >= 2`. The host and Agent UI are marked non-dumpable;
the parent withholds bytes until the child confirms that hardening is complete.

Keep these typed adapters in each app's `claw_glue` module. Reducers should
only select the user intent and pass the already-visible page, query, path, or
terminal output fields; they must not build JSON or know the Agent UI command.
The Terminal adapter uses the runtime's encoded-size predicate to drop oldest
lines first and then truncate at a UTF-8 boundary, retaining `app`, `mode`,
`cwd`, and `truncated` metadata while still opening the overlay.

The Ask Claw path does not alter or depend on the generic `cos_runtime::exec`
start/stop contract.

## Relationship to `claw-os-sdk`

```
┌─────────────────────────────────────────┐
│  claw-os-sdk     (public, published)    │
│  ─ ai · tools · serve · generated       │
└──────────────▲──────────────────────────┘
               │   uses Envelope, generated types
┌──────────────┴──────────────────────────┐
│  cos-runtime    (internal, OS-bundled)  │
│  ─ policy · snapshot · fs · exec · …    │
└─────────────────────────────────────────┘
```

`cos-runtime` depends on `claw-os-sdk` for the typed wire envelope, never the
other way around.
