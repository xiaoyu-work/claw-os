# Scripts Module

## Purpose

`scripts/` contains repository maintenance, installation, image identity, and
shared build helpers.

## Responsibilities

- Provide reusable target/rootfs helper functions.
- Keep architecture, image profile, package version, and identity logic
  centralized.
- Fail explicitly on unsupported hosts or unsafe state.

## Key Files

| Path | Role |
| --- | --- |
| `lib/image-profiles.sh` | Target feature-set source of truth |
| `lib/arch.sh` | Architecture/target mapping |
| `lib/package-version.sh` | Monotonic Debian package version |
| `lib/image-identity.sh` | Image user/identity assertions |
| `lib/git-readonly.sh` | Read-only Git wrapper for privileged builds |
| `app_sources.py` | Resolve the immutable App repository pin and stage product-owned package assets |

## Dependencies

Rootfs, targets, and packaging source these helpers rather than reimplementing
their logic. Scripts must be LF-only, non-interactive in CI, and safe when run
under sudo.

## Tests

```bash
bash -n scripts/*.sh scripts/lib/*.sh
python3 -m pytest -q packaging/deb/tests/test_app_sources.py
```

Also run the narrowest consuming target/package command.
