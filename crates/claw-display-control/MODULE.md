# OS Display Control

This unpublished internal crate defines the bounded, credential-authenticated
descriptor protocol shared by the Root login/display supervisor, PAM hook,
broker and compositor. It is not part of the public App SDK and makes no
permission decisions.

`identity.rs` reads kernel process/start-time and audit-login identities.
`transport.rs` requires Unix `SOCK_SEQPACKET`, per-message kernel credentials,
close-on-exec descriptor receipt, closed messages, deadlines and exact FD counts.
`wire.rs` separates login activation, PAM lifetime and compositor commands.
`install.rs` checks fixed executables and every ancestor. `locale.rs` admits
only bounded PAM locale values, never loader variables or authority metadata.
`session.rs` provides read-only kernel-login subscriptions and canonical
display/runtime/user-bus projection.
Owner, session and epoch fields in compositor messages are Root projections;
no user registration message may supply them.

Production consumers must bind an expected process using their Root-owned
child/parent or kernel peer, not a PID parsed from App JSON. Inherited socket
peer credentials alone are insufficient after fork: every received packet
checks its actual sender and current process identity.

The transport does not enable GUI permissions by itself. Its production
[Root activation](../../core/src/display_session/MODULE.md),
[GUI Host](../../core/src/clawd/gui/MODULE.md) and
[compositor](../../desktop/comp/MODULE.md) consumers are separate responsibilities.
No user socket, public SDK call or serialized owner claim creates a lease.

Run `cargo test -p claw-display-control -- --test-threads=1` from the repository
root; descriptor-leak cases count process-wide descriptors.
