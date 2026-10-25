# Workflows Module

## Purpose

`.github/workflows/` defines the pull-request test pipeline plus manually
dispatched and reusable test/publication pipelines.

## Responsibilities

- Run core/browser/Python validation, including resolved App-main source staging and
  preservation of the package-owned runtime during rootfs registration.
- Fetch current App main and prepare an immutable snapshot before cross-repository Rust tests,
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
- Exercise App-service data views with real sandboxed MCP/SQLite writes across
  distinct execution UIDs and mount lifetimes, retaining owner file identity
  and refusing foreign roots, aliases, submounts and replaced bindings.
- Exercise task-host ordinary App registration and persistent SQLite writes
  through authenticated private broker messages, across Host namespace/UID
  replacement, without exposing caller-selected data roots.
- Exercise checked App-service retirement with a real Host/cgroup, retained
  mount failures, cancellation and owner/identity/capacity fencing; cleanup
  failure must not become a successful stop or replacement.
- Exercise ordinary App bind-gated startup with the real runner and sandbox,
  including refused/delayed binding, exact stdin limits, raw bytes and EOF.
- Exercise real staged Calendar MCP writes and public SDK reads through the
  owner-scoped broker and standalone reader, including live WAL visibility,
  wrong scopes, single-use grants and physical read-only mounts.
- Build shared Docker/WSL images per architecture.
- Build and sign independent multi-architecture APT packages/repository.
- Build the React/Vite web desktop as an independent Pages input.
- Publish GHCR manifests, WSL releases, and GitHub Pages.

## Key Files

| Path | Role |
| --- | --- |
| `test.yml` | Pull-request, manual, and reusable test/clippy workflow |
| `release.yml` | Manual `agent`, `base`, `desktop`, `images`, `website` or `all` release; shared package builds, one website/APT composition and Pages deployment |
| `publish-sdk-release.yml` | GitHub SDK artifacts and synchronized language tags |
| `publish-app-platform.yml` | Manually published versioned SDK/runtime/toolkit development artifact for independent App builds |
| `../actionlint.yaml` | Dedicated runner labels and compatibility with GitHub's queued concurrency |
| `../../scripts/ci/` | Release planning, signing-key import, package checks and installed Agent/privileged test scripts |

There are four workflow entry points. `release.yml` freezes one commit-derived
package version and, when Apps are needed, one App-main snapshot for the whole
run. `all` calls `test.yml` with that snapshot before building. Targeted releases
retain their component checks without rerunning the full shared suite.
Agent/Base packages are built once and downloaded by the image matrix; image
jobs never compile the shared Rust binaries. Docker and WSL consume the same
stamped rootfs per architecture. Desktop retains its dedicated full-rootfs
build and native source graph.

Release runs queue without cancelling one another. Selected packages are merged
with the authenticated existing pool only once, alongside one independent web
build, then signed and deployed once. `images` builds signed Agent/Base inputs
without publishing those packages to APT. `website` preserves the existing
signed package pool. Non-expiring indexes need no scheduled refresh.

## Dependencies

Workflow commands call repository scripts that remain the implementation source
of truth. Long privileged test steps live in `scripts/ci/test-boundaries.sh`;
the workflow keeps their original names and order, and the packaging contract
checks both the invocation and the extracted implementation.
Secrets are referenced by name only. Package publication requires the
signing key and never produces an unsigned fallback. Trigger documentation
must match each `on:` block.

## Tests

Run `actionlint` when available, parse YAML, and run the exact changed shell
commands in the narrowest safe environment.
