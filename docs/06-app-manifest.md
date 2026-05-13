# 06 — App manifest & capability enforcement (for developers)

This is the contract between an app and the kernel: how an app
*declares* the capabilities it needs, how the kernel *resolves*
those declarations at call time, and how an app must call back
into the kernel to gate each operation.

> Reading this because you're a *user* of Claw OS, not an app
> author? See `docs/05-permissions.md` instead.

---

## The shape of the system

```
┌─ User grants role/caps to a session ──────────────────────────┐
│                                                                │
│   cos agent run --role worker --scope ~/Documents/**           │
│                                                                │
└─────────────────────────────┬──────────────────────────────────┘
                              │
                              ▼
            session registry  ($COS_DATA_DIR/proc/registry.json)
                              │
                              │  COS_SESSION env var threads
                              │  this session through every
                              │  subprocess.
                              ▼
   ┌──────────────────────────────────────────────────────────┐
   │ caps::require(verb, scope)  — the one and only kernel    │
   │ entry point. Every gated operation flows through here.   │
   │                                                          │
   │   Mode::Strict (default) — no session, missing cap,      │
   │     scope outside range, or PID-ancestry mismatch ⇒      │
   │     structured Denial.                                   │
   │   Mode::Permissive — opt-in escape hatch for first-boot  │
   │     scripts that run before the session registry exists. │
   │     Set `COS_PERMS_MODE=permissive` to use it.           │
   └──────────────────────────────────────────────────────────┘
                              │
              ┌───────────────┴────────────────┐
              ▼                                ▼
   Rust callers (kernel)              Python apps (apps/*)
                                              │
                                              ▼
                              shell out to `cos perms check`
                              via apps/_lib/policy.require()
```

