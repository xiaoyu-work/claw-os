# Workflows Module

## Purpose

`.github/workflows/` defines manually dispatched and reusable test/publication
pipelines.

## Responsibilities

- Run core/browser/Python validation.
- Build shared Docker/WSL images per architecture.
- Build and sign independent multi-architecture APT packages/repository.
- Publish GHCR manifests, WSL releases, and GitHub Pages.

## Key Files

| Path | Role |
| --- | --- |
| `test.yml` | Reusable test/clippy workflow |
| `build-docker-and-wsl.yml` | Shared rootfs, GHCR image, WSL artifacts/releases |
| `build-apt-repo.yml` | `.deb`, signed repository, Pages deployment |
| `release.yml` | Umbrella test + all publication channels |

## Dependencies

Workflow commands call repository scripts that remain the implementation source
of truth. Secrets are referenced by name only. A missing signing key skips APT
publication rather than producing unsigned output. Trigger documentation must
match each `on:` block.

## Tests

Run `actionlint` when available, parse YAML, and run the exact changed shell
commands in the narrowest safe environment.
