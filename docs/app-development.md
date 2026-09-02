# Writing Your First Claw OS App

End-to-end walkthrough for app authors: scaffold → run → install. The
SDK is pre-installed on every Claw OS system, the `cos` CLI discovers
your code automatically, and every invocation re-reads your source from
disk — so the dev loop is `$EDITOR` ➜ `cos app …` and nothing more.

For the architectural background see
[`app-ai-integration.md`](app-ai-integration.md) (AI gate, manifest,
audit) and [`app-ai-tool-catalog.md`](app-ai-tool-catalog.md) (catalog
of agent-callable tools). This document is the **how to** counterpart.

> **MCP-first migration:** New App contracts use `schema_version: 2` and one
> top-level `mcp` service containing lifecycle, caller access, and tools.
> Legacy `operations` and `session` remain accepted only while bundled Apps
> are migrated; the final migration removes both duplicate surfaces.

## 1. What is a Claw OS app?

A Claw OS app is a directory whose minimum contents are a manifest and an
entry point. Apps may also include a stateful session server, desktop surface,
dependencies, tests, and arbitrary assets:

```
my-app/
├── app.json        ← required manifest
├── main.py         ← default Python entry point
├── server.py       ← optional stateful session-tool server
└── assets/         ← optional app-owned files
```

The manifest's `runtime` selects how the entry point is launched. Current
values are `python`, `node`, `shell`, and `binary`; the default is `python`.

The `cos` CLI recursively scans `$COS_APPS_DIR` (default
`/usr/lib/cos/apps/`) for valid `app.json` files, then exposes each operation
as `cos app <id> <op>` ([`core/src/apps.rs`](../core/src/apps.rs)).

The manifest id must equal the app directory's **normalized path relative to
`$COS_APPS_DIR`**, with path segments joined by `-`. For example,
`gateway/discord/app.json` declares `id: "gateway-discord"`. An app whose
path and id disagree is skipped during discovery.

The id itself has to match `[a-z][a-z0-9_-]*`
([`manifest.rs:935`](../core/src/caps/manifest.rs)) — start with a
lowercase letter, then lowercase letters, digits, `_`, or `-`.

## 2. The SDK is already there

On either a Debian/Ubuntu Agent install or a complete Claw OS target, the
`claw-os-agent.deb` package puts both Python helper packages on the system at
`/usr/lib/cos/python/`:

* `claw_os_sdk` — public SDK for AI calls
  ([`claw-os-sdk/python/src/claw_os_sdk/`](../claw-os-sdk/python/src/claw_os_sdk/))
* `cos_runtime` — internal runtime, used by bundled apps for capability
  gating ([`cos-runtime/python/src/cos_runtime/`](../cos-runtime/python/src/cos_runtime/))

The bridge prepends those directories to `sys.path` before exec'ing
your `main.py`
([`core/src/bridge.rs:31-60`](../core/src/bridge.rs)), so plain
`import` statements just work — no `pip install` necessary.

Off-system (writing code on a non-Claw-OS machine) you can either:

* set `COS_SDK_PYTHON_DIR=/path/to/dir` and arrange both
  `claw_os_sdk/` and `cos_runtime/` as subdirectories of that
  dir, or
* `pip install -e claw-os-sdk/python/` (the public SDK does ship a
  `pyproject.toml`). `cos_runtime` is internal and not pip-publishable;
  use the env var path for it.

## 3. The minimum viable app

Manifest (anything not shown gets a sensible default — `runtime`
defaults to `python`):

```jsonc
{
  "id": "hello",
  "version": "0.1.0",
  "name": { "en": "Hello" },
  "runtime": "python",
  "operations": {
    "say": {
      "label": { "en": "Say something" }
    }
  }
}
```

Only `id`, `version`, and `name.en` are mandatory
([`manifest.rs:613-621`](../core/src/caps/manifest.rs)). Every
`name`/`label`/`summary`/`why` field is a localised map; English is
required, other locales (`zh-CN`, …) are optional fallbacks
([`i18n/text.rs:42-90`](../core/src/i18n/text.rs)).

Entry point:

