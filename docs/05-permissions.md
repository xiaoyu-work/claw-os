# 05 — Permissions: keeping agents on a leash

Claw OS is an agent-native OS. By design, an agent can ask the
kernel to read files, run programs, send email, dial the network,
and call other agents on your behalf. That convenience is only
safe if you decide *what* an agent is allowed to do — and the
system always knows, on your behalf, when an agent is about to
step outside that boundary.

This guide explains the permission system from a user's
perspective: what each capability means, what you see when you
grant or refuse one, and how to undo something an agent did.

> **Developer reading this?** See `docs/06-app-manifest.md`
> for how apps declare the capabilities they need and how
> enforcement is wired in code.

---

## How permissions work, in one paragraph

Every action that touches the world — reading a file, opening a
network connection, deleting a calendar event — belongs to a
**capability**. A capability is a *verb* (what the agent wants
to do, e.g. `fs.read`) plus a *scope* (what it wants to do it to,
e.g. `~/Documents/**`). When you start an agent you grant it a
set of capabilities. Every time the agent asks the kernel for
anything outside that set, the kernel refuses, tells you why, and
suggests how to fix it.

There is no implicit "I trust this agent with everything".
Wildcards exist, but you have to type them on purpose.

---

## The capability catalog

Capabilities are grouped into eleven domains. The label is what
shows up in the approval dialog and audit log; the verb is what
apps and the kernel use internally.

### Files (`fs.*`)

| Verb | Label | Risk |
|---|---|---|
| `fs.read` | View files | Low |
| `fs.write` | Modify files | Medium |
| `fs.delete` | Delete files | High |
| `fs.exec` | Run programs | High |
| `fs.watch` | Watch a folder for changes | Low |
| `fs.meta` | Read file info (size, dates) | Low |

Scope is always a path glob — `~/Documents/**`, `/etc/hosts`,
`*` (anywhere). Sensitive paths are highlighted in red:
`~/.ssh`, `~/.aws`, `~/.gnupg`, `/etc`, `/usr`, `/sys`, `/proc`,
`~/.config/git/credentials`, anything ending in `*.pem` /
`*.key` / `id_rsa`.

### Network (`net.*`)

| Verb | Label | Risk |
|---|---|---|
| `net.dial` | Access the network | Medium |
| `net.listen` | Open a port on this machine | High |
| `net.raw` | Send raw packets | High |
| `net.resolve` | Look up DNS names | Low |

Scope is `host[:port]`. `*.github.com:443` means "anything under
github.com on port 443". `*` means *the entire internet* and is
red.

### Processes (`proc.*`)

| Verb | Label | Risk |
|---|---|---|
| `proc.spawn` | Start a program | High |
| `proc.signal` | Send signals to a process | High |
| `proc.observe` | List / inspect processes | Low |

Scope is a session-id prefix — usually you'll see `self.*` or
`self.children.*`.

### System (`sys.*`)

| Verb | Label | Risk |
|---|---|---|
| `sys.service` | Start / stop a system service | High |
| `sys.package` | Install or remove software | Critical |
| `sys.mount` | Mount or unmount filesystems | Critical |
| `sys.time` | Change the system clock | High |
| `sys.power` | Shut down, reboot, suspend | High |
| `sys.kernel` | Load kernel modules | Critical |

These are almost always *admin* territory. The default agent role
does not get any of them.

### Secrets (`secret.*`)

| Verb | Label | Risk |
|---|---|---|
| `secret.read` | Use a saved credential | High |
| `secret.write` | Save / update a credential | High |
| `secret.grant` | Pass a credential to another agent | Critical |

Scope is a credential-name glob — e.g. `openai/*` matches every
key tagged with the `openai` namespace.

### Agents (`agent.*`)

| Verb | Label | Risk |
|---|---|---|
| `agent.spawn` | Create a child agent | Medium |
| `agent.invoke` | Call another agent or app | Medium |
| `agent.observe` | Watch an agent's progress | Low |
| `agent.delegate` | Pass capabilities to a child | High |

Scope is an agent or app name.

### Data (`data.*`)

