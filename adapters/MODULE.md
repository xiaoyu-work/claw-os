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

## Provenance and packaging status

Adapters are **not packaged today**: nothing under `packaging/` or
`rootfs/` installs `adapters/` onto a system. They are therefore
source-tree content, do not inherit vendor trust, and are quarantined by
the extension-provenance gate until they are either signed and installed
as packages or given an explicit developer grant:

```bash
cos provenance dev-trust --kind mcp --id <adapter-id> --path adapters/<id>
```

Productizing an adapter means shipping it as a signed package directory
(`agent-api.json` plus `.provenance.json`) under an approved package
root. See [`../docs/extension-provenance.md`](../docs/extension-provenance.md).

## Dependencies

Adapters use argument-vector subprocess APIs, never shell interpolation.
Operation manifests and runtime validation remain aligned. External binary
failure is surfaced as an adapter error, not a successful empty result.

## Tests

```bash
PYTHONPATH=claw-os-sdk/python/src:cos-runtime/python/src \
  python3 -m pytest -q adapters
```
