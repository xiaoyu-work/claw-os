# Permission Tiers and App Capabilities

Two layers of permissions in Claw OS:

1. **Role / scope on spawned processes** — set when the agent calls the `cos_proc` tool's `spawn` command.
2. **Per-app capability gating** — apps declare verbs in their manifest and the runtime checks them via `cos perms check <verb>`. App authors do not call this themselves; the `_lib/policy.py` helper does.

## Roles passed to `cos_proc spawn`

| Role | Allowed Operations |
|------|--------------------|
| `observer`   | Read only |
| `worker`     | Read, write inside scope |
| `curator`    | Read, write, delete |
| `connector`  | Read, write, network |
| `automator`  | Read, write, exec |
| `agent-host` | Read, write, exec, network |
| `admin`      | All operations |

```json
{ "command": "spawn",
  "args": ["--session", "reader-1", "--role", "observer",
           "--", "analyze.py"] }
{ "command": "spawn",
  "args": ["--session", "builder-1", "--role", "worker",
           "--scope-path", "/home/cos/project",
           "--", "build.py"] }
```

Child processes cannot escalate beyond the parent's role or widen the parent's scope.

## App capability check

Apps written in Python use `apps/_lib/policy.py`, which shells out:

```bash
cos perms check <verb> [--scope <path>]
```

The verb (e.g. `fs.read`, `net.http`) must be declared in the app's manifest. Agents and end users do not invoke `cos perms` directly.