```python
# main.py
def run(command, args):
    return {"command": command, "args": args, "msg": "hello"}
```

The bridge calls `mod.run(command, args)` exactly once per invocation
([`bridge.rs:64`](../core/src/bridge.rs)). Two crucial details:

* `command` is a **string** — the manifest-declared operation name, such as
  `"say"`. The kernel rejects undeclared operations before starting the app.
* `args` is a **list of strings**, not a dict — the bridge validates every
  declared positional and `--flag` value, rejects undeclared flags, and then
  passes the effective argv (including manifest defaults) to the handler. Apps
  parse the already validated values; see
  [`apps/notify/main.py:46-99`](../apps/notify/main.py) for the
  conventional positional-vs-flag style.

Each argument declares a value `kind`: `path`, `host`, `name`, `text`,
`number`, `integer`, or `bool`. `number` accepts decimal values while
`integer` rejects fractional input. `binding` is either `positional` or
`flag`. When omitted, booleans retain the historical `flag` behavior and every
other kind remains positional:

```jsonc
{
  "name": "timeout",
  "kind": "integer",
  "binding": "flag",
  "required": false,
  "default": 30
}
```

Use `--` to end flag parsing when a positional value itself begins with `--`.
Any supplied number, integer, or explicit boolean literal that does not match
its declared kind is rejected before launch. Missing required booleans are
rejected before optional booleans are materialized as `false`. A destructive
confirmation uses `"required": true, "choices": [true]`, so omission and
`--confirm=false` both fail before capability resolution.
For a mode-specific confirmation, use `required_when` with the same
`arg-present`, `arg-equals`, or `arg-not-equals` condition model as capability
needs. The argument is accepted exactly when the condition applies and is then
required. The condition must reference an earlier argument; conditionally
required arguments cannot be repeatable or also declare `required: true`,
defaults, or trusted resolvers. A conditional confirmation still declares
`choices: [true]`.

Use `choices` for a closed scalar enum. Set `repeatable: true` when every
occurrence is meaningful: the bound value becomes an ordered JSON array and a
flag is emitted once per item. Repeatable positional arguments must be the
last positional declaration. Repeatable booleans, derived defaults, and
trusted resolvers are rejected because their occurrence semantics would be
ambiguous.

Use `aliases` for explicit alternate option spellings, such as
`"aliases": ["-n"]` or a positional argument's compatibility
`"aliases": ["--output"]`. Every spelling binds the same effective value and
conflicting forms are rejected. `positional_alias: true` is reserved for an
optional flag that historically occupied a surplus leading positional slot;
the one-positional canonical form remains unambiguous and canonical argv emits
the flag form. Positional aliases cannot coexist with optional or repeatable
positional arguments.

An optional argument may declare a non-null literal `default` matching its
`kind`. Omit `default` when there is no default; explicit JSON `null` is
invalid. If its default depends on an earlier string argument, use
`default_from`:

```jsonc
{
  "name": "output",
  "kind": "path",
  "binding": "flag",
  "required": false,
  "default_from": {
    "arg": "url",
    "transform": "url-path-basename",
    "prefix": "~/",
    "fallback": "download"
  }
}
```

The supported transforms are `identity` and `url-path-basename`.
`url-path-basename` requires a text source, path destination, and safe
single-component fallback. `default_from` is limited to one-shot operations
and is rejected for session-tool arguments.
Defaulted arguments must be optional; defaulted positional arguments follow
all required positional arguments. Defaulted positionals cannot be mixed with
optional positional slots that have no default because argv cannot represent
those gaps consistently. The bridge resolves paths before capability
derivation and materializes defaults using their declared binding: positional
values remain positional, non-boolean flags become `--name value` (or
`--name=value` when the value starts with `--`), and a true boolean flag
becomes `--name`. An explicitly supplied false flag becomes `--name=false`;
only an omitted/default-false flag is omitted. Positional booleans are
serialized as `true` or `false`. Flag defaults are placed before an
end-of-options delimiter; positional defaults follow supplied positional
values. The handler must consume that canonical argv rather than recompute a
separate default.

