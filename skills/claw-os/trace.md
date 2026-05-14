# Execution Tracing

Call the `cos_trace` tool — **not the shell** — to record a tree-structured journal of what you did and why. There is no user-facing `cos trace` CLI command.

## Tool shape

`cos_trace` takes `{ "command": "<verb>", "args": [...] }`. Verbs: `start`, `end`, `span`, `span-end`, `show`, `list`.

```json
{ "command": "start", "args": ["refactor-task"] }
{ "command": "span",  "args": ["analyze"] }
// ...do work via other cos_* tools...
{ "command": "span-end", "args": [] }
{ "command": "span",  "args": ["verify"] }
// ...
{ "command": "span-end", "args": [] }
{ "command": "end",   "args": ["refactor-task"] }
```

Once a trace is open, other `cos_*` tool calls in the same session are automatically attached to it — you do not need to thread IDs through every call.

## View

```json
{ "command": "show", "args": ["refactor-task"] }
{ "command": "list", "args": ["--status", "active"] }
```

`show` returns the full tree (spans → operations) with timings, errors, and a `first_error` pointer when something failed.
