# Writing Your First Claw OS App

End-to-end walkthrough for app authors: scaffold → run → install. The
SDK is pre-installed on every Claw OS system, the `cos` CLI discovers
your code automatically, and every invocation re-reads your source from
disk — so the dev loop is `$EDITOR` ➜ `cos app …` and nothing more.

For the architectural background see
[`app-ai-integration.md`](app-ai-integration.md) (AI gate, manifest,
audit) and [`app-ai-tool-catalog.md`](app-ai-tool-catalog.md) (catalog
of agent-callable tools). This document is the **how to** counterpart.

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
its declared kind is rejected before launch.

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
all required positional arguments. The bridge resolves paths before capability
derivation and materializes defaults using their declared binding: positional
values remain positional, non-boolean flags become `--name value`, and a true
boolean flag becomes `--name`. The handler must consume that materialized value
rather than recompute a separate default.

Session tools receive a JSON object rather than argv. Before capability
resolution and MCP forwarding, the kernel inserts every declared literal
default into that object, including defaults unrelated to a capability scope.

The return value (a dict, list, or scalar) is JSON-dumped to stdout.
Return `None` to print nothing.

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
  reads the named op argument and constructs the scope from it. Only
  works for args of `kind` `path` / `host` / `name`; text args must
  use `"wild"` and the handler narrows the scope at runtime.
* `"from-arg-map"` — map explicit argument values to predefined scopes:
  `{"kind": "from-arg-map", "arg": "mode", "values": {...}}`.
* `"from-arg-or-wild"` — derive a scope from an argument normally, but use a
  wildcard when it equals `wild_when`:
  `{"kind": "from-arg-or-wild", "arg": "target", "wild_when": "all"}`.

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
integer behavior without importing entrypoints. Optional positional arguments
cannot precede required positionals. Fixed path scopes use absolute paths or
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
| `cos app install <dir> [--force] [--no-consent] [--yes]` | Validate the manifest, copy into `$COS_APPS_DIR/<id>/`, and (unless `--no-consent`) walk through the AI consent prompt. Copying is skipped only when the source resolves to that exact destination path. |
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
| `COS_DATA_DIR` | Where the current process persists Claw OS data. | `$XDG_DATA_HOME/cos`, normally `~/.local/share/cos`; clawd overrides this to `/var/lib/cos` |
| `COS_APPLICATIONS_DIR` | Where `cos app install` writes generated desktop launchers. | `/usr/share/applications` |
| `COS_SDK_PYTHON_DIR` | Force the SDK lookup to a specific dir. Must contain both `claw_os_sdk/` and `cos_runtime/` as subdirs. | unset → kernel probes `/usr/lib/cos/python` + sibling dev paths |
| `COS_APP_ID` | The id of the calling app. **Auto-set by the bridge** from `app.json`; do not override. | (auto) |
| `COS_BIN` | Path to the `cos` binary the SDK shells back to. | `cos` (from `$PATH`) |
| `COS_SESSION` | Session id for grouped multi-call audit. | unset |

## 10. Ship it

Once the app does what you want:

```sh
cos app lint ~/my-apps/hello         # static check
cos app install ~/my-apps/hello      # validate manifest + copy into $COS_APPS_DIR
# If the manifest has an `ai` block, the installer prompts for consent
# (skip with --no-consent and run `cos app consent grant hello` later).
cos app hello say                    # now lives under /usr/lib/cos/apps/hello/
```

Subsequent edits to `/usr/lib/cos/apps/<id>/main.py` are picked up
immediately — the same hot-reload story as §4. For a clean
distributable, package the directory as a tarball: any machine with
Claw OS can `cos app install <untarred-dir>` it.

## See also

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
