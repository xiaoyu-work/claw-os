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
| `publish-agent-package.yml` | Independent Agent build, Ubuntu smoke test, and publication |
| `publish-base-package.yml` | Independent Claw OS Base build and publication |
| `publish-desktop-package.yml` | Independent full-rootfs Desktop build and publication |
| `publish-apt-repo.yml` | Internal cumulative signed-repository publisher |
| `release.yml` | Umbrella test + all publication channels |

## Dependencies

Workflow commands call repository scripts that remain the implementation source
of truth. Secrets are referenced by name only. Package publication requires the
signing key and never produces an unsigned fallback. Trigger documentation
must match each `on:` block.

## Tests

Run `actionlint` when available, parse YAML, and run the exact changed shell
commands in the narrowest safe environment.
