# Network Firewall & Rate Limiting

Call the `cos_netfilter` tool — **not the shell** — to manage outbound firewall rules and per-host rate limits. There is no user-facing `cos netfilter` CLI command.

## Tool shape

`cos_netfilter` takes `{ "command": "<verb>", "args": [...] }`. Verbs: `add`, `remove`, `list`, `check`, `reset`, `default`, `export`, `rate-limit`, `rate-limits`, `rate-limit-remove`, `rate-check`.

## Firewall rules

```json
{ "command": "default", "args": ["deny-all"] }
{ "command": "add",     "args": ["--allow", "api.openai.com", "--port", "443"] }
{ "command": "add",     "args": ["--allow", "*.github.com"] }
{ "command": "check",   "args": ["api.openai.com"] }
{ "command": "list",    "args": [] }
```

## Rate limits

Prevent the agent from blowing through API quotas.

```json
{ "command": "rate-limit",        "args": ["api.openai.com", "--rpm", "60", "--burst", "10"] }
{ "command": "rate-check",        "args": ["api.openai.com"] }
{ "command": "rate-limits",       "args": [] }
{ "command": "rate-limit-remove", "args": ["api.openai.com"] }
```

Untrusted code should always be run through `cos_sandbox` with `--no-network`, not just gated by netfilter rules.
