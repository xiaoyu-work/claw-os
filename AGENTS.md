# Repository Instructions for Coding Agents

This file is the shortest reliable path into the Claw OS repository. Use it to
choose the right subsystem before reading implementation files.

## Repository Documents

Read only the documents relevant to the task:

1. [`ARCHITECTURE.md`](ARCHITECTURE.md) — system components, dependency
   direction, entry points, and end-to-end data flows.
2. [`CONTRIBUTING.md`](CONTRIBUTING.md) — build commands and architecture rules.
3. [`docs/app-development.md`](docs/app-development.md) — Python app manifest,
   capability, SDK, lint, and install contracts.
4. [`docs/image-architecture.md`](docs/image-architecture.md) — rootfs features,
   image profiles, and target identity.
5. [`packaging/README.md`](packaging/README.md) and
   [`docs/updating.md`](docs/updating.md) — Debian packages, APT publication,
   and installed-system updates.
6. The nearest component README, especially
   [`desktop/README.md`](desktop/README.md) before desktop work.
7. The nearest maintained `MODULE.md` for local responsibilities, key files,
   dependencies, and tests. Guides cover the major core/agent subsystems,
   apps/adapters, SDK/runtime/crates/extensions, rootfs/targets/packaging,
   scripts, and workflows.

Source code and manifests are authoritative when prose is stale.

## Task Routing

Start with the smallest row matching the task. Read callers and tests before
editing additional surfaces.

| Task | Start here | Commonly coupled files |
| --- | --- | --- |
| `cos` CLI command or primitive | `core/src/main.rs`, `core/src/router.rs` | The primitive module, `core/src/clawd/`, inline Rust tests |
| Agent ask/chat loop | `core/src/agent/runtime/loop_.rs`, `core/src/agent/runtime/turn.rs` | `prompt/`, `tools/`, `memory/`, `llm/` |
| Agent worker process / broker isolation | `core/src/agentd/`, `core/src/bin/claw-agentd.rs` | `clawd/server.rs`, `agent/service.rs`, `clawd.service`, `packaging/deb/build-debs.sh` |
| LLM provider or model setup | `core/src/agent/llm/providers/`, `core/src/agent/llm/registry.rs`, `core/src/agent/setup.rs` | `types.rs`, `accumulate.rs`, streaming and non-streaming tests |
| Tool, guardrail, or approval | `core/src/agent/tools/registry.rs`, `core/src/agent/runtime/turn.rs` | `guardrails.rs`, hooks, capability checks, audit |
| Memory, recall, or sessions | `core/src/agent/memory/`, `core/src/session/` | runtime recording, prompt injection, audit/session CLI |
| Session journal or mutation bracketing | `core/src/session/journal/`, `core/src/clawd/journal.rs` | `core/src/clawd/server.rs` dispatch, `core/src/agentd/supervisor.rs`, authority audit, packaging modes |
| `clawd` RPC or privileged operation | `core/src/bin/clawd.rs`, `core/src/clawd/server.rs` | client RPC, caps, audit, the owning `clawd` module |
| Broker wire protocol or a new broker route | `core/src/clawd/routes.rs`, `core/src/clawd/wire/`, `core/src/clawd/transport/` | `client.rs`, every in-repo client, `audit_policy.rs`, `core/tests/clawd_broker_socket.rs` |
| MCP client/server integration | `core/src/agent/tools/mcp/`, `core/src/config.rs` | tool registry and agent lifecycle attachment |
| Python app operation | `apps/<id>/app.json`, `apps/<id>/main.py` | `test_main.py`, `cos_runtime.policy`, app lint |
| Adapter | `adapters/<id>/app.json`, `adapters/<id>/main.py` | adapter tests and external binary dependency |
| App/SDK wire contract | `claw-os-sdk/wire/`, language SDK package | generated bindings, conformance tests, `publish-sdk-release.yml` |
| Rootfs composition | `scripts/lib/image-profiles.sh`, `rootfs/build.sh`, `rootfs/features/` | target build script and package contents |
| WSL or Docker image | `.github/workflows/build-docker-and-wsl.yml`, `targets/wsl/`, `targets/docker/` | shared rootfs profile |
| Debian/APT package | `packaging/deb/`, `packaging/apt-repo/` | `publish-*-package.yml`, rootfs package-install features |
| Web desktop or website | `web/src/App.tsx`, `web/MODULE.md` | `web/src/components/`, `web/public/site/`, Pages composition workflow |
| Desktop component | `desktop/README.md`, `desktop/PROVENANCE.md`, component README | component Cargo/just manifest and license |
| CI workflow | `.github/workflows/` | scripts invoked by the workflow; only `test.yml` runs on pull requests, while publication workflows are manually dispatched or reusable |

## Development

Use Linux or WSL2. On Windows, Rust and Python commands should run inside WSL.
Clone into the Linux filesystem for rootfs/image builds; `/mnt/c` cannot
represent all device nodes, hardlinks, permissions, and case-sensitive paths
needed by the build.

Core development:

```bash
cargo check -p cos --lib
cd core && cargo build
```

