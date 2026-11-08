# Owner-bound messaging ingress

Wire v1 supports exactly Discord, Telegram, Slack and WhatsApp. The system
broker owns receiver lifetime, credentials, owner/provider/sender bindings,
cursor state and submission receipts. Apps do not submit Agent tasks.

An authenticated human first creates a bounded Activity and its ordinary
generation-bound trigger. The trigger must match `channel:gateway-<platform>`
and `message.received` exactly. Normal scheduler consent, capabilities,
execution limits and the existing Job queue apply; channel configuration
neither creates a trigger nor grants Agent authority.

`cos channels configure <platform>` reads an object on stdin containing only
`account_id`, `channel_id`, `sender_id` and `trigger_id`. This crosses the
protected scheduler approval path for the exact channel, provider host and
fixed owner credential names. Root verifies provider account credentials.
There is one profile per owner/platform, one exact sender/channel per profile,
and at most 32 profiles. Telegram bots cannot share destructive cursors across
owners. Credential fingerprints bind the provider verification; rotating any
referenced credential requires an explicit stopped-profile reconfiguration.
Discord's existing App-local `configure` settings are not admission;
they and previous App data are not migrated.

| Provider | Account binding | Channel | Sender | Owner credential names |
| --- | --- | --- | --- | --- |
| Discord | bot user ID | exact DM/guild/thread channel ID | exact user ID | `discord_bot_token` |
| Telegram | bot ID | exact private/group chat ID | exact user ID, not just group ID | `telegram_bot_token` |
| Slack | `team_id/api_app_id` | exact channel ID | exact user ID | `slack_bot_token`, `slack_signing_secret` |
| WhatsApp | business account ID | sending phone-number ID | exact phone digits or complete BSUID | `whatsapp_access_token`, `whatsapp_phone_number_id`, `whatsapp_app_secret`, `whatsapp_verify_token` |

Discord and Telegram use bounded authenticated provider polling. Initial
activation establishes an inert latest-message baseline, not a historical
backlog. Discord requires the bot's normal channel/history/message-content
access; this is not full Gateway guild discovery. Telegram requires no other
poller/webhook consuming that bot. Reconfiguration does not erase replay state.

Slack and WhatsApp receive signed requests at
`http://127.0.0.1:9743/channels/<owner-uid>/<platform>`. An operator must expose
that exact route through a public HTTPS reverse proxy, preserving body bytes
and signature headers. No App-selected URL, executable, listener port or
unsigned producer is admitted. Slack verifies raw-body HMAC and a five-minute
timestamp; WhatsApp verifies raw-body HMAC and the separate GET verify token.
Workspace/App and business-account/number IDs are independently checked.
Only ordinary text messages are admitted, never bot messages, edits, status
notifications or provider attachments.

The public SDK's `channels.control(action, ...)` calls
`system.channel.control` on bounded stdin using its live inherited session.
Root derives the exact connector App and non-root owner. Every action requires
`ipc.publish:Name(channel:gateway-<platform>)`; callers cannot choose a provider,
owner, recipient, source, credentials, capabilities or prompt.
Actions are `status`, `start`, `stop`, and `reply`. Human `cos channels status`
and `cos channels stop` remain available without an App grant.

Start/stop govern OS receiver admission, independent of a model process.
Stop prevents later admission and reports pending evidence/replies; it does
not undo an already-dispatched provider request or cancel existing Agent Jobs.
Reply requires `event_id`, `text` and `confirm: true`. Root resolves the
recipient/account/thread from the original owned, provider-verified event,
checks the current binding and 24-hour window, and disables Discord mentions.
Discord replies are at most 2000 characters; other replies at most 4000.
WhatsApp BSUID replies preserve the complete opaque `recipient` value.

New messages are bounded to five per minute per profile, with a durable counter
and five-minute freshness window. Text is clipped at 8 KiB with an explicit
`truncated` field. A pending event is synced before journal publication and its
cursor advances only afterward. Exact IDs/payloads deduplicate across restart;
changed data conflicts. Partial/corrupt progress is preserved and refused, not
reset. Events enter the ordinary context journal as `provider_verified`, with
owner UID and a stable `channel-event-...` ID, and wake the existing scheduler.
Event text stays fenced untrusted data, not the rule's owner-authored prompt.

Reply receipts are synced in-flight before the external POST. A repeated
identical completed request returns the retained result; changed text or an
uncertain submission never resends automatically. A submission ID is not
delivery confirmation. State is bounded to 2 MiB/profile and 4096 retained
reply receipts; the shared event journal retains its 32-MiB bound. Capacity,
corruption, unavailable credentials and receiver failures are explicit holds,
not permission, replay, compaction or unconfined-process fallbacks.

Synthetic protocol/Job tests do not establish live provider delivery, public
HTTPS deployment, native UI acceptance or installed-system publication.
