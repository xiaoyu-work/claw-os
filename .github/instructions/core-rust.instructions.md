---
applyTo: "core/**/*.rs,crates/claw-*/**/*.rs,crates/cos-*/**/*.rs,claw-os-sdk/rust/**/*.rs,cos-runtime/rust/**/*.rs"
---

# Core Rust

- Read `core/MODULE.md`; for agent code also read
  `core/src/agent/MODULE.md`.
- Preserve the definition → provider → consumer dependency direction.
- Do not bypass `clawd`, capability scopes, the guarded tool registry, hooks,
  session persistence, or audit logging.
- Validate external identifiers, paths, and arguments before authorization or
  side effects.
- Provider changes must keep streaming/non-streaming text, tools, opaque
  reasoning state, usage, errors, pool accounting, and fallback behavior
  equivalent.
- Keep model-visible prompt additions reconstructable from session/audit data.
- Prefer extracting a natural responsibility from an oversized dispatcher
  rather than extending it with another unrelated concern.

Validation:

```bash
cargo test -p cos <test-filter> -- --test-threads=1
(cd core && cargo clippy -- -D warnings)
```
