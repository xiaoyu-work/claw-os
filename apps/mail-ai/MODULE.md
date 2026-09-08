# Mail AI Module

## Purpose and boundaries

Preparatory M1 Mail business layer, still identified as `mail-ai`.
[`README.md`](README.md) describes the contract and limits; the
[product plan](../../docs/app-product-redesign.md) owns the later Mail merger.
This module does not own a mailbox, provider credentials, or App orchestration.

## Key files

| File | Responsibility |
| --- | --- |
| `main.py` | Six typed business functions, shared validation, AI shaping, narrowly scoped memory |
| `app.json` | Sole MCP argument/capability schema and optional human CLI binding metadata |
| `server.py` | Public SDK authenticated MCP binding and MCP result encoding |
| `native_host.py` | Canonical isolated bootstrap and strict Native Messaging transport |
| `test_main.py` | Business, manifest, authority, real process/framing and ingress parity tests |

Native Messaging and MCP share `main.py`, not one another's dispatchers.
Validate untrusted input before `ai.chat.untrusted`; keep AI budget/safety
handling and `mail-ai` memory scope aligned with the manifest. Native callers
must never construct MCP metadata or use SDK-private authentication methods.
Core owns launcher trust, verified App sessions, owner identity, and consent.

## Coupled surfaces and tests

Argument changes must update all
[`claw-mail-ai`](../../extensions/claw-mail-ai/) callers and their contract
tests in the same change. Installed startup changes also require coordination
with the core launcher, rootfs feature, and development installer.

From the repository root:

```bash
PYTHONPATH=claw-os-sdk/python/src:cos-runtime/python/src \
  python3 -m pytest -q apps/mail-ai/test_main.py extensions/claw-mail-ai/test_contract.py
```
