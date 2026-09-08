# MailExtension Module

## Responsibility

Thunderbird UI integration for the preparatory Mail AI foundation. The
[`mail-ai` App](../../apps/mail-ai/) owns shared AI business behavior and its
single manifest schema. The extension owns message extraction, compose-window
integration, UI state, and optional Thunderbird tag application; it does not
own provider credentials, model policy, or an independent Mail backend.

## Key files and boundaries

| Path | Responsibility |
| --- | --- |
| `background.js` | Thunderbird API integration, triage, metadata extraction, native forwarding |
| `lib/native.js` | Native Messaging connection and request correlation; no argument coercion |
| `lib/ui.js` | Popup-to-background messaging and UI helpers |
| `ui/` | Summary, reply/compose, translation, and supplied-context chat callers |
| `test_contract.py` | Node-executed UI request builders and native argument-preservation tests |

All six callers use the exact business names documented in
[`README.md`](README.md). Change callers and the App manifest/domain tests
together; do not introduce aliases or a second argument schema.
Native transport receives typed objects and calls the shared domain directly,
without manufacturing MCP callers or invoking another App.

## Validation

From the repository root on Linux/WSL (Python pytest and Node are required):

```bash
PYTHONPATH=claw-os-sdk/python/src:cos-runtime/python/src \
  python3 -m pytest -q apps/mail-ai/test_main.py extensions/claw-mail-ai/test_contract.py
```

These deterministic fixtures cover code contracts, not interactive Thunderbird
rendering, mailbox synchronization, or the later Mail data/consent migration.
