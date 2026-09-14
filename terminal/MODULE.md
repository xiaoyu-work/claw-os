# Terminal frontend source module

## Responsibility

Acquire and compile the real, pinned OpenAI Codex CLI/TUI for use as a private
Claw OS presentation frontend. This module does not implement another renderer,
an Agent runtime, a model provider, or canonical conversation persistence.
It is deliberately outside the root Cargo workspace.

## Key files

- [`source.json`](source.json): public Git revision, input hashes, Rust build
  recipe, and runtime attribution assets.
- [`build.sh`](build.sh): Linux/WSL entry point with explicit Linux HOME/PATH.
- [`build.py`](build.py): clean-source acquisition, locked upstream build,
  digest-verified patch replay, checksum-based source-cache synchronization,
  explicit target selection, and runtime staging.
- [`patches/`](patches/README.md): minimal recorded upstream changes for
  explicit-remote startup and presentation-only durable exit hints; no
  replacement rendering or Agent backend.
- [`artifacts.py`](artifacts.py): schema-v2 provenance, pin/recipe and ELF
  architecture/debug-section checks, attribution integrity, and fail-closed
  packaging input.
- [`staging.py`](staging.py): target-qualified GNU binutils selection and
  controlled debug stripping of staging copies only.
- [`test_build.py`](test_build.py): source-pin, clean-cache, extraction, and
  artifact/attribution staging and verification regressions.
- [`smoke.py`](smoke.py): real-binary PTY/WebSocket startup probe, stopping before
  backend work, using ephemeral auth and disabled update/OTEL exports; optional
  syscall tracing detects automatic Internet work.
- [`README.md`](README.md): build/packaging commands, the remote startup contract,
  child-only credential isolation, and remaining direct-cloud boundaries.
- [`PROVENANCE.md`](PROVENANCE.md): source origin, licensing, and update rules.

## Dependencies and boundary

The upstream workspace stays intact in the ignored `build/agent-tui` cache.
Its dependencies include Codex's core and app-server crates; compiling them is
not permission to use the Codex Agent backend. The Claw launcher must always
pass an explicit private Unix `--remote` endpoint owned by the Claw adapter.
Claw continues to own models, tools, capabilities, sessions, and persistence.
See the repository [architecture](../ARCHITECTURE.md).
Package consumers must preserve the complete bundle below `/usr/lib/cos/tui`;
the executable is `/usr/lib/cos/tui/bin/codex-tui`. Packaging verifies both the
source bundle and its copied staging root, without symlinks, duplicate binaries,
or post-receipt stripping.

## Validation

Build the actual artifact with `bash terminal/build.sh` from the repository
root on Linux. `--prepare-only` validates acquisition without compiling;
`--source /path/to/clean/codex` uses an explicit exact-pin development cache.
Packaging must pass `--target TRIPLE` to the builder, then run
`python3 -B terminal/build.py --verify-artifact --target TRIPLE`. The latter
does not build or search for fallback executables and emits JSON only on
successful verification. See the [packaging contract](README.md#packaging-api).
The builder also compiles/runs the pin's stdlib-only Rust exit-policy checks
before Cargo, covering flagged and unflagged presentation.
`--restage-verified --target TRIPLE` applies a staging-only policy change to a
verified same-source bundle without invoking Cargo; it cannot change source,
patches, compile options, or attribution identity.
Run builder regressions with
`python3 -B -m unittest discover -s terminal -p 'test_*.py'`.
After building, run
`unshare --user --map-root-user --net python3 -B terminal/smoke.py` to exercise
the binary without credentials or frontend Internet access. For a WSL checkout
on a Windows drive, add `--mount` to `unshare` and `--project-tmpfs` to the probe;
see the [README](README.md).
Inspect the generated build receipt and preserve all staged license files when
packaging. A successful build alone is not proof of Claw protocol feature parity.
