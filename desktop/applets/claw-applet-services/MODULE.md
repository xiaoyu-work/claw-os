# Shared Applet Services

This GPL-3.0-only library owns the existing applet policy client and read-only
Calendar/task-list and telemetry providers, extracted from applet consumers.
It has no UI/App dependencies.

- `src/policy.rs`: bounded `cos __policy check` process, named/wild scopes,
  deny/error handling.
- `src/calendar.rs`: first check `data.db.read:Name(calendar)`, then read
  `COS_DATA_DIR/calendar/events.db` read-only, preserving timezone, overlap,
  sorting and empty/missing database behavior.
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

From the repository root:

```bash
python3 scripts/app_sources.py --native
cargo test --manifest-path desktop/applets/Cargo.toml -p claw-applet-services --lib
```
