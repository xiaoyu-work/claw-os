---
applyTo: "adapters/**/*.py,adapters/**/app.json,claw-os-sdk/python/**/*.py,cos-runtime/python/**/*.py"
---

# Python Apps, Adapters, and SDK

- Read `docs/app-development.md`; App implementations and common support use
  the owning guides in the external `clawos-app` repository.
- Keep `app.json` operation args, dependencies, AI declaration, and `needs`
  aligned with `main.py` behavior.
- Validate untrusted input before calling `cos_runtime.policy.require`.
- Request the narrowest exact capability scope; do not replace validation with
  wildcard authority.
- Apps do not import provider SDKs or own model credentials. Use the Claw OS
  SDK/agent gate.
- App-owned `_shared`, `gateway._shared` and `canonical_argv` support lives in
  external `shared/python`, not in OS SDK/runtime. OS integration fixtures
  needing it must use the pinned public staging contract, not source imports.
- Do not manually edit generated SDK bindings; update the wire source and
  regenerate.

Validation:

```bash
PYTHONPATH=claw-os-sdk/python/src:cos-runtime/python/src \
  python3 -m pytest -q <affected-test-paths>
```