Session tools receive a JSON object rather than argv. Before capability
resolution and MCP forwarding, the kernel inserts every declared literal
default and omitted boolean into that object, validates choices and repeatable
arrays, and normalizes path values. One shared effective-call resolver feeds
the in-process gate, daemon gate, and forwarded tool arguments.

The bundled email and calendar apps use the reserved `email-provider` and
`calendar-provider` trusted resolvers. Before capability derivation, the
trusted launcher selects a provider from credential metadata, materializes
`--provider <name>`, and the manifest grants only that provider's exact
credential and host scopes. Calendar falls back to `local` when neither remote
credential exists. The bundled ntfy gateway similarly materializes its
configured `NTFY_SERVER` before deriving the exact URL-host scope and falls
back to `https://ntfy.sh` only when no server is configured. Third-party apps
and session tools cannot use trusted resolvers.

An operation may set `stdin: true` to receive explicitly forwarded caller
input. The top-level CLI opts in with `--stdin`, for example
`printf data | cos app fs write /workspace/out.txt --stdin`. This switch is
recognized only in an App operation's pre-`--` option region, so command-owned
`--stdin` flags elsewhere remain untouched. The CLI streams at most 16 MiB
(configurable with `COS_APP_STDIN_MAX_BYTES`) and fails before launch on
overflow. The bridge never inherits or probes process stdin. Agent, session,
service, and ordinary CLI calls therefore keep child stdin closed. Python list
handlers use `apps/canonical_argv.py`;
argparse and gateway parsers consume the same inline flags and `--` delimiter
directly.

The return value (a dict, list, or scalar) is JSON-dumped to stdout.
Return `None` to print nothing.

### Agent-launched isolation

An App invoked from a daemon-backed Agent task does not run in root `clawd` or
inside the `claw-agentd` model/tool process. `clawd` starts one
`claw-extension-host` as the task owner, and the worker sends App lifecycle and
call requests to that host over a versioned task-bound control socket.

The host starts with no supplementary groups, `NoNewPrivs`, a `0077` umask,
sanitized environment and descriptors, finite resource limits, and its own
session/process group. App children receive a separate private broker proxy
path, never `/run/cos/clawd.sock`. The proxy accepts only routes for the
child's nearest registered App/MCP session, then re-enters the normal
capability authority and final provider checks. Session registration,
per-call transient scopes, approvals, and teardown therefore retain the same
manifest-derived policy as direct launches.

Do not depend on inherited file descriptors, arbitrary parent environment
variables, the broker socket pathname, or daemonized children. Calls and
frames are bounded; cancellation, timeout, host failure, and task completion
terminate the hosted process tree. App stdout/stderr and returned values are
treated as untrusted Agent input.

## 4. The dev loop — no rebuild, no restart

Every one-shot `cos app <id> <op>` invocation launches the app entry point
through the runtime selected by its manifest. Python apps run in a fresh
subprocess and are imported from disk for every call, so there is no module
cache or daemon to restart. **Save the file, re-run the command, see the
change.** This is true on-system too — edits under
`/usr/lib/cos/apps/<id>/` are picked up immediately.

The simplest workflow is to point the kernel at a directory you own:

```sh
mkdir -p ~/my-apps/hello
$EDITOR ~/my-apps/hello/app.json    # paste the manifest from §3
$EDITOR ~/my-apps/hello/main.py     # paste the run() from §3

export COS_APPS_DIR=~/my-apps
cos app                              # shows hello in the list
cos app hello say --foo bar          # runs it
$EDITOR ~/my-apps/hello/main.py      # change the return value
cos app hello say                    # new behaviour, no reinstall
```

Or work from a copy of a bundled app as a starting point:

```sh
cp -r /usr/lib/cos/apps/notify ~/my-apps/my-notify
# rename the id in app.json AND rename the directory to match
```

## 5. Capability gating

Any operation that touches a controlled resource — files, network,
secrets, AI, the user's screen — has to declare it in the manifest
**and** check it at runtime.

Declare it in `operations.<op>.needs[]`:

```jsonc
"send": {
  "label": { "en": "Send a notification" },
  "needs": [
    {
      "verb": "ui.notify",
      "scope": { "kind": "wild" },
      "why": { "en": "Display a notification on your screen." }
    }
  ]
}
```

