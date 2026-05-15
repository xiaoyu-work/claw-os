# `adapters/` — third-party tool wrappers for the system Agent

Each subdirectory here wraps **one** open-source command-line tool so
the Claw OS Agent (and any other MCP host on the system) can discover
and call it without knowing how the upstream is invoked.

## How discovery works

At agent startup, `core/src/agent/tools/mcp/discover.rs` scans every
directory listed in:

1. `agent.agent_api_paths` (config override, used by tests / dev).
2. `$XDG_DATA_HOME/claw/agent-api/` *(per-user)*.
3. Every `$XDG_DATA_DIRS` entry joined with `claw/agent-api/`
   *(system-wide, typically `/usr/share/claw/agent-api/` and
   `/usr/local/share/claw/agent-api/`)*.

For every `*.json` file it finds it parses a `claw.agent-api/v1`
manifest and, if `enabled` and `ai.callable_by_ai` are both true,
registers the described MCP server alongside any `[[agent.mcp_servers]]`
configured in `config.json`. First match per `id` wins, so a per-user
manifest under `$XDG_DATA_HOME` shadows a system one with the same
`id`.

## Manifest schema

See the rustdoc on `AgentApiManifest` in
`core/src/agent/tools/mcp/discover.rs` for the authoritative schema.
A minimum-viable manifest is:

```json
{
  "schema": "claw.agent-api/v1",
  "id": "org.qpdf",
  "name": "pdf",
  "title": { "en": "qpdf PDF toolkit" },
  "vendor": "claw-adapter",
  "license": "Apache-2.0",
  "transport": "mcp+stdio",
  "command": "python3",
  "args": ["${manifest_dir}/main.py"],
  "env": {},
  "timeout_secs": 30,
  "enabled": true,
  "ai": {
    "callable_by_ai": true,
    "uses_ai_internally": false,
    "safety": "standard",
    "origins": ["external-content"]
  }
}
```

The `${manifest_dir}` token in `command`, any `args` entry, or any
`env` value resolves to the absolute parent directory of the manifest
at load time, so the same JSON works both in-repo (sitting next to
`main.py`) and installed under `/opt/claw/adapters/<name>/`.

## Directory layout

```
adapters/
  <name>/
    manifest.json   # claw.agent-api/v1 sidecar
    main.py         # MCP server using claw_os_sdk.serve
    test_main.py    # Python unit tests
```

At install time the manifest is copied to
`/usr/share/claw/agent-api/<id>.json` and the rest of the directory to
`/opt/claw/adapters/<name>/`. Until the .deb packaging is in place,
adapters are exercised by pointing `agent.agent_api_paths` at this
repo's `adapters/` directory.

## Authoring an adapter

The adapter is a Python MCP server using the `claw_os_sdk.serve.App`
helper. The `main.py` is responsible for resolving the SDK install
location so it can `from claw_os_sdk import serve` both in-repo and
after install — see `adapters/_template/main.py` for the copy-paste
bootstrap.

## Audit semantics

Every `tools/call` invocation that arrives via this MCP path is logged
to `ai.jsonl` and gated by the registered tool's caps (per the
kernel's existing `ToolRegistry` + `ApprovalGate`). User-initiated GUI
or CLI invocations of the upstream tool — `qpdf --pages a 1-3 -- a.pdf out.pdf`
typed in a terminal, for example — are **not** logged: only the
adapter surface is audited, because that's the surface the agent uses.

## Testing in-repo

```bash
cd adapters/<name>
python3 -m unittest test_main
```

To test end-to-end discovery on a dev box, set
`agent.agent_api_paths = ["<repo>/adapters"]` in `config.json` and
run `cos agent ask "..."`. The tools show up as `mcp_<name>_<tool>`
in `cos agent tools`.
