# Extensions Module

## Purpose

Browser and Mail integration sources now live in
[`clawos-app`](https://github.com/xiaoyu-work/clawos-app). This directory retains
OS-side navigation for their installation and authority boundaries.

## Responsibilities

- Package optional integration logic at a clear external boundary.
- Communicate through stable SDK, CLI, or protocol surfaces.
- Keep credentials and provider policy owned by core services.

## Key Files

| Path | Role |
| --- | --- |
| [`Browser product`](https://github.com/xiaoyu-work/clawos-app/tree/main/products/browser) | Attached-browser App, MV3 extension and Native Host source |
| [`Mail product`](https://github.com/xiaoyu-work/clawos-app/tree/main/products/mail) | Mail AI host/extension source |
| `../tools/install-browser-agent.sh` | Manual Browser extension installer using the OS App source pin |
| `../rootfs/features/claw-mail-ai/` | Installed-package Mail integration |

## Dependencies

Extensions depend on public contracts and cannot bypass capabilities, consent,
budgets, or audit. Changes coordinate with the matching core/app host surface.

## Tests

Use each extension manifest/package test command and the affected core/app
integration tests.
