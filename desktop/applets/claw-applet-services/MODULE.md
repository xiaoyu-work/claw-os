# Shared Applet Services

This GPL-3.0-only library owns the existing applet policy client and read-only
Calendar provider, extracted from Widget Rail. It has no UI/App dependencies.

- `src/policy.rs`: bounded `cos __policy check` process, named/wild scopes,
  deny/error handling.
- `src/calendar.rs`: first check `data.db.read:Name(calendar)`, then read
  `COS_DATA_DIR/calendar/events.db` read-only, preserving timezone, overlap,
  sorting and empty/missing database behavior.
- `test/unit/`: original provider/policy tests plus exact named-scope coverage.

Widget Rail re-exports the library without changing its consumers. The OS
`cosmic-applets` dispatcher translates events to the product-owned Calendar
UI's `AgendaProvider` contract. No App invokes another App, and no grant or
data partition is broadened or combined.

From the repository root:

```bash
python3 scripts/app_sources.py --native
cargo test --manifest-path desktop/applets/Cargo.toml -p claw-applet-services --lib
```
