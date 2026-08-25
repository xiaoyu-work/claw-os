---
applyTo: ".github/workflows/**/*.yml,rootfs/**/*,targets/**/*,packaging/**/*,scripts/lib/**/*.sh,build.sh"
---

# Images, Packaging, and CI

- Read `rootfs/MODULE.md`, `packaging/MODULE.md`, `targets/MODULE.md`, and
  `docs/image-architecture.md` as applicable.
- `scripts/lib/image-profiles.sh` is the target feature-set source of truth.
- Rootfs features describe reusable OS capabilities; target scripts package a
  profile and add platform-only integration.
- Do not duplicate a rootfs build when consumers can safely share its complete
  stamped tree.
- Debian packages are assembled from compiled binaries and source assets; the
  APT channel does not require debootstrap/rootfs construction.
- Never publish an unsigned APT fallback. Missing signing material must skip
  publication explicitly.
- Keep provisioning shell scripts LF-only and validate them with `bash -n`.
- Update `docs/updating.md` when installed-system package/update behavior
  changes.
- Workflows are manually dispatched or reusable through `workflow_call`; do not
  describe push/PR automation unless the triggers actually exist.
