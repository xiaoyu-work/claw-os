# Maintained Codex frontend patches

The public upstream revision remains fixed in [`../source.json`](../source.json).
Its ordered `patches` entries identify every downstream change by path, SHA-256,
and purpose. The builder verifies each patch's bytes once, checks applicability,
and applies those same bytes to a fresh upstream export. A digest mismatch or
drift is fatal; there is no fuzzy fallback, unrecorded patch, or alternate TUI.

## Explicit-remote cloud startup

[`0001-remote-cloud-startup.patch`](0001-remote-cloud-startup.patch) changes only
`codex-rs/tui/src/lib.rs` and `codex-rs/tui/src/startup_orchestration.rs`:

- Skip announcement-tip preloading for an explicit remote app-server target.
- Return an empty cloud-config bundle before constructing the bootstrap local
  auth/cloud loader for that target.
- Skip the later local auth/cloud-loader reinitialization after config loading.

Embedded and implicitly discovered local-daemon startup retain their upstream
behavior. Rendering, widgets, editor/browser actions, pets, and the remote
protocol remain upstream code. This patch does not install another Agent
backend or create a network sandbox.

Modified source files carry a Claw OS change notice. Runtime attribution assets
preserve upstream `LICENSE` and `NOTICE`, include the exact applied patch, and
add a generated `MODIFICATIONS` record. The artifact verifier checks all of
these against the current pin.

## Durable Claw exit hints

[`0002-claw-exit-hints.patch`](0002-claw-exit-hints.patch) changes only exit
presentation. With child-only `COS_TUI_FRONTEND=1`, reconnect/continue hints use
`cos agent chat --session <presentation UUID>` rather than the private adapter
socket, which intentionally dies with its `cos` launcher. Stop guidance resumes
that conversation and then presses Esc. The Codex-only named-picker alternative
is not printed for Claw, whose session resolver accepts canonical or presentation
IDs rather than arbitrary names.

Missing or other marker values preserve upstream Codex command arguments, stop
shortcuts, colors, and exit formatting. The marker never selects a backend or
grants authority; explicit `--remote` and all Claw policy gates remain required.
The patch includes a stdlib-only production hint-policy module and regressions
for flagged/unflagged commands and stop text, run as pinned build checks.

## Maintenance and regression checks

Run `bash terminal/build.sh --prepare-only` from the repository root to replay
the patch through the builder. Do not edit cached upstream files by hand.
Update the pin's SHA-256 whenever the maintained patch changes.

`python3 -B -m unittest discover -s terminal -p 'test_*.py'` checks patch input
integrity, immutable application inputs, drift rejection, source-cache repair,
and packaged patch/notice integrity.

The pin's `checks.rust_stdlib_tests` entries are compiled with the pinned Rust
toolchain and run before the artifact build. They exercise the same presentation
policy module used by the TUI without starting an Agent, accessing credentials,
or compiling a separate dependency graph.

The real-binary smoke command in the [module README](../README.md) adds
`--check-no-cloud` to trace Internet connect/send attempts during explicit-remote
startup. Its inert access-token sentinel checks that startup does not need local
cloud-auth bootstrap. The probe stops before model turns and is not a claim
about user-triggered networking or complete backend feature parity.
