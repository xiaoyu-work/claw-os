---
applyTo: "claw-os-sdk/wire/**/*,claw-os-sdk/node/**/*,claw-os-sdk/go/**/*"
---

# SDK Wire and Generated Bindings

- Read `claw-os-sdk/MODULE.md`.
- Treat the versioned wire schema as the source of truth; do not evolve one
  language binding independently.
- Preserve backwards-compatible field names, defaults, and serialization
  unless introducing an explicitly versioned protocol change.
- Regenerate bindings after wire changes instead of editing generated files.
- Update conformance tests and all affected language SDKs in the same change.
- Public SDK code must not expose internal `cos-runtime` policy helpers or
  broker implementation details.

