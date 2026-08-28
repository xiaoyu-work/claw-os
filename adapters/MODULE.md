# Adapters Module

## Purpose

`adapters/` exposes narrowly wrapped external command-line tools through the
same manifest/operation contract as bundled apps.

## Responsibilities

- Validate paths/options before spawning external binaries.
- Translate tool output/errors into structured results.
- Declare binary dependencies and exact operation capability needs.
- Prevent shell injection, unsafe clobbering, and unbounded output.

## Key Files

| Path | Role |
| --- | --- |
| `<id>/app.json` | Adapter operation/dependency contract |
| `<id>/main.py` | Safe external-binary invocation |
| `<id>/test_main.py` | Argument, command-line, output, and error tests |
| `_template/` | Starting structure for a new adapter |
| `README.md` | Adapter overview |

## Dependencies

Adapters use argument-vector subprocess APIs, never shell interpolation.
Operation manifests and runtime validation remain aligned. External binary
failure is surfaced as an adapter error, not a successful empty result.

Adapters attach as MCP servers and therefore run in the same
hostile-worker sandbox as any third-party server
([`../core/src/worker/MODULE.md`](../core/src/worker/MODULE.md)): a
read-only system image, no App data directory, no host paths beyond the
configured working directory, no network at all, and a per-launch broker
endpoint that admits nothing by default. An adapter that shells out to a
binary outside `/usr` must declare it as a dependency; a binary the
sandbox cannot see is not a smaller adapter, it is a failed launch.

## Tests

```bash
PYTHONPATH=claw-os-sdk/python/src:cos-runtime/python/src \
  python3 -m pytest -q adapters
```