`scope.kind` is one of (kebab-case;
[`manifest.rs:462`](../core/src/caps/manifest.rs)):

* `"wild"` — anything matches. Use for verbs without natural scope
  (`ui.notify`, `time.delay`).
* `"fixed"` — hard-code a scope: `{"kind": "fixed", "scope": {...}}`.
  Useful for ops that always touch the same resource.
* `"from-arg"` — late binding: `{"kind": "from-arg", "arg": "path"}`
  reads the named op argument and constructs the scope from it. Without a
  transform it works for `path` / `host` / `name`. The optional safe
  `transform` is `parent` for a path's containing directory or `url-host`
  for the exact host and effective port parsed from a text URL. `url-host`
  resolves HTTP and HTTPS defaults to ports 80 and 443, preserves explicit
  ports and bracketed IPv6, and rejects schemes without a known or explicit
  port. App-side URL checks must derive the identical scope; HTTP clients use
  `_shared.safe_http.host_scope`, including for every redirect hop. The shared
  `_shared/url_host_scope_vectors.json` corpus locks Rust and Python behavior
  for UTS-46 ignored/mapped/contextual/rejected input, IDNA/punycode domains,
  legacy IPv4 forms, IPv6 compression, and ports. Production images provide
  `idna >= 3.3, < 4`; missing or unsupported versions fail closed. Core's
  `url 2.5.8` parser materializes caller URLs as canonical ASCII effective
  arguments before capability derivation and child dispatch. Python therefore
  never reinterprets caller Unicode on the initial request; it applies UTS-46
  itself only when canonicalizing redirect locations before requesting their
  authority. Rust browser/JavaScript consumers use
  `obscura_net::effective_host_scope` so every initial and redirect policy
  check includes the same effective port and bracketed IPv6 form.
* `"from-arg-map"` — map explicit argument values to predefined scopes:
  `{"kind": "from-arg-map", "arg": "mode", "values": {...}}`.
* `"from-arg-or-wild"` — derive a scope from an argument normally, but use a
  wildcard when it equals `wild_when`:
  `{"kind": "from-arg-or-wild", "arg": "target", "wild_when": "all"}`.

Needs that apply only in one mode declare `when` explicitly:

```jsonc
{
  "verb": "fs.read",
  "scope": { "kind": "from-arg", "arg": "file" },
  "when": { "kind": "arg-present", "arg": "file" },
  "why": { "en": "Read the optional file when one was supplied." }
}
```

`arg-equals` gates provider/mode-specific fixed needs:
`{"kind":"arg-equals","arg":"provider","value":"google"}`. An inactive
condition omits only that declared need. Once active, missing arguments and
unmapped `from-arg-map` values remain errors. Binding a capability
unconditionally to an optional argument without a default or trusted resolver
is rejected at manifest load time. `arg-not-equals` provides the inverse
comparison when a safe fallback must omit authority, such as preventing a
private credential from being used with a public default endpoint.

Check it at runtime by importing the internal runtime:

```python
from cos_runtime import policy

def run(command, args):
    try:
        policy.require("ui.notify", wild=True)
    except policy.PermissionDenied as denied:
        return {"error": str(denied), "denial": denied.denial}
    except policy.PolicyUnavailable as exc:
        return {"error": f"capability check failed: {exc}"}
    ...
```

`policy.require(verb, *, path=..., host=..., name=..., self_ref=..., wild=False)`
uses the hidden kernel policy bridge and raises `PermissionDenied` on deny
([`cos-runtime/python/src/cos_runtime/policy.py:79-106`](../cos-runtime/python/src/cos_runtime/policy.py)).
Pass exactly one scope keyword (or `wild=True` for unscoped verbs).

Use `policy.check(...)` (same signature, returns the raw decision
envelope) when you want to surface "would-be-denied" without aborting.

### Agent-resumable authorization

A bundled App that needs Google or Microsoft user authorization should return
an `auth_required` error with a constrained Agent action:

