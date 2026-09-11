# Workflows Module

## Purpose

`.github/workflows/` defines the pull-request test pipeline plus manually
dispatched and reusable test/publication pipelines.

## Responsibilities

- Run core/browser/Python validation, including pinned App source staging and
  preservation of the package-owned runtime during rootfs registration.
- Prepare immutable product source inputs before cross-repository Rust tests,
  retaining real App/broker coverage without a second local implementation.
- Run OS adapter/SDK/runtime and pinned staging/MCP fixtures, not the removed
  OS `apps/` tests. App helper tests/vectors belong to App `tests/shared`;
  process fixtures receive separately staged common support, the required
  `python3-idna` runtime dependency and OpenSSL for private HTTPS certificates.
- Exercise the public App data codec/client, independent OS provider library and
  executable, exact export/ELF guards, and private real-kernel fixtures whose
  clients remain non-Root. No GUI/Clipboard admission or release gate is relaxed.
- Build the internal display control/PAM crates with `libpam0g-dev`, and run
  the actual Root-authenticated GUI/session fixture with a private `dbus-daemon`.
  Private namespace cases cover startup-failure exit and deferred retirement
  reclamation as well as normal lifecycle. Agent packaging builds the PAM
  library for GNU/glibc even when core uses musl.
- Run egress retirement cases against private listener queues and stalled DNS,
  with synthetic NSS configuration confined to a private mount/network namespace.
  Real lookup children must be reaped before the retirement acknowledgement.
- Build shared Docker/WSL images per architecture.
- Build and sign independent multi-architecture APT packages/repository.
- Build the React/Vite web desktop as an independent Pages input.
- Publish GHCR manifests, WSL releases, and GitHub Pages.

## Key Files

| Path | Role |
| --- | --- |
| `test.yml` | Pull-request, manual, and reusable test/clippy workflow |
| `build-docker-and-wsl.yml` | Shared rootfs, GHCR image, WSL artifacts/releases |
| `publish-agent-package.yml` | Independent Agent build, Ubuntu smoke test, and publication |
| `publish-base-package.yml` | Independent Claw OS Base build and publication |
| `publish-desktop-package.yml` | Independent full-rootfs Desktop build and publication |
| `publish-website.yml` | Website-only manual publication |
| `publish-apt-repo.yml` | Internal web/APT artifact composition and Pages deployment |
| `publish-sdk-release.yml` | GitHub SDK artifacts and synchronized language tags |
| `publish-app-platform.yml` | Manually published versioned SDK/runtime/toolkit development artifact for independent App builds |
| `release.yml` | Umbrella test + all publication channels |

## Dependencies

Workflow commands call repository scripts that remain the implementation source
of truth. Secrets are referenced by name only. Package publication requires the
signing key and never produces an unsigned fallback. Trigger documentation
must match each `on:` block.

## Tests

Run `actionlint` when available, parse YAML, and run the exact changed shell
commands in the narrowest safe environment.
