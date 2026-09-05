# Wire protocol v1

The wire protocol is the **stable contract** between every `claw-os-sdk`
language binding and the `cos` kernel binary. It is what every SDK
ultimately speaks; what users call from their app code is a
language-idiomatic wrapper over this protocol.

## Stable AI surface

Wire v1 exposes text chat only: `ai.chat` and the hardened
`ai.chat.untrusted` variant selected by `origin=external-content`.
Unsupported modalities are not published as placeholder SDK APIs.

## Transport

**v1 uses a subprocess transport.** A request is encoded as
`cos --wire=1` plus command-line arguments and, optionally, stdin JSON. The
reply is exactly one JSON **envelope** on stdout. Exit code 0 accompanies a
success envelope; non-zero accompanies an error envelope. SDKs reject flat
command output, JSON on stderr, malformed envelopes, unsupported versions, and
an exit-status/`ok` mismatch.

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

## Request families

| Family   | Route                                   | Schema                  |
|----------|-----------------------------------------|-------------------------|
| `policy` | OS-internal capability check; no public SDK API | `perms.schema.json` |
| `ai`     | `cos ai chat --app <id> [...] `         | `ai.schema.json`        |
| `tool`   | `cos ai tool <name> --app <id> --args <json>` | `tool.schema.json` |
| `app`    | `cos app <id> <verb> [...]`             | `app.schema.json`       |

The `policy` wire family is consumed only by the OS-internal
`cos-runtime` package. It is not an importable API in any public SDK;
third-party SDK calls are capability-checked by the `cos` kernel.

App manifests (`app.json`) are validated against `manifest.schema.json`.

## Private App MCP calls

MCP-first Apps serve their manifest-declared tools over private stdio owned
by the Claw App Host. For every `tools/call`, the Gateway replaces any
caller-supplied Claw metadata and injects a value conforming to
`mcp_call_context.schema.json` under
`_meta["claw-os.dev/call-context"]`. It binds the authenticated workload
principal to the call/trace correlation, owner, task/session, and
deadline. This context is descriptive, not authority; the App Host retains
the transient target capability grant.

Caller kinds are `system-agent`, `external-agent`, and `cli`. App principals,
caller `app_id`, `parent_call_id`, and `depth` are rejected, not translated.
Apps cannot call other Apps; cross-App orchestration belongs to the system
Agent. The manifest access object accepts only `system_agent` and
`external_agents`.

The Python runtime exposes that value through `claw_os_sdk.mcp.current_context()`
and supports MCP progress tokens plus cooperative
`notifications/cancelled`. MCP-first runtimes reject calls without a valid
Gateway context.

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

1. The SDK requests a version explicitly with `cos --wire=1`.
2. `wire_version` in every reply must equal the requested version.
3. `cos --wire=1 --version` provides a transport-level compatibility probe.

Breaking changes bump the wire version. Bug fixes in the kernel that
don't change the envelope shape do not.