| Verb | Label | Risk |
|---|---|---|
| `data.kv.read` / `data.kv.write` | Read / write key-value store | Low / Medium |
| `data.db.read` / `data.db.write` | Read / write SQLite databases | Low / Medium |
| `data.log.read` / `data.log.write` | Read / write structured logs | Low |
| `data.inbox.read` / `data.inbox.write` | Read / write the agent inbox | Low |

Scope is a key, table, or topic glob.

### IPC (`ipc.*`)

| Verb | Label | Risk |
|---|---|---|
| `ipc.publish` | Send a message on a topic | Low |
| `ipc.subscribe` | Listen for messages | Low |
| `ipc.invoke` | Make a service call | Medium |

Scope is a topic or service name.

### UI (`ui.*`)

| Verb | Label | Risk |
|---|---|---|
| `ui.notify` | Show a notification | Low |
| `ui.prompt` | Ask you a question | Low |
| `ui.window` | Open a window | Medium |
| `ui.input` | Capture keyboard / pointer | High |

These have no scope.

### Devices (`device.*`)

| Verb | Label | Risk |
|---|---|---|
| `device.audio` | Play sound | Low |
| `device.camera` | Use the camera | High |
| `device.microphone` | Use the microphone | High |
| `device.location` | Read your location | High |
| `device.sensor` | Read other sensors | Medium |
| `device.usb` | Talk to USB devices | High |

Scope is a device id or `*`.

### Time (`time.*`)

| Verb | Label | Risk |
|---|---|---|
| `time.cron` | Schedule recurring jobs | Medium |
| `time.delay` | Sleep / wait | Low |

These have no scope.

---

## Roles: pre-packaged bundles for common agents

Instead of granting capabilities one verb at a time, you can pick
a **role**. Each role is a curated bundle; you can still override
individual capabilities on top of it.

| Role | Good for | What it includes |
|---|---|---|
| 👀 **observer** | Read-only inspection | `fs.read`, `fs.meta`, `fs.watch`, `data.*.read`, `proc.observe`, `agent.observe`, `ui.notify` |
| 📝 **worker** | Editing files, taking notes | observer + `fs.write`, `data.*.write`, `ipc.publish`, `ui.prompt` |
| 🗂 **curator** | Tidying / archiving | worker + `fs.delete` |
| 🌐 **connector** | Research, fetching | observer + `net.dial`, `net.resolve`, `secret.read` |
| ⚙️ **automator** | Multi-step workflows | curator + `fs.exec`, `proc.spawn`, `net.dial`, `ipc.invoke`, `secret.read` |
| 🤖 **agent-host** | Orchestrating sub-agents | automator + `agent.spawn`, `agent.invoke`, `agent.delegate` |
| 🛡 **admin** | Full system control | agent-host + `sys.service`, `sys.package`, all `secret.*`, all `ui.*`, all `device.*` |