```python
{
    "error": "Gmail authorization is required",
    "auth_required": True,
    "retryable": False,
    "setup": {
        "interactive_oauth_available": True,
        "agent_action": {
            "tool": "cos_oauth_login",
            "input": {"provider": "google"},
        },
        "login_command": "cos credential oauth-login google",
    },
}
```

Only `google` and `microsoft` are supported. In an attended local Agent session,
the system Agent starts the trusted browser flow in the default credential
namespace and retries the original operation after authorization. Keep the CLI
command as a human fallback, and never include client secrets, access tokens,
refresh tokens, authorization codes, or browser state in App output.

## 6. Manifest-derived schema and help

`cos app <id> --schema` and `cos app <id> <op> --schema` generate their
output **only from `app.json`**. Schema inspection never imports or executes
the app entry point, so listing an untrusted app remains side-effect free.

Describe every operation in the manifest:

```jsonc
"say": {
  "label": { "en": "Say something" },
  "summary": { "en": "Return the supplied message." },
  "args": [
    {
      "name": "message",
      "kind": "text",
      "required": true,
      "label": { "en": "Message to return." }
    }
  ]
}
```

The manifest is the only maintained operation and argument contract. The entry
point implements behavior for those declared operations; it must not define
`_schema()`, handle `__schema__`, or maintain a parallel parameter list.
Static app tests compare manifest operations with dispatcher branches, require
parser flags to use `binding: "flag"`, and compare argparse required/default/
integer/repeatable/choice behavior without importing entrypoints. Optional
positional arguments cannot precede required positionals, and repeatable
positionals must be last. Fixed path scopes use absolute paths or
`~/...`; `$HOME`, `$XDG_DATA_HOME`, and other environment placeholders are
rejected because capability matching does not expand them.

## 7. AI features

Any AI call must route through the kernel's AI gate — the linter
(§8) will refuse an app that imports `openai`, `anthropic`, or
`google.generativeai` directly. Use `claw_os_sdk.ai` instead:

```python
from claw_os_sdk import ai

def run(command, args):
    body = open(args[0]).read()
    resp = ai.chat(
        prompt=f"Summarise:\n{body}",
        origin="external-content",   # third-party text → strict pipeline
        max_units=2000,
    )
    return {"summary": resp.text, "usage": resp.usage.__dict__}
```

Three things this triggers:

1. The manifest **must** carry an `ai` block declaring budget,
   safety profile, origins, and tool allowlist
   ([`manifest.rs:142-193`](../core/src/caps/manifest.rs)).
2. Each op that calls AI must include the matching `ai.*` verb in
   `needs[]`.
3. The user must grant consent for the app's current AI-policy snapshot before
   any call is allowed: `cos app consent grant <id>`. Changing the app's
   budget, safety profile, origins, or tool allowlist makes the saved consent
   stale and requires a new grant.

For the full AI integration model — `origin`, safety profiles, tool
catalog, budget envelope, audit surface — read
[`app-ai-integration.md`](app-ai-integration.md).

## 8. Developer commands

The `cos` CLI ships with everything you need to introspect, validate,
and ship an app
([`core/src/router.rs:107-187`](../core/src/router.rs)).

| Command | Effect |
|---|---|
| `cos app` | List every discovered app under `$COS_APPS_DIR`. |
| `cos app <id>` | Show ops + version for one app. |
| `cos app <id> <op> [args…]` | Run an op. |
| `cos app <id> --schema` | Full manifest-derived schema for the app. |
| `cos app <id> <op> --schema` | Schema for one op. |
| `cos app lint [<id>]` | Refuse apps that import provider SDKs directly. Run on every app if no id given. |
| `cos app tool list [<id>]` | Show the session-tool surface this app exposes to the agent. |
| `cos app install <dir> [--force] [--no-consent] [--yes] [--dev-trust]` | Validate the manifest, verify the package's provenance envelope, copy into `$COS_APPS_DIR/<id>/`, and (unless `--no-consent`) walk through the AI consent prompt. Copying is skipped only when the source resolves to that exact destination path. `--dev-trust` is the only route that installs unsigned content and records a persistent, digest-bound developer decision. |
| `cos app consent list` | Which apps you have granted AI consent to. |
| `cos app consent show <id>` | Display the manifest's AI block. |
| `cos app consent grant <id> [--yes]` | Grant AI consent. |
| `cos app consent revoke <id>` | Revoke it. |
| `cos app consent path <id>` | Print the consent record path. |

