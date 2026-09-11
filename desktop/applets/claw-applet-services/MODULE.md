# Shared Applet Services

This GPL-3.0-only library owns the existing applet policy client and read-only
Calendar/task-list and telemetry providers, extracted from applet consumers.
It has no UI/App dependencies. It is an independent Cargo workspace: default
features retain the trusted OS library; `provider` adds the public SDK protocol
dependency and `/usr/libexec/claw-os-applet-provider` executable.

- `src/policy.rs`: bounded `cos __policy check` process, named/wild scopes,
  typed deny/error handling and exact verb/scope affirmation.
- `src/command.rs`: bounded subprocess stdout/stderr, deadlines and checked
  kill/reaping. The installed provider fixes `/usr/local/bin/cos`; the original
  `COS_BIN` test/development override remains only in the default OS library.
- `src/service.rs`, `src/bin/claw-os-applet-provider.rs`: closed v1 dispatch,
  independent per-operation checks, bounded framing and nonblocking private
  Linux pipes. No blocking stdin worker can outlive a framing deadline.
- `src/calendar.rs`: first check `data.db.read:Name(calendar)`, then read
  `COS_DATA_DIR/calendar/events.db` read-only, preserving timezone, overlap,
  sorting and empty/missing database behavior; streaming selection, one-MiB
  native-record/result bounds, 250 ms busy timeout and a three-second query
  deadline fail explicitly instead of truncating or accumulating all rows.
- `test/unit/`: original provider/policy tests plus exact named-scope coverage.
- `src/tasks.rs`: shared read-only `cos agent ls` records and bounded async
  listing; `observe()` first checks `agent.observe:Name(tasks)`.
- `src/system.rs`: first check `sys.observe:Wild`, then sample CPU/network and
  bounded `cos sys resources`, preserving the existing Linux fallback and
  process-local delta state. Original telemetry tests remain alongside it.

The OS `cosmic-applets` dispatcher translates events to the product-owned
Calendar UI's `AgendaProvider` contract and Calendar/task/system records to
Widget Rail's `Providers` contract. Agent Activity re-exports only the shared
task-list adapter, retaining its own UI, detail/mutation commands and tests.
No App invokes another App, and no grant or data partition is broadened or combined.
The Clipboard host also uses this policy client, mapping the product library's
typed read/write requests only to `Name(history)`. CopyQ implementation belongs
to the external Clipboard product, not this library or Widget Rail.

Standalone clients consume only the
[public SDK contract](../../../claw-os-sdk/wire/v1/applet-services.md).
`history-check` remains a preflight probe; the provider does not execute CopyQ,
open Wayland, establish a login/GUI instance or revoke raw resources. The Host
must establish the authenticated grant and owner-scoped data view. A caller
environment is not independently attested by this library. Provider sources
and binaries are excluded from the public development artifact.

`../justfile` independently builds/installs the helper and syncs its lock during
vendoring. The Desktop package advertises `claw-os-applet-services-v1 (= 1)`
only with a regular, executable, correctly targeted ELF. Existing linked shell
and App installation recipes are not cut over by this source slice.

From the repository root:

```bash
cargo test --manifest-path desktop/applets/claw-applet-services/Cargo.toml --lib --locked -- --test-threads=1
cargo test --manifest-path desktop/applets/claw-applet-services/Cargo.toml --features provider --locked -- --test-threads=1
just --justfile desktop/applets/justfile build-applet-provider --release
```

`tests/provider_process.rs` uses the actual public client and provider with a
synthetic kernel mounted only inside fresh user/mount namespaces. It covers
per-operation denial, complete wire conformance, output bounds, real deadlines,
read-only data and fixed executable selection. `tests/provider_lifecycle.rs`
isolates orphan adoption in its own process and proves cancellation terminates
the provider and active policy child, not GUI retirement.
For exact installed-candidate checks, `CLAW_APPLET_TEST_PROVIDER` selects an
absolute built ELF in these test fixtures only; the installed provider never
consults this variable.
`tests/kernel_policy.rs` is a separately invoked real-kernel fixture:
`test/support/run_kernel_fixture.py --help` describes its built-binary inputs.
The Root fixture creates private mount/network namespaces and runtime/data
roots; each SDK client runs as the non-Root source owner with `no_new_privs`.
It creates no production grant or package trust. Missing authenticated App
provenance remains a denial, not a fixture bootstrap.
