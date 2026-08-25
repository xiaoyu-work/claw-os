---
applyTo: "apps/**/*.py,apps/**/app.json,adapters/**/*.py,adapters/**/app.json,claw-os-sdk/python/**/*.py,cos-runtime/python/**/*.py"
---

# Python Apps, Adapters, and SDK

- Read `apps/MODULE.md` and `docs/app-development.md` for bundled app work.
- Keep `app.json` operation args, dependencies, AI declaration, and `needs`
  aligned with `main.py` behavior.
- Validate untrusted input before calling `cos_runtime.policy.require`.
- Request the narrowest exact capability scope; do not replace validation with
  wildcard authority.
- Apps do not import provider SDKs or own model credentials. Use the Claw OS
  SDK/agent gate.
- Reuse `_shared` safety helpers for filesystem, subprocess, network, and
  credential handling.
- Do not manually edit generated SDK bindings; update the wire source and
  regenerate.

Validation:

```bash
PYTHONPATH=claw-os-sdk/python/src:cos-runtime/python/src \
  python3 -m pytest -q <affected-test-paths>
```
