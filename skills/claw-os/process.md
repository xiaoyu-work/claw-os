# Process Sessions

Call the `cos_proc` tool — **not the shell** — to spawn, query, and control long-running processes registered with cos. There is no user-facing `cos proc` CLI command.

## Tool shape

`cos_proc` takes `{ "command": "<verb>", "args": [...] }`. Supported verbs: `spawn`, `status`, `output`, `kill`, `list`, `wait`, `signal`, `result`, `stats`, `renice`.

```json
{
  "command": "spawn",
  "args": [
    "--session", "build-1",
    "--group", "ci",
    "--role", "worker",
    "--scope-path", "/work",
    "--", "cargo", "build"
  ]
}
```

## Common verbs

| Verb | Purpose |
|---|---|
| `spawn` | Start a process. Use `--session <id>`, optional `--group`, `--role`, `--scope-path`, `--priority`. End flags with `--` then the command. |
| `status <id>` | Is the session running? |
| `output <id>` | Read buffered stdout/stderr. `--tail N`, `--follow`, `--since-offset N`. |
| `wait <id>` | Block until exit. `--timeout <secs>` or `--group <name>`. |
| `result <id>` | One-call summary: status, duration, output tails, `likely_success`. |
| `signal <id> <SIGNAL>` | Send a signal (e.g. `TERM`, `INT`). |
| `kill <id>` | Terminate the session, or `--group <name>` for a whole group. |
| `list` | All sessions. `--group <name>` to filter. |

## Result envelope

Every call returns JSON. `spawn` returns `{session, pid, group?}`; `result` returns the comprehensive summary an agent should consume to decide next steps without making three more calls.
