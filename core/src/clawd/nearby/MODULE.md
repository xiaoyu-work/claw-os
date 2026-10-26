# Nearby Sharing Provider

This provider implements the owner-scoped
[`nearby.status` / `nearby.control` contract](../../../../docs/nearby-sharing.md).
It does not depend on an Agent, model, Activity or display session.
[`clawd-client::nearby`](../../../../crates/clawd-client/src/nearby.rs) is the
definition; CLI, TUI, authenticated Web and native bridge are consumers.

| File | Responsibility |
| --- | --- |
| `mod.rs` | Closed request validation, human owner binding and exact existing capability/approval gates |
| `runtime.rs` | One shared radio owner/window, bounded transfer state, no replay, explicit decisions and durable-before-notification publication |
| `files.rs` | Owner-identity source access, protected-root checks, private immutable snapshots, chunked uploads, pinned non-overwriting inbox imports and staging retirement |
| `process.rs` | Fixed root-verified transport, mandatory user/PID/mount isolation, empty home, no bus, bounded cancellation-safe pipe records and resource limits |
| `radio.rs` | Fixed D-Bus API, unique service-owner pinning, finite lease, live health and checked release/expiry receipt |
| `bluetooth.rs` | Fixed anonymous AirDrop wake advertisement, without contact hashes, pairing or automatic adapter power changes |

Only Root owns `/var/lib/cos/nearby/<uid>`. Upload offsets and metadata are
synced before acknowledgement. Incoming/outgoing staging is private and retired
after checked containment; imported files remain owner-owned in
`~/Downloads/Claw Nearby/<transfer UUID>`. Partial import failure retains exact
affected paths, fails the window and never reports a completed receive.

Sender names, peer IDs and offered sizes are untrusted metadata. Actual file
limits are enforced independently. A peer ID is not identity or authority;
send approval binds the actual scoped link-local endpoint and canonical source
paths. One-shot approval identity includes the kernel peer and complete action.
An open receive window writes only its dedicated inbox after a separate exact
offer decision. App extension Hosts cannot use this human control surface.

The dedicated radio binary uses a small
[`owlyd` retirement fix](../../../../crates/claw-nearby/vendor/owlyd/PROVENANCE.md).
Successful `Release` and idle expiry receipts must mean `Backend::down` actually
completed. Failed startup recovery never advertises an idle service. Do not
remove this property when updating the upstream dependency.

Run from the repository root:

```sh
cargo test -p cos --lib nearby -- --test-threads=1
cargo test -p clawd-client --lib --locked
```

Four ignored temporary-file/namespace cases require Root; CI runs the exact
compiled test binary with `clawd::nearby:: --include-ignored --test-threads=1`
inside a private mount/network namespace. They never acquire a wireless radio.
Hardware acceptance is a separate target-machine/phone check, not a unit-test
or compilation claim.