The default for `cos agent run` (when you don't pass `--role`)
is `worker`. Whatever the role grants, you still narrow the
*scope* — by default an agent only gets capabilities on
`$HOME/agent-workspace/<session>/**`.

---

## Risk badges

Every capability carries a risk level. The badge shows up next to
every line in the approval dialog and in audit logs:

- 🟢 **Low** — usually safe; we display these compactly.
- 🟡 **Medium** — worth a glance.
- 🟠 **High** — pay attention; we surface the icon prominently.
- 🔴 **Critical** — full-screen warning, single-line confirmation.

The overall risk of an approval request is the **maximum** of the
risks of every capability it asks for. One critical line makes
the whole prompt critical.

---

## The approval dialog

When an agent asks for something it doesn't already have, you'll
see:

```
🤖  <agent name>  wants to do these things:    🔴 critical

  🗑   Delete files       /tmp/cleanup/**         🟠 high
  🌐   Access network     *.github.com:443        🟡 medium
  🔑   Use a saved credential   openai/*           🟠 high
  🛠   Install software   curl                     🔴 critical

  Why:
    "Build the release artifact and upload it to S3."

  Grant for:   ◉ Just this once   ○ This conversation   ○ Forever

       [ Deny ]      [ Adjust scope ]      [ Allow ]
```

- **Deny** sends a structured refusal back to the agent. Good
  agents will explain what they were trying to do and offer a
  narrower alternative.
- **Adjust scope** lets you narrow paths, hosts, or credential
  names before approving. Useful when an agent over-asks
  (`/**` when `~/Downloads/**` is all you wanted).
- **Allow** grants the listed capabilities for the chosen
  duration. You can revoke them at any time from `cos perms`.

---

## The `cos perms` command

`cos perms` is your dashboard for everything permission-related.

```
cos perms list                       # what every agent currently has
cos perms show <agent>               # detailed capabilities + last-used
cos perms check <verb> [--scope]     # "would <verb> on <scope> be allowed?"
cos perms revoke <agent> [--cap ...] # revoke (everything, or just one)
cos perms audit [--days N]           # the last N days of granted actions
cos perms undo <session-id>          # undo an agent's file changes
```

The most-used subcommand is `cos perms check`, which prints a
JSON envelope saying whether your current session may exercise a
capability:

```bash
$ cos perms check fs.read --path ~/notes.md
{"decision":"allow","verb":"fs.read","scope":{"kind":"path","value":"~/notes.md"}}

$ cos perms check fs.delete --path /etc/passwd
{"decision":"deny","reason":"verb-not-granted","verb":"fs.delete",
 "requested_scope":{"kind":"path","value":"/etc/passwd"},
 "granted_scopes":[],
 "summary":"Permission denied (capability not granted): fs.delete on path:/etc/passwd"}
```

Apps shell out to this command to gate every operation that
touches the world; the JSON output is therefore part of a stable
contract — see `docs/06-app-manifest.md`.

---

## The two enforcement modes

The kernel has two modes, controlled by the `COS_PERMS_MODE`
environment variable.

- **`strict`** *(default)* — if there is no active session, or the
  session has no capabilities matching the request, the kernel
  refuses. This is the normal mode for everyday use.
- **`permissive`** — opt-in escape hatch. If no active session is
  set, any operation is allowed. Use this only for first-boot
  installer scripts that run before the session registry exists.
  **Never set this on a multi-user machine.**

You can flip into permissive mode for a single command:

```bash
COS_PERMS_MODE=permissive cos pkg need ripgrep
```

---

## The "after the fact" toolbox

### Undo a file change

Every `fs.write` and `fs.delete` first snapshots the affected file
into `$COS_DATA_DIR/trash/<session>/<timestamp>-<path-hash>/`.
The snapshot lives for 30 days.

```bash
cos perms undo <session-id>
```

reverses every snapshot in that session: writes are rolled back to
their previous content, deletes are restored. You can also dig
into the trash directly with `cos app fs ls
$COS_DATA_DIR/trash/<session>/`.

### Audit log

Every `caps::require` decision — allow or deny — is appended to a
structured audit log. `cos perms audit --days 7` is the easiest
way to read it; the raw file is JSON Lines under
`$COS_DATA_DIR/audit/perms.jsonl`.

Each line has:

```json
{
  "ts":         "2026-04-12T15:31:09Z",
  "session_id": "ag-3a91",
  "verb":       "fs.write",
  "scope":      {"kind":"path","value":"/tmp/report.pdf"},
  "decision":   "allow",
  "agent_name": "Build Helper",
  "caller_pid": 8421
}
```

---

## Language

The labels, hints, and dialog text above all flow through the
Claw OS i18n layer. The system language is set by the
`COS_LOCALE` environment variable (also exposed in the GUI's
settings). The first release ships English; additional locales
are added by adding a new variant to `Locale` and translating the
catalog strings — see `docs/06-app-manifest.md` for the
manifest-level mechanism.

---

## When in doubt

- Run with `--role observer` first; promote later only when the
  agent demonstrates it can't accomplish the task at lower
  privilege.
- Prefer **narrow scopes** over wildcards. `~/Documents/**` is
  almost always closer to what you mean than `~/**`.
- Read the audit log periodically: `cos perms audit --days 1`.
- If an agent asks for a Critical capability, slow down. Read the
  "Why" line; ask the agent to explain in your own conversation;
  consider whether a smaller-scope alternative exists.
