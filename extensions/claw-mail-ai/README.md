# claw-mail-ai

A Thunderbird MailExtension that surfaces claw-os's local AI inside the
Thunderbird UI. Every model call routes through the host's `cos ai chat`
verb so capability gating, audit logging, and budget enforcement work
the same way they do for any other claw-os app.

The extension never embeds a model. It speaks **Native Messaging**
through the dedicated kernel `claw-mail-ai-host` launcher, which registers the restricted
`mail-ai` identity before starting `apps/mail-ai/native_host.py`.

## What it adds

- **Summary** popup (read pane / context menu) — TL;DR, key points,
  action items, citations, sentiment.
- **Smart Reply** (compose toolbar / context menu) — three drafts
  (formal / casual / short), insertable into the compose window.
- **Smart Compose** (compose toolbar) — full draft from a one-line intent.
- **Translate** popup (selection / message / context menu) — any → any.
- **Auto-triage** (optional, off by default) — categorise new mail with
  `claw/<category>` tags and an importance score.
- **Mail Assistant** in the Spaces rail — a chat UI that knows about
  recent inbox content.

All features are toggleable via the options page.

## Layout

```
manifest.json                  MV3, gecko id claw-mail-ai@claw.os
background.js                  event page; NM port; menus; triage listener
lib/native.js                  ClawNative — long-lived NM port wrapper
lib/messages.js                ClawMessages — body/thread helpers
lib/ui.js                      ClawUI — aiCall/showBusy/i18n helpers
lib/ui.css                     shared design tokens (emerald / amber / red)
ui/summarize/                  message-display-action popup
ui/compose/                    compose-action popup + composeScript.js
ui/translate/                  translate popup
ui/spaces/                     full-page Mail Assistant
ui/options/                    options page
_locales/en, zh_CN/            i18n
icons/icon.svg                 toolbar / spaces icon
```

## Native messaging wire format

```
{ "id": "<uuid>", "verb": "<verb>", "args": { ... } }
{ "id": "<uuid>", "ok": true,  "result": { ... } }
{ "id": "<uuid>", "ok": false, "error": "...", "detail": { ... } }
```

The host echoes the request id back so multiple in-flight calls don't
collide. Framing is the standard Chromium 4-byte LE length prefix +
UTF-8 JSON body, implemented in `native_host.py`.

## Verbs

All forwarded to `apps/mail-ai/main.py`:

| Verb            | Inputs                                     |
| --------------- | ------------------------------------------ |
| `summarize`     | `subject, from, to, date, body`            |
| `smart_reply`   | `thread, subject, from, my-intent, lang`   |
| `smart_compose` | `intent, to, subject, draft, style, lang`  |
| `translate`     | `text, target`                             |
| `triage`        | `subject, from, snippet, has_attachments`  |
| `chat`          | `history[], recent[], query, lang`         |

## Install (dev)

```sh
tools/install-mail-ai.sh
```

This copies the Python host into `/usr/lib/cos/apps/mail-ai`, drops the
Native Messaging manifest under `~/.thunderbird/native-messaging-hosts/`
(per-user) or `/etc/thunderbird/native-messaging-hosts/` (system), and
sideloads the extension as an unpacked add-on.

## Install (rootfs)

The `claw-mail-ai` rootfs feature packages the extension as a `.xpi`,
drops it under `/usr/lib/thunderbird/distribution/extensions/`, and
deploys the system-wide NM manifest and host script. See
`rootfs/features/claw-mail-ai/install.sh`.
