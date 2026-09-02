# cos agent serve — web UI

This is a Vite + React 18 + Tailwind v4 + Radix UI single-page app
that gets compiled into the cos binary by include_dir!. The visual
language is ported verbatim from open-agents/apps/web (oklch dark
tokens, sidebar + inset shell, shadcn primitives).

## Re-building after a UI source change

The compiled bundle lives in dist/ and is **committed to git**
so cargo builds don't require bun/node.

To rebuild:

```bash
# one-time
~/workspace/claw-os/scripts/install-bun.sh

# every change
cd core/src/agent/web/ui
~/.local/bin/bun install
~/.local/bin/bun run build
```

Then cargo build -p cos --bin cos will pick up the new dist files
via include_dir!. Commit the regenerated dist/.

## Layout

- src/main.tsx          — React entry
- src/App.tsx           — sidebar + inset shell + hash router
- src/lib/api.ts        — fetch + SSE wrappers (token-aware)
- src/lib/notifications.tsx — live notification subscription and browser opt-in
- src/lib/router.ts     — hash-based router (no HTML5 history)
- src/components/       — sidebar, token gate
- src/components/ui/    — shadcn primitives (copied verbatim from OA)
- src/pages/            — durable chat/tasks, approvals, notification Inbox, raw system events, settings
- src/app/globals.css   — OA's oklch tokens (light + dark + sidebar)

Chat, Tasks, approvals, and Inbox use owner-scoped `clawd` routes. Session
history is read by the user-owned Web process from the same owner partition
the worker writes. Approval decisions invoke the installed polkit helper; the
Web process never gains direct permission-decision authority.
