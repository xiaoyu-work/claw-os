# App events

`events.publish` reports bounded App-owned data through the ordinary authenticated
session and `system.app-event.publish`. Root derives the owner, App ID, source
`app:<id>`, task and session. No request may select an owner or another source.
The exact existing capability is `ipc.publish:Name(app:<id>)`.

The request has `event_type`, an App-scoped stable `key`, optional `entity_id`
and optional JSON `payload`. Business data travels on stdin, never argv.
Payloads are at most 16 KiB; complete requests are at most 20 KiB.
The source is caller-reported, not a mutation attestation or a grant.

Identical keys and reported data return the original durable event. Different
data conflicts. A partial/corrupt journal is preserved and cannot be replayed
or reset by a client. Publication does not itself create an Agent task:
the owner's existing authorized trigger/work rule controls admission.
Trigger event fields remain fenced untrusted data.
