# 06 — App manifest v2 & capability enforcement (for developers)

This is the contract between an app and the kernel: how an app
*declares* the capabilities it needs, how the kernel *resolves*
those declarations at call time, and how an app must call back
into the kernel to gate the operation.

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
   │   Mode::Permissive (default during migration) — no       │
   │     session ⇒ allow. Once everything is on caps, flip    │
   │     to strict.                                           │
   │   Mode::Strict — no session, missing cap, scope outside  │
   │     range, or PID-ancestry mismatch ⇒ structured Denial. │
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

## App manifest v2: declaring needs

Each app lives under `apps/<name>/` with two files: `main.py`
(the implementation) and `app.json` (the manifest). The v2
manifest extends v1 with an `operations` block that names every
verb the app exposes and the capabilities each verb needs.

### Minimal example

```json
{
  "name": "fs",
  "manifest_version": 2,
  "version": "0.1.0",
  "label": "Files",
  "description": "Agent-native file system with metadata and search",
  "operations": {
    "ls": {
      "label": "List a folder",
      "needs": [
        {
          "verb": "fs.read",
          "scope_from": "$arg.path",
          "why": "Read the contents of the folder you asked to list."
        }
      ]
    },
    "rm": {
      "label": "Delete a file",
      "needs": [
        {
          "verb": "fs.delete",
          "scope_from": "$arg.path",
          "why": "Remove the file you asked to delete."
        }
      ]
    },
    "mv": {
      "label": "Move a file",
      "needs": [
        { "verb": "fs.read",   "scope_from": "$arg.src" },
        { "verb": "fs.write",  "scope_from": "$arg.dst" },
        { "verb": "fs.delete", "scope_from": "$arg.src" }
      ]
    }
  }
}
```

### Field reference

| Field | Type | Meaning |
|---|---|---|
| `manifest_version` | `2` | Must be exactly `2`. v1 manifests still load via the legacy path. |
| `name` | string \| LocalizedText | Internal app name; also a `LocalizedText` if you want a translated label. |
| `label` | string \| LocalizedText | Friendly name shown in approval dialogs. |
| `description` | string | Single-line summary for the agent registry. |
| `operations.<verb>.label` | string \| LocalizedText | What this operation does (one short verb). |
| `operations.<verb>.needs[]` | array | One entry per capability the operation needs. |
| `operations.<verb>.needs[].verb` | string | The kernel capability verb (e.g. `fs.read`). Validated against the catalog at load time. |
| `operations.<verb>.needs[].scope_from` | string | A binding for the scope: `$arg.<name>` (use named arg), `$fixed.<value>` (constant), or `$wild` (explicit wildcard — be deliberate). |
| `operations.<verb>.needs[].why` | string \| LocalizedText | The "why" line shown in the approval dialog. Optional but recommended. |

### LocalizedText

Any field marked `string \| LocalizedText` accepts either:

```json
"label": "Files"
```

or:

```json
"label": { "en": "Files", "zh-CN": "文件" }
```

The bare-string form is treated as the English translation;
`LocalizedText::validate()` requires that English always be
present.

### Late binding

`scope_from: "$arg.path"` is *late-bound*: the manifest doesn't
know the value, only the source. When `Manifest::resolve_needs`
is called at invocation time with the actual `{path: "/tmp/x"}`
arguments map, it produces a concrete `Cap { verb: fs.read,
scope: Path("/tmp/x") }` for the approval check.

`$fixed.<value>` is useful for verbs whose scope is intrinsic to
the operation (e.g. an app that only ever reads its own data
directory).

`$wild` produces a `Scope::Wild` request. Every wild request
shows up red in the approval dialog and audit log — use only for
operations that *genuinely* need access to everything (very rare).

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

* `permissive` *(default)* — no `COS_SESSION` env var ⇒ allow.
  The migration knob: legacy code keeps working while you wire
  up `caps::require` site by site.
* `strict` — no session ⇒ deny with `DenialReason::NoSession`.
  Set this before flipping the default.

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

---

## Migration from v1 manifests

The v1 manifest had `commands` (a string map of name → blurb)
and no capability metadata. v1 manifests continue to load — the
kernel treats their commands as unconstrained and falls back to
the legacy tier check inside `bridge::run_python_app`. As you
upgrade each app:

1. Add `"manifest_version": 2` at the top of `app.json`.
2. Replace `commands` with the v2 `operations` block, declaring
   the capabilities each operation needs.
3. Add `from _lib import policy` to `main.py` and call
   `policy.require()` in each command handler after the args are
   parsed.
4. Wrap the dispatcher to surface `PermissionDenied` as a
   structured error.

Once every app is on v2, the legacy tier path in
`bridge::run_python_app` will be removed and `Mode::Strict`
becomes the default.
