# mail-ai — Agent-side AI helpers for Thunderbird

The agent-driven half of the Mail AI feature. This app exposes a tiny
verb surface that the [`claw-mail-ai` Thunderbird extension](../../extensions/claw-mail-ai)
calls over Native Messaging. Every model call goes through the kernel's
AI gate (`cos ai chat`), so keys, monthly budget, safety pipeline and
audit log are uniform with the rest of `apps/`.

## Verbs

| Verb            | Purpose                                                                      |
|-----------------|------------------------------------------------------------------------------|
| `summarize`     | One-line summary + key points + action items + sentiment for a single email. |
| `smart_reply`   | Three reply drafts (formal / casual / short) for a thread.                   |
| `smart_compose` | Continue / complete a draft from a brief intent.                             |
| `translate`     | Translate email text into a target language.                                 |
| `triage`        | Classify an incoming email: category + tags + priority.                      |
| `chat`          | Grounded Q&A over a supplied list of recent emails.                          |

Two surfaces share these verbs:

1. **The Thunderbird extension** — via the dedicated kernel
   `claw-mail-ai-host` launcher, which registers the `mail-ai` App
   identity before starting
   `native_host.py` over Native Messaging.
2. **The cos CLI** — `cos app mail-ai <verb> …`, so the same logic is
   testable from the command line and reachable by other agents.

## CLI examples

```bash
# Summarize
cos app mail-ai summarize \
    --subject "Q3 plan" \
    --from "alex@example.com" \
    --body  "Hello, please review the attached plan and let me know …"

# Smart reply
cos app mail-ai smart_reply \
    --subject "Q3 plan" \
    --from "alex@example.com" \
    --thread "From: alex\n…\n\nFrom: you\n…"

# Smart compose
cos app mail-ai smart_compose \
    --to "alex@example.com" \
    --intent "ask Alex to push the deadline by one week" \
    --style formal

# Translate
cos app mail-ai translate --text "Bonjour, comment ça va?" --target English

# Triage
cos app mail-ai triage \
    --from "noreply@stripe.com" \
    --subject "Your receipt from Stripe" \
    --snippet "Thanks for your payment of \$12 …"

# Chat (RAG-style over supplied context)
cos app mail-ai chat \
    --question "Who proposed the budget cut?" \
    --context-json '[{"from":"alex","subject":"Budget","snippet":"…"}]'
```

## Native Messaging wire format

The extension speaks Mozilla's Native Messaging protocol (4-byte little-
endian length prefix + JSON body) over stdio:

```
→ host:  { "id": "<uuid>", "verb": "summarize", "args": { "body": "…", "subject": "Q3" } }
← host:  { "id": "<uuid>", "ok": true,  "result": { "summary": "…", … } }
← host:  { "id": "<uuid>", "ok": false, "error":  "<reason>", "detail": { … } }
```

`args` is a flat JSON object; underscores in keys are translated to
dashes, matching the CLI flags (e.g. `has_attachments → --has-attachments`).

## Deployment

System-wide install lives at `rootfs/features/claw-mail-ai/`. It drops:

- `/etc/thunderbird/native-messaging-hosts/os.claw.mail_ai.json`
  — the manifest Thunderbird reads to find this host. The
  `allowed_extensions` array pins the extension ID
  `claw-mail-ai@claw.os`.
- `/usr/lib/cos/apps/mail-ai/{native_host.py, main.py, _lib/…}`
  — the canonical verified copy of this app. The root-owned
  `/usr/lib/cos/claw-mail-ai-host` binary invokes it.
- `/etc/thunderbird/policies/policies.json`
  — `ExtensionSettings` that pins the extension as system-installed
  and non-removable, and disables Mozilla telemetry.

For local dev, see `tools/install-mail-ai.sh`.

## Why route through cos ai chat

App developers never see provider SDKs, model names, or API keys.
The machine owner configures one provider in `/etc/cos/agent.toml`;
every app's call uses that. External email content is authorized with
`ai.chat.untrusted`; `claw_os_sdk.ai.chat(origin="external-content")`
is the only sanctioned path.

This means the same extension code runs against Claude, GPT-4o, a
local Llama 3, or any future provider, without any code change inside
this app.
