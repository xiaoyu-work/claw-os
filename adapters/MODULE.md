# Adapters Module

## Purpose

`adapters/` contains App packages that expose narrowly wrapped external
command-line tools through the authenticated MCP App Mesh.

## Responsibilities

- Validate paths/options before spawning external binaries.
- Translate tool output/errors into structured results.
- Declare binary dependencies and exact operation capability needs.
- Prevent shell injection, unsafe clobbering, and unbounded output.

## Key Files

| Path | Role |
| --- | --- |
| `<id>/app.json` | Signed App identity, MCP tools, dependencies, and needs |
| `<id>/main.py` | Safe external-binary invocation |
| `<id>/test_main.py` | Argument, command-line, output, and error tests |
| `_template/` | Starting structure for a new adapter |
| `README.md` | Adapter overview |

## Provenance and packaging status

Adapters are **not packaged today**: nothing under `packaging/` or
`rootfs/` installs `adapters/` onto a system. They are therefore source-tree
content and do not inherit vendor trust. Install one as an App package with
an authenticated publisher signature, or record an explicit digest-bound
development decision:

```bash
cos app install adapters/<id> --dev-trust
```

See [`../docs/extension-provenance.md`](../docs/extension-provenance.md).

## Dependencies

Adapters use argument-vector subprocess APIs, never shell interpolation.
Operation manifests and runtime validation remain aligned. External binary
failure is surfaced as an adapter error, not a successful empty result.

Adapters run through the task-owned Extension Host. Every call carries a
Gateway-authenticated caller context, receives only the exact capabilities
derived from `app.json.mcp.tools[].needs`, and is audited under the App's
verified package identity. An adapter that shells out to a binary must
declare both the binary dependency and the exact `proc.spawn` need.

## Tests

```bash
PYTHONPATH=claw-os-sdk/python/src:cos-runtime/python/src \
  python3 -m pytest -q adapters
```