## 9. Environment variables

Set explicitly only when overriding defaults
([`bridge.rs:28-53`](../core/src/bridge.rs),
[`router.rs:25`](../core/src/router.rs),
[`claw_os_sdk/ai.py:525`](../claw-os-sdk/python/src/claw_os_sdk/ai.py)).

| Variable | Purpose | Default |
|---|---|---|
| `COS_APPS_DIR` | Apps root the kernel scans. | `/usr/lib/cos/apps` |
| `COS_DATA_DIR` | Where the current process persists Claw OS data. Inside a sandboxed operation this is the app's own partition, `<data-root>/apps/<app-id>` — never the owner's data root. | `$XDG_DATA_HOME/cos`, normally `~/.local/share/cos`; clawd overrides this to `/var/lib/cos` |
| `COS_APPLICATIONS_DIR` | Where `cos app install` writes generated desktop launchers. | `/usr/share/applications` |
| `COS_SDK_PYTHON_DIR` | Force the SDK lookup to a specific dir. Must contain both `claw_os_sdk/` and `cos_runtime/` as subdirs. | unset → kernel probes `/usr/lib/cos/python` + sibling dev paths |
| `COS_APP_ID` | The id of the calling app. **Auto-set by the bridge** from `app.json`; do not override. | (auto) |
| `COS_BIN` | Path to the `cos` binary the SDK shells back to. | `cos` (from `$PATH`) |
| `COS_SESSION` | Session id for grouped multi-call audit. | unset |
| `COS_WORKER_SANDBOX` | Set to `1` inside every sandboxed operation. Read it to detect the sandbox; do not rely on it for security. | (auto) |
| `COS_EGRESS_SOCKET` | HTTP `CONNECT` proxy socket, present only when the operation was granted exact `net.dial` hosts. | unset |
| `COS_EGRESS_ENDPOINTS` | Comma-separated `host:port` list the egress broker admits. | unset |

Everything else is cleared. An operation starts from an empty
environment and receives only the variables above plus a fixed `PATH`,
`HOME`, `TMPDIR` and locale — never the launcher's environment, never a
provider key, never `COS_PROC_DATA_DIR`, `COS_CAPS_DATA_DIR`,
`SSH_AUTH_SOCK`, `DISPLAY`, `WAYLAND_DISPLAY` or a session bus address.

## 10. What an operation can reach

Operations run inside a namespace/seccomp/cgroup sandbox
([`core/src/worker/MODULE.md`](../core/src/worker/MODULE.md)). Plan for
this while writing the manifest:

* **Filesystem.** Only the app package (read-only), the app data
  directory (read-write), the `cos` binary and SDK trees (read-only),
  and the paths the operation's `needs[]` actually granted. A
  `fs.read` grant is a read-only mount, `fs.write` and `fs.delete` are
  read-write, and a `from-arg` scope is mounted at the same absolute
  path the argument named. A wildcard scope maps to no mount at all: the
  capability check passes and the path is still not there. Never assume
  `$HOME` is the user's home — inside the sandbox it is the app's own
  data directory.
* **App data.** `COS_DATA_DIR` is the app's own partition of the owner's
  data root, `<data-root>/apps/<app-id>`. Neither the root nor another
  app's partition is mounted, so two apps cannot read each other's
  files even though they run as the same user. State a bundled app
  wrote to the data root before isolation is moved into that partition
  once, automatically, before its first sandboxed run
  ([`docs/updating.md`](updating.md) covers the cases that stop and ask).
* **Agent memory.** `cos_runtime.memory` still writes into the owner's
  cross-app agent memory; the database is not mounted, and the call is
  carried out by the launcher after checking `memory.write` scoped
  `self:<your-app-id>`. Writing to another app's source is refused, and
  there is no file to open directly. `show` and `forget --row` answer
  for another app's row exactly as they answer for a row that does not
  exist, so the row id space says nothing about what other apps store.
