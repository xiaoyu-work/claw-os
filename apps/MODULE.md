# Apps Module

## Purpose

`apps/` contains bundled Python operations exposed through `cos app`. Each app
is declarative at discovery time and executable only when an operation is
invoked.

## Responsibilities

- Declare operations, args, dependencies, AI use, and capability needs.
- Validate untrusted input before requesting policy/capability authority.
- Return structured JSON-compatible results.
- Use the public SDK for OS/agent access and `cos_runtime` only for bundled-app
  policy/runtime helpers.

## Key Files

| Path | Role |
| --- | --- |
| `<id>/app.json` | App identity and operation/capability contract |
| `<id>/main.py` | `run(command, args)` implementation |
| `<id>/test_main.py` | App behavior, validation, and scope tests |
| `_shared/` | Shared safe filesystem/HTTP/process helpers |
| `gateway/` | External messaging gateways and shared gateway safety helpers |
| [`../docs/app-development.md`](../docs/app-development.md) | Normative app/manifest development contract |

## Dependencies

Apps do not import model-provider SDKs or own provider credentials. AI calls go
through the Claw OS SDK/agent gate. Bundled capability checks use
`cos_runtime.policy`; operation `needs` in `app.json` must match runtime checks.
Schema/listing paths must not execute `main.py`.

## Tests

```bash
PYTHONPATH=claw-os-sdk/python/src:cos-runtime/python/src \
  python3 -m pytest -q apps

# One app
PYTHONPATH=claw-os-sdk/python/src:cos-runtime/python/src \
  python3 -m pytest -q apps/<id>/test_main.py
```

When available, also run `cos app lint <id>`. Tests should verify that invalid
arguments are rejected before policy checks and that requested scopes are
exact.
