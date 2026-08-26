# Claw OS Web Desktop

The GitHub Pages root is a complete browser-based Linux desktop imported from
[`xiaoyu-work/linux_web`](https://github.com/xiaoyu-work/linux_web). The public
entry opens the desktop immediately, without an operating-system picker, boot
sequence, or login gate.

The application grid mirrors the first-party Claw OS desktop: **Claw OS
Agent**, **Files**, **Text Editor**, **App Store**, **Media Player**,
**Screenshot**, and **Settings**. Their icons come from the native desktop
components. Template Help, Terminal, and Games entries are not exposed.

A blocking first-run spotlight guides visitors into **Claw OS Agent** and
through one complete approval-gated task. The Agent then exposes six reusable
demos—system health, crash explanation, cross-app workflows, shared AI models,
memory/history, and app access—plus a freeform AI chat surface.

The **Claw OS Website** desktop shortcut opens `public/site/` in the built-in
browser. That directory contains the existing agent-native marketing website,
so the root URL remains entirely inside the desktop experience.

From the repository root:

```bash
npm ci --prefix web --replace-registry-host=always
npm run dev --prefix web
npm run build --prefix web
```

Production files are generated in `web/dist/`. The Pages composition workflow
combines this independent web artifact with the signed APT repository artifact
without including either one in the other.