The two-tier enforcement model has **one** gate at the
Rust/Python boundary (coarse, "may I dispatch the fs app at
all?") and one **per-operation** gate inside each Python handler
(fine, "may I delete *this specific* path?").

---

## The manifest: declaring needs

Each app lives under `apps/<id>/` with two files: `main.py`
(the implementation) and `app.json` (the manifest). The manifest
names every operation the app exposes and the capabilities each
operation needs.

There is exactly **one** manifest format — no version field,
no fallback path. Apps either parse and validate cleanly, or
they don't load.

### Minimal example

```json
{
  "id": "fs",
  "version": "0.1.0",
  "name": { "en": "Files", "zh-CN": "文件" },
  "summary": { "en": "Browse, read, write, and search files." },
  "icon": "📁",
  "operations": {
    "ls": {
      "label": { "en": "List a folder" },
      "args": [
        { "name": "path", "kind": "path", "required": true }
      ],
      "needs": [
        {
          "verb": "fs.read",
          "scope": { "kind": "from-arg", "arg": "path" },
          "why": { "en": "Read the contents of the folder you asked to list." }
        }
      ]
    },
    "rm": {
      "label": { "en": "Delete a file" },
      "args": [
        { "name": "path", "kind": "path", "required": true }
      ],
      "needs": [
        {
          "verb": "fs.delete",
          "scope": { "kind": "from-arg", "arg": "path" },
          "why": { "en": "Remove the file you asked to delete." }
        }
      ]
    }
  }
}
```

### Top-level fields

| Field | Type | Meaning |
|---|---|---|
| `id` | string `[a-z][a-z0-9_-]*` | Internal app slug; must match the directory name. |
| `version` | string | App version (semver-ish; the kernel does not parse it). |
| `name` | LocalizedText | Friendly name shown in lists and approval dialogs. |
| `summary` | LocalizedText | One-line description. |
| `icon` | string | Optional emoji or icon name. |
| `runtime` | `"python"` \| `"node"` \| `"shell"` \| `"binary"` | Bridge target. Defaults to `python`. |
| `entry` | string | Override entry file. Defaults are `main.py` / `main.js` / `main.sh` (Windows: `main.bat`) / `main` (Windows: `main.exe`). |
| `operations` | object | The verbs this app exposes, keyed by command name. |
| `dependencies` | object | Free-form dependency declarations for the package resolver. |

### Per-operation fields

| Field | Type | Meaning |
|---|---|---|
| `label` | LocalizedText | What this operation does (one short verb phrase). Required. |
| `summary` | LocalizedText | Optional longer explanation. |
| `args` | array of Arg | Declared input parameters. Order matters for the UI. |
| `needs` | array of Need | Capabilities required to run this operation. Empty = local-only. |

### Arg

```jsonc
{
  "name": "path",          // identifier referenced by `from-arg` scopes
  "kind": "path",          // path | host | name | text | number | bool
  "required": true,        // optional, default false
  "default": null,         // optional default value
  "label": { "en": "..." } // optional UI help text
}
```

`kind` values `path`, `host`, and `name` are the only ones that
can populate a scope (`text` / `number` / `bool` cannot).

### Need

```jsonc
{
  "verb": "fs.delete",
  "scope": { "kind": "from-arg", "arg": "path" },
  "why":  { "en": "Remove the file you asked to delete." }
}
```

`scope` is one of:

* `{ "kind": "from-arg", "arg": "<name>" }` — late binding: at
  call time the kernel reads the named arg's value and builds a
  `Scope` matching the arg's declared `kind`.
* `{ "kind": "fixed", "scope": { "kind": "path", "value": "/foo/**" } }`
  — the scope is hard-coded in the manifest. Useful for ops
  that always touch the same resource (e.g. a per-app data dir).
* `{ "kind": "wild" }` — explicit wildcard. There is no implicit
  `*`; the author has to spell this out so the approval dialog
  can flag it red.

`verb` is validated at parse time against
`core/src/caps/verb.rs::ALL_VERBS`. An unknown verb fails the
parse with `ManifestError::Json`, not a runtime error — broken
manifests never load.

### LocalizedText

Any field marked `LocalizedText` accepts either:

```json
"label": "Files"
```

or:

```json
"label": { "en": "Files", "zh-CN": "文件" }
```

The bare-string form is treated as the English translation;
`LocalizedText::validate()` requires that English always be
present. The user's active locale (`cos locale set`) decides
which translation renders.

---

## Enforcement on the Rust side

### The kernel API

```rust
use cos::caps::{require, Verb, Scope, Denial};

match require(Verb::FS_READ, Scope::path("/tmp/x")) {
    Ok(()) => { /* proceed */ }
    Err(denial) => {
        // denial.to_json()       → structured JSON for audit/UI
        // denial.summary()       → one-line human-readable message
        // denial.reason          → DenialReason enum
        return Err(denial.summary());
    }
}
```

### Where to call it

* **Direct kernel operations** (cron, netfilter, credential store,
  sandbox launcher): call `require()` immediately before the
  syscall, with the concrete scope.
* **Subprocess dispatch** (`bridge::run_python_app`, plugin
  launchers): use the coarse `agent.invoke` check at the
  boundary, then let the subprocess do its own fine-grained
  checks. This is the pattern in `router.rs::run_app_command` and
  `agent::tools::cos_apps::CosAppTool::exec` — both gate on
  `agent.invoke` with `Scope::name(app)` before delegating to
  `bridge::run_python_app`. Schema introspection
  (`command == "__schema__"`) bypasses the gate so tooling can
  describe apps it cannot run.

### Modes

Read from `COS_PERMS_MODE`:

* `strict` *(default)* — no `COS_SESSION`, missing cap, scope
  outside range, or PID-ancestry mismatch ⇒ deny.
* `permissive` — opt-in escape hatch. No session ⇒ allow. Use
  this only for first-boot installer scripts that run before the
  session registry exists.

### Anti-spoofing

`require()` does a PID-ancestry check on Linux: the caller's PID
must descend from the session's recorded PID. This is a
defence-in-depth against an attacker who learns `COS_SESSION` and
tries to use it from outside the session's process tree. The
check is skipped if `session.pid == 0` (used in tests).

### Structured denial

`Denial` exposes:

```rust
pub struct Denial {
    pub verb: Verb,
    pub requested_scope: Scope,
    pub granted_scopes: Vec<Scope>,
    pub reason: DenialReason,
    pub hint: Option<String>,
}
```

`reason` discriminates between `VerbNotGranted`,
`ScopeOutOfRange`, `NoSession`, and `PidAncestryMismatch`. Audit
logs should record the entire envelope; UIs should render
`summary()` + `hint`.

---

## Enforcement on the Python side

Python apps use the tiny helper at `apps/_lib/policy.py`:

```python
from _lib import policy

def cmd_rm(args):
    if not args:
        raise Exception("rm requires a path argument")
    path = os.path.abspath(args[0])
    policy.require("fs.delete", path=path)
    os.remove(path)
    return {"removed": path}
```

`policy.require()` shells out to `cos perms check`, which is the
**same** Rust `require()` function the kernel uses — there is no
parallel rule-set in Python.

### Exceptions

* `policy.PermissionDenied(denial)` — the kernel refused. The
  `denial` attribute holds the full envelope; `str(exc)` is the
  one-line summary suitable for logs.
* `policy.PolicyUnavailable` — the `cos` binary is missing or
  the response was malformed. This is a programmer/environment
  error, not a denial; treat it differently in your handler.

### Wrap your dispatcher

Surface denials structurally so the agent (or the user looking at
the JSON output) can branch on them:

```python
def run(command, args):
    if command == "__schema__":
        return _schema()
    handler = COMMANDS.get(command)
    if handler is None:
        return {"error": f"unknown command: {command}"}
    try:
        return handler(args)
    except policy.PermissionDenied as denied:
        return {"error": str(denied), "denial": denied.denial}
    except policy.PolicyUnavailable as exc:
        return {"error": f"capability check failed: {exc}"}
```

The bridge then surfaces this JSON unchanged to the agent's tool
call result.

### Scope argument names

`policy.require` accepts at most one of: `path=`, `host=`,
`name=`, `self_ref=`, `wild=True`. Unscoped verbs
(`ui.notify`, `time.delay`) take no scope argument. Passing more
than one raises `TypeError` to catch the mistake early.

### Schema introspection bypasses caps

The bridge skips the coarse `agent.invoke` gate when
`command == "__schema__"` so the agent registry can describe
every installed app even when the session lacks permission to
actually invoke them. **Do not** call `policy.require` inside
your `_schema()` function — by contract `__schema__` is always
allowed.

---

## Working with the catalog

Capabilities are registered in `core/src/caps/catalog.rs`. The
catalog says, for each verb, what scope kind it accepts (path,
host, name, self-ref, none), the human label, a one-line "what
this lets the agent do" blurb, the icon, and the risk level.

Adding a new verb is a three-step change:

1. Add a `pub const FOO_BAR: Verb = Verb::new("foo.bar");` in
   `verb.rs` and append it to `ALL_VERBS`.
2. Add a `CapMeta` entry in `catalog.rs` keyed on the new verb.
3. If it should be in a role bundle, add it to the appropriate
   role in `role.rs`.

`caps::self_check()` runs at boot and asserts every verb in
`ALL_VERBS` has a catalog entry and vice versa — so if you skip
step 2 the binary will refuse to start.

---

## Audit log

Every `caps::require` call (allow *and* deny) is appended to the
audit log at `$COS_DATA_DIR/audit/perms.jsonl`. The schema:

```jsonc
{
  "ts":         "2026-04-12T15:31:09Z",
  "session_id": "ag-3a91",
  "verb":       "fs.write",
  "scope":      { "kind": "path", "value": "/tmp/report.pdf" },
  "decision":   "allow" | "deny",
  "reason":     "verb-not-granted",    // only on deny
  "caller_pid": 8421,
  "agent_name": "Build Helper"          // optional, from session record
}
```

Apps should not write to this log directly — `caps::require`
emits the entry. Apps that need their own audit trail should use
the `audit` app or `cos audit` CLI.

---

## Testing

* `cargo test -p cos --bin cos caps::` runs all kernel-side
  capability tests.
* `cargo test -p cos --bin cos perms::` runs `cos perms check`
  CLI tests.
* Add new app-level tests under `apps/<name>/tests/`. The
  Python helper exposes `policy.check()` (returns the envelope
  without raising) for assertions.
* When testing strict-mode behaviour, set `COS_PERMS_MODE=strict`
  and provide a session via a synthetic
  `$COS_DATA_DIR/proc/registry.json`. The registry shape is:

  ```json
  {
    "sessions": [{
      "session_id": "test-sess",
      "pid": 0,
      "caps": [
        { "verb": "agent.invoke",
          "scope": { "kind": "name", "value": "fs" } },
        { "verb": "fs.read",
          "scope": { "kind": "path", "value": "/tmp/**" } }
      ]
    }]
  }
  ```

  `pid: 0` disables the ancestry check, which is necessary in
  tests that cannot fork to inherit the session.
