---
applyTo: "desktop/**/*"
---

# Desktop Product Fork

- Read `desktop/README.md`, `desktop/PROVENANCE.md`, and the component README.
- Preserve every component license and upstream copyright notice.
- Keep AI logic in `core/` or first-party crates and communicate through a
  stable CLI/DBus/Wayland/MCP boundary; do not pull privileged agent logic into
  GPL desktop processes.
- Scope changes to the owning component; avoid workspace-wide reformatting of
  the vendored product fork.
- Internal `cosmic-*` binary/crate names may remain even when public App IDs
  use `com.clawos.*`.
- `desktop/icons-tela/links/` contains case-sensitive symlink names. Never
  commit Windows case-collision phantom modifications.
- Use the component's Cargo/just manifest and tests rather than assuming the
  root Rust workspace owns desktop crates.

