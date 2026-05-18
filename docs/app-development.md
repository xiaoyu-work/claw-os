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

A Claw OS app is a directory containing two files:

```
my-app/
├── app.json        ← manifest (id, ops, capability needs, AI policy)
└── main.py         ← Python entry point
```

The `cos` kernel CLI discovers every subdirectory of
`$COS_APPS_DIR` (default `/usr/lib/cos/apps/`) that has a valid
`app.json`, then exposes each op as `cos app <id> <op>`
([`core/src/apps.rs:29`](../core/src/apps.rs),
[`core/src/router.rs:25`](../core/src/router.rs)).

Hard rule: **the directory name must equal `manifest.id`**
([`apps.rs:61`](../core/src/apps.rs)). An app whose folder name and id
disagree is silently skipped during discovery.

The id itself has to match `[a-z][a-z0-9_-]*`
([`manifest.rs:935`](../core/src/caps/manifest.rs)) — start with a
lowercase letter, then lowercase letters, digits, `_`, or `-`.

## 2. The SDK is already there

On a Claw OS install (Docker, WSL, VM, or ISO target) the
`claw-os-base.deb` package puts both Python helper packages on the
system at `/usr/lib/cos/python/`:

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

* `command` is a **string** — the op name (`"say"` here, or the special
  `"__schema__"` covered in §6).
* `args` is a **list of strings**, not a dict — every CLI token after
  the op name (`["--foo", "bar"]` for `cos app hello say --foo bar`).
  Apps parse their own flags; see
  [`apps/notify/main.py:46-99`](../apps/notify/main.py) for the
  conventional positional-vs-flag style.

The return value (a dict, list, or scalar) is JSON-dumped to stdout.
Return `None` to print nothing.

## 4. The dev loop — no rebuild, no restart

Every `cos app <id> <op>` call spawns a fresh `python3` subprocess and
does `importlib.spec_from_file_location(...).loader.exec_module(...)`
([`bridge.rs:61-63`](../core/src/bridge.rs)), so there is no caching
and no daemon to restart. **Save the file, re-run the command, see
the change.** This is true on-system too — `/usr/lib/cos/apps/<id>/main.py`
edits are picked up immediately.

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

## 6. Optional `__schema__` for richer help

`cos app <id> --schema` and `cos app <id> <op> --schema` print the
manifest-derived schema by default, but they also call your app with
`command="__schema__"` and merge any returned `parameters` /
`example` fields into the output
([`router.rs:1679-1716`](../core/src/router.rs)).

Return a dict keyed by op name:

```python
def _schema():
    return {
        "say": {
            "description": "Say something",
            "parameters": [
                {"name": "message", "type": "string", "required": True,
                 "kind": "positional", "description": "Text to say"},
            ],
            "example": "cos app hello say 'hi there'",
        },
    }

def run(command, args):
    if command == "__schema__":
        return _schema()
    ...
```

See [`apps/notify/main.py:114-131`](../apps/notify/main.py) for the
canonical pattern.

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
3. The user must grant consent **once per app** before any call is
   allowed: `cos app consent grant <id>`.

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
| `cos app <id> --schema` | Full schema for the app (merges `__schema__` output). |
| `cos app <id> <op> --schema` | Schema for one op. |
| `cos app lint [<id>]` | Refuse apps that import provider SDKs directly. Run on every app if no id given. |
| `cos app tool list [<id>]` | Show the session-tool surface this app exposes to the agent. |
| `cos app install <dir> [--force] [--no-consent] [--yes]` | Validate the manifest, copy into `$COS_APPS_DIR/<id>/`, and (unless `--no-consent`) walk through the AI consent prompt. No-op `copied:false, in_place:true` if the source is already inside `$COS_APPS_DIR`. |
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
| `COS_DATA_DIR` | Where Python apps may persist data. | `/var/lib/cos` |
| `COS_SDK_PYTHON_DIR` | Force the SDK lookup to a specific dir. Must contain both `claw_os_sdk/` and `cos_runtime/` as subdirs. | unset → kernel probes `/usr/lib/cos/python` + sibling dev paths |
| `COS_APP_ID` | The id of the calling app. **Auto-set by the bridge** to the app's directory name; do not override. | (auto) |
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