Image and package entry points:

```bash
sudo ./build.sh wsl
./build.sh docker
ARCH=amd64 ./packaging/deb/build-debs.sh
```

Do not edit generated output under `build/` or `target/`.
`claw-os-sdk/python/src/claw_os_sdk/generated.py` is generated from the wire
schema; regenerate it from `claw-os-sdk/` with:

```bash
python3 wire/codegen.py
```

## Testing

Use the narrowest existing test first. Core tests that mutate process-wide
environment variables must run serially when combined.

Rust unit-test bodies live outside production source trees under each crate's
`test/unit/` directory, mirroring the `src/` path. Production modules contain
only a small `include!` declaration so unit tests retain private access.

During initial task discovery, do **not** scan `test/` or `tests/`. Read the
production entry point, types, and callers first. Open only the matching unit
test file after selecting the implementation, when confirming existing
behavior, adding a regression, or diagnosing a failure.

```bash
# One Rust test or module
cargo test -p cos <test-filter> -- --test-threads=1

# Complete core suite
(cd core && cargo test -- --test-threads=1)

# Exact CI clippy command
(cd core && cargo clippy -- -D warnings)

# Browser crate
cargo test -p cos-browser

# Complete Python suite from the repository root
PYTHONPATH=claw-os-sdk/python/src:cos-runtime/python/src \
  python3 -m pytest -q apps adapters claw-os-sdk/python/src cos-runtime/python/src
```

Documentation-only changes do not require code tests.

## Conventions

- Preserve unrelated dirty-worktree changes. Stage explicit paths rather than
  using `git add -A`.
- Keep public SDK code in `claw-os-sdk`; `cos-runtime` is for bundled apps and
  internal policy helpers.
- Apps declare operations and scopes in `app.json`; implementation belongs in
  `main.py`. Schema inspection must not execute app entrypoint code.
- Apps never call model-provider SDKs directly. AI access goes through the
  Claw OS SDK/agent gate so consent, budgets, logging, and provider ownership
  remain centralized.
- Privileged or model-visible behavior must remain capability-gated and
  reconstructable from session/audit logs.
- Reuse the service-definition/provider/consumer seam documented in
  `CONTRIBUTING.md`; consumers must not reach around an interface to a concrete
  provider.
- Do not add broad catches, silent success fallbacks, or casts that bypass type
  safety.
- `desktop/` is a product fork with vendored origins. Preserve component
  licenses/provenance and avoid repository-wide reformatting.
- `crates/obscura-*` are vendored browser internals. Keep changes scoped and
  preserve their workspace boundary.
- `desktop/icons-tela/links/` contains case-sensitive symlink names. A Windows
  checkout reports false modifications for case-colliding paths; never stage
  those phantom changes.

## Cross-Surface Change Contracts

### New or changed app operation

1. Update `app.json` operation args and `needs`.
2. Implement or change the `main.py` handler.
3. Validate untrusted args before the policy check.
4. Update `test_main.py`.
5. Run `cos app lint <id>` when the binary is available.

### New or changed capability

Trace the full chain: catalog/scope definition → enforcement/provider →
consumer/tool or app manifest. Update policy-facing tests and audit behavior in
the same change.

### LLM provider change

Cover configuration/setup, credential resolution, non-streaming probes,
streaming events, text/tool/reasoning round-trips, usage/error classification,
and provider-chain/pool behavior. A successful text-only request is not enough.

### Rootfs or package change

Decide separately whether the change belongs in:

- a reusable rootfs feature (`rootfs/features/`);
- an installed Debian package (`packaging/deb/`);
- a target profile (`scripts/lib/image-profiles.sh`); or
- target-only packaging (`targets/`).

If installed-system update behavior changes, update `docs/updating.md`.

## Agent Workflow

Before editing:

1. Select the subsystem from the task-routing table.
2. Read its entry point, direct callers, data types, and nearest tests.
3. Search for the same concept before adding helpers or configuration.
4. Check `git status` and preserve concurrent user work.

While editing:

1. Keep changes within the verified dependency direction.
2. Cover every coupled surface in the relevant change contract.
3. Prefer a natural module extraction over adding more responsibility to an
   already oversized dispatcher.
4. Run targeted validation after each coherent behavior change.

Before completion:

1. Re-run the exact requirement, not a proxy.
2. Run `git diff --check`.
3. Confirm only intended paths are staged.
4. Update architecture/navigation docs when boundaries, entry points, commands,
   or cross-surface contracts changed.

## Documentation Updates

| Change | Documentation to check |
| --- | --- |
| Build/test command or contributor workflow | `AGENTS.md`, `CONTRIBUTING.md` |
| Component, dependency direction, entry point, or data flow | `ARCHITECTURE.md` |
| App/manifest/SDK contract | `docs/app-development.md`, SDK README |
| Image feature/profile/identity | `docs/image-architecture.md`, `rootfs/features/README.md` |
| Package or installed update behavior | `packaging/README.md`, `docs/updating.md` |
| Desktop boundary or provenance | `desktop/README.md`, `desktop/PROVENANCE.md` |
