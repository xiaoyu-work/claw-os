# Wire protocol v1

The wire protocol is the **stable contract** between every `claw-os-sdk`
language binding and the `cos` kernel binary. It is what every SDK
ultimately speaks; what users call from their app code is a
language-idiomatic wrapper over this protocol.

## Stable AI surface

Wire v1 exposes text chat only: `ai.chat` and the hardened
`ai.chat.untrusted` variant selected by `origin=external-content`.
Embed, image, vision, audio, and video selectors are experimental and
currently unsupported; compatibility helpers fail before invoking `cos`.

## Transport

**v1 uses a subprocess transport.** A request is encoded as `cos`
command-line arguments + (optionally) stdin JSON; the reply is a JSON
**envelope** on stdout. Exit code 0 means "the request reached the
gate"; non-zero means "the gate or kernel rejected the call before
dispatch" — and even then the body on stdout (or stderr) is a JSON
envelope describing the error.

Reasoning:
- Identity and audit-trail come from process ancestry. The `cos`
  binary verifies that its parent is an app the kernel itself launched.
- Subprocess transport works on every OS without setting up sockets,
  named pipes, or daemons.
- The same envelopes will flow over v2 (Unix socket + length-prefixed
  framing). Switching transports is an SDK-internal change.

## Envelope

Every reply is one JSON object that conforms to `envelope.schema.json`.

Successful reply:
```json
{
  "ok": true,
  "data": { /* request-specific shape, see {perms,ai,tool,app}.schema.json */ },
  "audit_id": "01J…",
  "wire_version": 1
}
```

Error reply:
```json
{
  "ok": false,
  "error": "permission denied",
  "code": "PERMISSION_DENIED",
  "detail": { /* request-specific shape */ },
  "audit_id": "01J…",
  "wire_version": 1
}
```

`code` is a stable string drawn from `error_codes.md`. New codes may
be added in v1; existing codes keep their meaning.

> **Compatibility note.** The current `cos` binary (kernel v0.3.x) does
> **not** yet wrap its replies in this `{ok, data}` shape — each
> sub-command returns its own ad-hoc envelope (e.g. policy checks return
> `{"decision": "allow"|"deny", …}`). The v1 wrapping is the
> *target* protocol; SDKs read the existing flat shape and normalise
> it through `envelope.rs` / `envelope.py` etc. so user code already
> sees the v1 surface. The kernel will be migrated to emit v1 wrappings
> natively in a follow-up; SDK behaviour will not change.

## Request families

| Family   | Route                                   | Schema                  |
|----------|-----------------------------------------|-------------------------|
| `policy` | internal capability check              | `perms.schema.json`     |
| `ai`     | `cos ai chat --app <id> [...] `         | `ai.schema.json`        |
| `tool`   | `cos ai tool <name> --app <id> --args <json>` | `tool.schema.json` |
| `app`    | `cos app <id> <verb> [...]`             | `app.schema.json`       |

App manifests (`app.json`) are validated against `manifest.schema.json`.

## Error codes

See `error_codes.md` for the canonical list. The minimum:

| Code                  | Meaning                                                    |
|-----------------------|------------------------------------------------------------|
| `PERMISSION_DENIED`   | Caps gate refused the call.                                |
| `BUDGET_EXCEEDED`     | App's AI budget is exhausted for the current period.       |
| `SAFETY_VIOLATION`    | Safety pipeline blocked the request (prompt injection etc.). |
| `UNKNOWN_APP`         | App id not installed.                                      |
| `UNKNOWN_VERB`        | App doesn't expose this verb / catalog tool not found.     |
| `INVALID_ARGS`        | Args failed schema validation.                             |
| `KERNEL_UNAVAILABLE`  | `cos` binary couldn't reach a required subsystem.          |
| `INTERNAL_ERROR`      | Anything else; details in `detail.message`.                |

## Versioning

The protocol version is announced in three places:

1. `wire_version` field in every reply envelope.
2. `cos --version --wire` prints the supported wire version(s).
3. The `claw-os-sdk` library handshakes on startup and refuses to
   continue against an unsupported kernel.

Breaking changes bump the wire version. Bug fixes in the kernel that
don't change the envelope shape do not.
