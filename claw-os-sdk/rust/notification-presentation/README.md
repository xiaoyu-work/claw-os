# Notification presentation contract

This OS-defined, renderer-independent App interface contains only versioned
rendering records, presentation mute preferences and optional private D-Bus
client declarations. It does not depend on a product, toolkit or authority
provider. The existing `rust-sdk` development export includes this companion
crate; consumers depend on `claw-notification-presentation`.

This definition alone does not activate a presenter. OS consumers and matching
Apps must use the same hardened contract.

Any authenticated presenter selected by the generic Host may implement it.
Selection is not exclusivity or permission. The protocol supplies no App-ID,
owner, session, capability or trust input. It opens no bus/socket and grants
no access. The Host supplies the existing private connection.

`muted` is an App-owned presentation preference, not OS DND/delivery policy.
Durable records, acknowledgement, delivery leases and authority stay with the
Notification Service. Cards and numeric handles are not durable records.
Markup parsing, file/image preparation and product settings stay in the App;
the host receives text runs and decoded RGBA8 pixels or symbolic alpha masks,
never encoded image data, sender-selected theme names or host paths. Images
are limited to 512 pixels per dimension and 1 MiB of decoded RGBA8 storage.
This prevents compressed images or embedded SVG references from selecting
decoder work or file access in the OS consumer.

See the [versioned protocol](../../wire/v1/notification-presentation.md).
Run from this directory:

```sh
cargo test --locked
cargo check --features dbus --locked
```
