# claw-mail-ai

A Thunderbird MailExtension using the shared
[`mail-ai` business implementation](../../apps/mail-ai/). The system Agent's
authenticated MCP tools call that same implementation. Model calls use the
public SDK AI gate, keeping capability checks, consent, audit, and budgets
owned by core.

This is the preparatory M1 Mail foundation, not a unified mailbox or a completed
Mail fork. The App identity remains `mail-ai`; future mailbox ownership and
data/consent migration are covered by the
[product redesign plan](../../docs/app-product-redesign.md).

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
| `summarize`     | `body, subject, sender, lang`               |
| `smart_reply`   | `thread, subject, sender, intent, lang`     |
| `smart_compose` | `intent, recipient, subject, draft, style, lang` |
| `translate`     | `text, target`                             |
| `triage`        | `subject, sender, snippet, has_attachments` |
| `chat`          | `question, context_json, lang`             |

Argument names are exact: there are no aliases or underscore-to-hyphen
translations. `has_attachments` is a boolean; the other arguments are strings.
Chat serializes recent message metadata to `context_json` as an array of
`{sender, subject, date, snippet}` objects (fields are optional strings) and
renders the returned `answer`. Its conversation history remains local UI
state, not model context. A failed metadata read is shown to the user rather
than silently asking the model without context.

## Install (dev)

First build and install the matching `claw-os-agent` package using the normal
[packaging workflow](../../packaging/README.md). It supplies the canonical App
at `/usr/lib/cos/apps/mail-ai`, shared SDK/runtime at `/usr/lib/cos/python`,
root-owned `/usr/lib/cos/claw-mail-ai-host` launcher, and matching extension at
`/usr/lib/thunderbird/distribution/extensions/claw-mail-ai@claw.os.xpi`.
Shipping both protocol peers in one package keeps their argument contracts
aligned across APT upgrades without requiring Thunderbird for headless use.
Then, from the repository root:

```sh
tools/install-mail-ai.sh
```

The installer requires those matching installed components and only registers
the native host and Thunderbird policies. It reuses the package-owned XPI;
it never regenerates or shell-copies the UI from source, overwrites App/SDK
files, or substitutes a custom host binary. There is no
`CLAW_MAIL_AI_HOST_BIN` fallback. Changes to either the business implementation
or extension UI take effect after rebuilding/reinstalling the package, not
after running the shell installer. This keeps package provenance intact.

## Install (rootfs)

The `claw-mail-ai` rootfs feature deploys the system-wide Native Messaging/policy
integration. It reuses the App, SDK/runtime, launcher, and protocol-matched XPI
supplied by the installed `claw-os-agent` package; it neither regenerates the
extension nor copies or overwrites the host script or another executable App
tree.
See [`rootfs/features/claw-mail-ai/install.sh`](../../rootfs/features/claw-mail-ai/install.sh).

## Tests

From the repository root on Linux/WSL:

```bash
PYTHONPATH=claw-os-sdk/python/src:cos-runtime/python/src \
  python3 -m pytest -q apps/mail-ai/test_main.py extensions/claw-mail-ai/test_contract.py
```

The existing pytest runner drives Node to execute the real UI request
builders with deterministic Thunderbird stubs. Native/MCP process tests use
injected AI and capability effects; no Thunderbird UI or model is required.
