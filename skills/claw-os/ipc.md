# Inter-Process Communication

Call the `cos_ipc` tool — **not the shell** — for cross-session messaging, locks, barriers, and streaming pipes. There is no user-facing `cos ipc` CLI command.

## Tool shape

`cos_ipc` takes `{ "command": "<verb>", "args": [...] }`. Verbs: `send`, `recv`, `list`, `clear`, `lock`, `unlock`, `locks`, `barrier`, `pipe`.

```json
{ "command": "send",
  "args": ["worker-1", "build complete", "--from", "orchestrator"] }
```

```json
{ "command": "recv",
  "args": ["my-session", "--timeout", "30"] }
```

## Locks

Mutual exclusion. Stale locks from dead processes are auto-reclaimed.

```json
{ "command": "lock",   "args": ["database", "--holder", "agent-1", "--timeout", "10"] }
{ "command": "unlock", "args": ["database", "--holder", "agent-1"] }
{ "command": "locks",  "args": [] }
```

## Barriers

Block until N sessions reach a sync point.

```json
{ "command": "barrier",
  "args": ["merge-ready", "--expect", "3", "--session", "search-1", "--timeout", "60"] }
```

## Streaming pipes

Named channels with replay, backpressure, and follow mode. `pipe` is itself a verb; its operation goes into `args`.

```json
{ "command": "pipe", "args": ["create",    "my-events", "--buffer-size", "500"] }
{ "command": "pipe", "args": ["publish",   "my-events", "{\"type\":\"progress\"}"] }
{ "command": "pipe", "args": ["subscribe", "my-events", "--follow", "--timeout", "30"] }
{ "command": "pipe", "args": ["list"] }
{ "command": "pipe", "args": ["destroy",   "my-events"] }
```
