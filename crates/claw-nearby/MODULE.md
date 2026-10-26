# Nearby Transport Module

`claw-nearby` is an independent Rust workspace for the two fixed nearby
transport binaries. Its Owly/Tokio graph must not upgrade the Agent workspace.
The renderer-independent schema and private pipe protocol live in
`../clawd-client/src/nearby.rs`; the broker provider is
`../../core/src/clawd/nearby/`. The public service and four presentations are
specified in [`../../docs/nearby-sharing.md`](../../docs/nearby-sharing.md).

| Entry | Responsibility |
| --- | --- |
| `src/main.rs` | Non-root file-only AirDrop transport, explicit offer decisions, discovery, progress and bounded pipe messages |
| `src/radio.rs` | Root Owly radio/lease wrapper on the fixed system bus, without file-transfer parsing |
| `Cargo.lock` | Immutable dependency graph for both binaries and package builds |
| `PROVENANCE.md`, `OWLY-LICENSE` | Upstream revision, license and packaging adaptations |
| `../../packaging/deb/claw-os-agent/nearby/` | D-Bus-only activation, Root-only acquisition and interface policies |

The file worker receives only the current link and staged source assignments.
Package payloads use the GNU target, like the browser/PAM helpers: upstream
AirDrop's atomic `renameat2` binding is GNU-only in its current `nix` dependency.
Do not switch this independent crate to the core's musl target.
It cannot choose owners, capabilities, binaries, daemon endpoints or a
discovery root. It rejects URL/directory offers and never auto-accepts files.
Worker disconnect/expiry aborts work, not remote effects; received content
never becomes executable or Agent context automatically.

Run targeted checks from the repository root on Linux/WSL:

```sh
cargo check --manifest-path crates/claw-nearby/Cargo.toml --locked
cargo test --manifest-path crates/claw-nearby/Cargo.toml --locked
cargo clippy --manifest-path crates/claw-nearby/Cargo.toml --all-targets --locked -- -D warnings
```

Physical acceptance additionally requires a supported raw wireless card, the
appropriate upstream radio tools, Bluetooth wake where required, scoped host
firewall admission and an actual iPhone in Everyone mode. An ordinary WSL
virtual network interface does not meet that requirement.
