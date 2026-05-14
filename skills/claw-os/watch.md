# File / Process / Service Watching

Call the `cos_watch` tool — **not the shell** — for event-driven watching backed by inotify (Linux). There is no user-facing `cos watch` CLI command.

## Tool shape

`cos_watch` takes `{ "command": "<verb>", "args": [...] }`. Verbs: `file`, `dir`, `proc`, `on`, `multi`, `history`.

```json
{ "command": "file", "args": ["/home/cos/output.txt", "--timeout", "30"] }
{ "command": "dir",  "args": ["/home/cos/results",     "--timeout", "60"] }
{ "command": "proc", "args": ["build-1",               "--timeout", "300"] }
```

## Multi-source watch

Returns on the first event from any source.

```json
{ "command": "multi",
  "args": [
    "--file", "/home/cos/main.py",
    "--dir",  "/home/cos/output/",
    "--proc", "worker-1",
    "--service", "my-api",
    "--timeout", "60"
  ] }
```

## OS events

```json
{ "command": "on", "args": ["proc.exit",          "--session", "build-1", "--timeout", "600"] }
{ "command": "on", "args": ["service.health-fail", "--name",   "my-api",  "--timeout", "3600"] }
{ "command": "on", "args": ["ipc.message",         "--session", "worker-1", "--timeout", "30"] }
{ "command": "on", "args": ["credential.expired",  "--name",   "API_TOKEN", "--timeout", "300"] }
```

## History

```json
{ "command": "history", "args": ["--limit", "20", "--source", "file"] }
```
