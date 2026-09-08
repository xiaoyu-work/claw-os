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
| `app_sources.py --native` | Validate and refresh product-declared native libraries/assets (Calendar, Clipboard, Widget Rail) and standalone Launcher/Editor/Files at stable ignored build paths from the immutable pin |

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

`--stage <root>` validates all locked App identities; `--package agent` or
`--package desktop` preserves the explicit Debian package partition.
`--app-path <id>` resolves a single source directory for desktop package
assembly. None of these commands fetches application code at runtime.
Native preparation validates all declared names and product-local source paths
before replacement, rejects duplicate exports and only replaces exact declared
component paths; unrelated build caches are preserved.