* **Network.** Denied by default, in a private network namespace, and
  the worker seccomp filter allows only `AF_UNIX` sockets — an
  `AF_INET` socket cannot even be created, so there is no direct dial
  to fall back to. An operation that declares `net.dial` with an exact
  `host[:port]` scope receives a Unix-domain egress socket at
  `COS_EGRESS_SOCKET`; `cos_runtime.egress.create_connection()` opens a
  tunnel through it, and the broker admits only those endpoints, pins
  the address it resolved itself, and refuses loopback, link-local,
  cloud-metadata, private and CGNAT addresses. TLS is established by the
  caller over the returned stream, against the hostname it asked for. A
  wildcard host scope grants nothing, because there is no identity to
  check against.
* **Filesystem globs.** `*` matches one segment and is expanded entry
  by entry, so `Documents/*` exposes the entries in `Documents` and not
  their children. `**` covers a subtree and is refused outright when
  that subtree contains a credential store, so `$HOME/**` does not
  quietly hand over `~/.ssh`. A write scope must name an exact path or a
  `**` subtree.
* **Processes.** Bounded process count, open files, file size, memory,
  CPU, wall clock and captured output. A worker cannot daemonize: the
  whole process group and cgroup are killed when the operation ends, is
  cancelled or times out.
* **Authority.** `cos_runtime.policy` and every `cos` call the app makes
  go through a private broker endpoint bound to that one launch, which
  relays them to `clawd` under a launcher-held grant. `clawd` still
  decodes, authorizes and spends the exact capability against the live
  App session, so the endpoint can only narrow what the app may do. An
  app cannot register an identity, widen its own capabilities, answer
  its own consent prompts, or reach the real broker socket.
* **GUI.** Display and GPU transports are granted only to a
  `desktop.exec` launch, never to a headless operation.

## 11. Ship it

Once the app does what you want:

```sh
cos app lint ~/my-apps/hello         # static check
cos provenance sign --kind app --id hello --version 1.0.0 \
    --path ~/my-apps/hello --key ~/.secrets/my-publisher.json \
    --entrypoint main.py             # bind the whole tree to your key
cos app install ~/my-apps/hello      # verify provenance + copy into $COS_APPS_DIR
# If the manifest has an `ai` block, the installer prompts for consent
# (skip with --no-consent and run `cos app consent grant hello` later).
cos app hello say                    # now lives under /usr/lib/cos/apps/hello/
```

An App is authenticated before its manifest can influence capability
grants or App identity, so `cos app install` refuses an unsigned tree
unless the operator explicitly opts in:

```sh
cos app install ~/my-apps/hello --dev-trust
```

That records a developer grant bound to the installed content digest,
runs the App with a restricted ceiling and no privileged routes, and is
invalidated the moment the tree changes. Editing
`/usr/lib/cos/apps/<id>/main.py` in place therefore no longer
hot-reloads: the edited package stops verifying and is quarantined
until it is re-signed and re-installed (or the developer grant is
re-recorded). Iterate in your source tree and re-run `cos app install`.

For a clean distributable, sign the directory and package it as a
tarball: any machine that trusts your publisher key can
`cos app install <untarred-dir>` it. See
[`extension-provenance.md`](extension-provenance.md) for the envelope
format, trust roots, revocation and rollback.

## See also

* [`extension-provenance.md`](extension-provenance.md) — package
  signing, trust roots, install pipeline, revocation, rollback.
* [`app-ai-integration.md`](app-ai-integration.md) — AI gate, manifest
  reference, lifecycle, audit surface.
* [`app-ai-tool-catalog.md`](app-ai-tool-catalog.md) — every
  agent-callable tool with its verb and scope.
* [`browser-attached-design.md`](browser-attached-design.md) —
  worked example of a non-trivial app with a Chromium extension and a
  native-messaging bridge.
* [`apps/notify/`](../apps/notify/),
  [`apps/kv/`](../apps/kv/),
  [`apps/fs/`](../apps/fs/) — small bundled apps that double as
  reference templates.
