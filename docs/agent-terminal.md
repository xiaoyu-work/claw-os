# Agent terminal

The terminal frontend uses the pinned upstream Codex TUI, not a separate
lookalike renderer. Claw OS supplies its Agent backend through a private local
app-server protocol adapter. Provider calls, tools, capabilities, approval
decisions, tasks, and canonical conversation history remain Claw OS services.

## Launch

Run as the ordinary account that configured the Claw Agent:

```bash
cos agent chat
cos agent chat --session <session-id>
cos agent chat --no-memory --max-turns 8
cos agent chat -- --no-alt-screen
```

The TUI is selected only when stdin, stdout, and stderr are terminals and
`TERM` is not `dumb`. `--tui` explicitly requires that environment. Root cannot
submit an Agent task on behalf of another account.

The existing line interface remains available:

```bash
cos agent chat --plain
printf 'Explain the current task\n/quit\n' | cos agent chat
```

`--no-stream` and `--show-tools` retain their existing line-interface behavior.
They cannot be combined with `--tui`. Frontend arguments follow `--` and
cannot replace Claw's `--remote`, transport, or authentication settings.

The current task backend executes from the account's verified home directory.
The frontend's actual working directory and displayed workspace are aligned
with that directory; selecting another workspace requires a backend contract,
not an ignored `--cd` option.

A missing frontend binary is an actionable error, not permission to start a
Codex Agent or silently downgrade an explicitly requested TUI.

## Source and build

See the [terminal module](../terminal/README.md) for the pinned source,
reproducible build, upstream licensing, and frontend artifact. The frontend
has an isolated dependency graph; the core Cargo workspace does not acquire
the Codex execution engine as its Agent provider.

Build the core from the repository root:

```bash
cargo build -p cos
```

The launcher discovers the installed private frontend at
`/usr/lib/cos/tui/bin/codex-tui`, then the repository's
`build/agent-tui/bin/codex-tui` development artifact. An explicit
`COS_AGENT_TUI_BIN` override must be an absolute executable path. It is a local
operator setting, not a model-visible tool or a way to select another backend.
Before launching an installed frontend, `cos` reuses the release-security
runtime projection to measure the complete critical component set, including
the TUI binary. A replaced installed frontend is refused rather than trusted as
an owner task client.

## Backend and state ownership

```text
cos agent chat
  -> private upstream TUI child
  -> private app-server protocol adapter in cos
  -> authenticated clawd task/conversation/approval services
  -> claw-agentd and the existing guarded Agent runtime
```

The listener lives in a fresh owner-private temporary directory and has mode
`0600`. The frontend and adapter are supervised together. Losing the frontend
does not invent a successful task result or undo admitted effects. Explicit
interruption uses the normal task cancellation path.

The frontend receives a dedicated `CODEX_HOME` under the Claw user-data
directory's `terminal/` subdirectory. It holds frontend preferences and drafts,
not a second authoritative Agent conversation store. Claw provider credentials
are not copied into Codex configuration.

The launcher forces ephemeral frontend authentication, disabled upstream
updates/telemetry, and disabled Codex-native web search. It removes upstream
credentials and executor/telemetry routing variables only from the frontend
child, leaving Claw's backend configuration untouched. Conflicting frontend
overrides are errors. Claw's normal guarded web tools remain available.
Codex project configuration is explicitly untrusted: the Claw backend does not
load Codex folder hooks or accept a frontend trust write as execution authority.

The `agent.conversation.*` broker surface owns conversation creation and
presentation metadata. It uses actual Claw sessions and owner memory.
Branching conversation history does not clone capability grants. Changes to
the displayed transcript do not erase the audit trail or roll back filesystem
and other system effects.

Per-task model selection stays with the configured Claw provider:

```bash
cos agent service submit "Summarize this work" --model <model-id>
```

The requested model is persisted before queue publication and carried in the
worker assignment. It changes a per-request configuration clone, not the
owner's provider, credentials, policies, or another task's configuration.

## Presentation contract

Frontend code being present is not evidence that its backend operation is
supported. The [adapter module](../core/src/agent/tui_backend/MODULE.md)
documents the implemented protocol and its compatibility limits.

The adapter must preserve actual thread, turn, and item identities. A
provider response ending is not the end of an Agent turn when tool work
continues. A successful task is not proof that an Activity goal is complete.

Tool inputs and successful result bodies remain behind the shared
`runtime::presentation` projection. Rich operation previews and diffs require
explicit, safe backend data; they must not be reconstructed as authoritative
effects from model prose. Reasoning summaries may be presented, but encrypted
reasoning state, thought signatures, credentials, and raw internal payloads
must not enter the terminal.

Approval controls are presentations of existing protected requests. Neither a
frontend choice nor a protocol response grants permission independently of
Claw's approval authority.

## Terminal integration fixture

The Linux PTY fixture launches the real `cos` and pinned frontend against a
deterministic native broker fixture. It requires the task to be submitted,
render its result, and exit; the cancellation case sends the TUI's actual Esc
action and observes cancellation of the matching native task. No model
generation or installed daemon is used.

Run from the repository root after both binaries are built:

```bash
original_namespace="$(readlink /proc/self/ns/mnt)"
unshare --user --map-current-user --keep-caps --mount --net \
  python3 -B core/tests/agent_tui_pty.py \
  --cos target/debug/cos \
  --frontend build/agent-tui/bin/codex-tui \
  --case complete \
  --original-mount-namespace "$original_namespace"
```

Repeat with `--case cancel` and `--case plain`. The plain case deliberately
selects a nonexistent frontend binary and requires the legacy REPL to exit
without a broker task, covering the executable's global-flag routing too.
The fixture requires private mount and network
namespaces and overlays an empty home only inside that namespace, so it cannot
read, trust, or modify the account's real frontend configuration. It must not
be run as root. This is a frontend/adapter integration fixture, not proof of
all backend features or live provider behavior.
