# Notification presentation v1

This is an OS-defined App presentation contract, not a product configuration
schema, plugin flag, exclusive provider registration or authority API.
The [Rust companion crate](../../rust/notification-presentation/README.md)
contains renderer-independent records and optional D-Bus client declarations.
Other languages can use the same documented JSON and D-Bus signatures.
Existing CLI/MCP wire schemas are unchanged.

## Transport and ownership

The Host supplies a private connection to its selected presenter. The existing
`PANEL_NOTIFICATIONS_FD`, `DAEMON_NOTIFICATIONS_FD` and `COSMIC_NOTIFICATIONS`
descriptor names retain their values for compatibility. The panel's existing
`com.clawos.NotificationsSocket.GetFd` handoff is unchanged. The new interface
on each private applet connection is `com.clawos.NotificationPresentation1`,
at `/com/clawos/NotificationPresentation1`.

No client opens a new bus/socket, selects an App, supplies a publisher/App ID,
or gains authority from this protocol. Any appropriately hosted App can
implement it. Default selection is not exclusivity. The ordinary authenticated
Host, capability and resource-owner contracts still apply.

The App owns popup UI, markup parsing, image preparation, theme choices and
product settings. The OS panel consumes only rendering records and a generic
presentation mute. Numeric handles belong to the existing private connection,
not a durable ID or authorization token. Existing sender checks, OS durable
notification state, DND/delivery policy, leases and acknowledgement are unchanged.

Presentation content, action clicks, printed JSON, `contract_digest` and
`--yes` are not first-review proof or permission decisions. Consistent
terminal/desktop review, protected state and per-App allow-within-scope,
ask/deny controls belong to the OS review/controller contract across GUI,
MCP and background execution. App purposes come from verified `needs[].why`;
this interface adds no purpose or plugin flag. It exposes no permission
toggle or revocation claim.

## Methods and signals

| Member | D-Bus signature | Meaning |
| --- | --- | --- |
| `Version` | `() -> u` | Must return 1 |
| `Preferences` | `() -> s` | JSON preference snapshot |
| `SetPreferences` | `s -> s` | Validate and persist the App-owned presentation mute; return a snapshot or explicit error |
| `Dismiss` | `u -> ()` | Existing human dismissal of a nonzero presentation handle |
| `InvokeAction` | `us -> ()` | Existing human action for a nonzero handle and bounded opaque action string |
| `Card` signal | `s` | JSON rendering record |
| `PreferencesChanged` signal | `s` | JSON preference snapshot |
| `Failure` signal | `s` | Bounded plain-text projection error, not an empty successful record |

Consumers subscribe before fetching preferences, preserve the last confirmed
value on failure, and do not enable the toggle before successful negotiation.
Loss of the private connection retires the control channel and disables the
popup-mute toggle; it never implies that a mute or permission revocation occurred.
Calls have a three-second consumer deadline; the App's bounded mutation queue
has a one-second deadline. An incompatible or missing presenter is an explicit
error, never a fallback to App configuration files.

## Preferences and compatibility

```json
{"version":1,"muted":false}
```

Only these fields are accepted; the JSON limit is 256 bytes. The current
Notifications App maps `muted` to its existing `do_not_disturb` key in
`com.clawos.Notifications/v1`. A missing key keeps the existing App default;
malformed settings and failed writes are errors. A change writes only that
key, never anchor, maximum count, timeout, theme or other product preferences.
The App broadcasts external configuration changes as well.

**No DND/data migration occurs.** The existing panel label "Do Not Disturb"
still controls the same additional popup mute. It does not replace the
Notification Service's durable DND schedule or delivery policy. Installation,
permission review and user approval remain separate OS responsibilities.

The App keeps its legacy private `com.clawos.NotificationsApplet` methods and
notification signal for older hosts. The updated OS consumer requires
presentation v1. Deployment must supply a compatible selected presenter;
the old App-owned config/util build artifact is not a compatibility fallback.

## Rendering records and bounds

```json
{"version":1,"id":7,"source_label":"Example","title":"Title","body":[{"text":"Body","bold":true,"italic":false,"underline":false,"accent":false}],"activation":"default","icon":{"kind":"mask","width":1,"height":1,"alpha":[255]},"group_icon":null}
```

All object/enum shapes are closed. The version is 1; IDs are nonzero unsigned
32-bit integers. `source_label` is display text, not producer identity.
The title and combined text runs are each limited to 65,536 UTF-8 bytes,
the label to 1,024 bytes, and the body to 4,096 runs. Run style flags default
to false. They request bold, italic, underline or the current accent role,
not executable markup or host-supplied product parsing.

`activation` is null or an opaque action string of at most 4,096 bytes.
The App preserves its original default/first-action choice. `icon` and
`group_icon` independently preserve row/group precedence. Each is null or:

- `{"kind":"rgba","width":1,"height":1,"pixels":[0,0,0,255]}`: straight-alpha,
  row-major RGBA8; exactly `width * height * 4` bytes.
- `{"kind":"mask","width":1,"height":1,"alpha":[255]}`: row-major alpha coverage;
  exactly `width * height` bytes. The OS renders this with the inherited icon
  foreground, preserving symbolic icons across themes and button states.

Each dimension must be 1 through 512. Both forms are limited to 262,144 pixels
and at most 1 MiB when materialized as RGBA8. A mask therefore contains at most
262,144 bytes. The complete JSON record is limited to 10 MiB. These are decoded
dimension/work bounds, not compressed-file limits. Pixel bytes are never
interpreted as SVG, PNG, another encoded format, a resource name or a URI.

The App resolves theme icons and prepares file/vector/raster images before
transmission. The OS consumer neither performs sender-selected theme lookup
nor invokes an image decoder for these records. The `encoded` and `theme`
variants from the earlier unpublished draft are refused, including in
`group_icon`; there is no alternate parser or compatibility fallback.
Rebuild the presenter and consumer together before publishing this v1 contract.

Malformed or excessive images produce an explicit projection error, not an
empty successful image. An absent optional icon is still valid. The legacy
popup path and durable record are not silently deleted or acknowledged.
These limits are an explicit new wire boundary, not a claim of unrestricted
legacy input compatibility. No preferences or user data are migrated.
