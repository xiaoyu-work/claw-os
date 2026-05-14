# `clawos-agent-web` — what was stripped from upstream

This package was forked from
[`open-agents/apps/web`](https://github.com/vercel/open-agents) (commit
captured in the initial vendor commit; MIT-licensed, see `./LICENSE`).
Open Agents is a **cloud coding agent** UI — it manages remote sandbox
VMs, GitHub repos, PR creation, deployment previews. Claw OS only needs
the **chat surface + local agent runtime**; the entire cloud half was
removed.

If something feels missing and you need to bring it back, check the
upstream repo at the vendor commit; everything below has a 1:1
counterpart there.

## Removed wholesale

### Backend / API
- `app/api/{auth,github,sandbox,vercel,generate-pr,usage,transcribe,settings,generate-title}/`
- `app/api/chat/[chatId]/` (durable workflow streaming + stop endpoints)
- `app/api/sessions/[sessionId]/` (session/diff/git endpoints)
- All `_lib/` request-validation helpers under `app/api/*/`

### App routes
- `app/[username]/`, `app/u/` — public profile pages
- `app/codespace/`, `app/deploy-your-own/`, `app/get-started/`,
  `app/workflows/` — onboarding & Vercel deploy flows
- `app/sessions/` and `app/sessions/[sessionId]/chats/[chatId]/` —
  the cloud-coding-agent chat surface (deeply tied to sandbox VMs,
  GitHub branches, diff viewer, PR dialogs). Replaced with a minimal
  `<ChatShell />` rendered at `/`.
- `app/settings/`, `app/shared/`
- `app/home-page.tsx`, `app/config.ts`
- `app/opengraph-image.tsx`, `app/twitter-image.tsx`

### `lib/` subsystems
- `lib/auth/`, `lib/db/`, `lib/session/` — better-auth + Drizzle + Postgres
- `lib/github/`, `lib/git/` — Octokit + git provider integrations
- `lib/sandbox/` — Vercel Sandbox lifecycle
- `lib/vercel/` — Vercel projects/tokens
- `lib/admin/`, `lib/deployment/`, `lib/skills/`, `lib/usage/`,
  `lib/diff/`
- `lib/chat/` — durable workflow plumbing
- Loose modules:
  `abortable-chat-transport`, `assistant-file-links*`, `botid`,
  `chat-auto-commit*`, `chat-instance-manager`, `chat-route-cleanup*`,
  `chat-streaming-state*`, `diffs-config`, `file-suggestions`,
  `managed-template-trial`, `merge-readiness-polling*`,
  `model-access*`, `model-availability*`, `model-options*`,
  `model-variants*`, `models-with-context`, `onboarding`,
  `pr-deployment-polling*`, `rate-limit*`, `redirect-safety`,
  `redis*`, `skills-cache*`, `streamdown-config*`, `vercel-themes`,
  `workspace-status-store*`

### Components
- `components/auth/`, `components/landing/`, `components/tool-call/`
- All branch-/repo-/PR-/merge-/sandbox-related dialogs and selectors
- `components/{github-reconnect-dialog,github-reconnect-gate,
  chat-switcher-dropdown,contribution-chart,file-suggestions-dropdown,
  new-session-dialog,sandbox-selector-compact,
  session-starter-vercel-sync-section,task-group-view}.tsx`
- Chat-surface components that depended on the dropped backend:
  `assistant-file-link`, `assistant-message-groups`, `inbox-sidebar*`,
  `inline-question-input`, `image-attachments-preview`,
  `text-attachments-preview`, `home-skeleton`, `pinned-todo-panel*`,
  `selection-popover`, `session-list`, `session-drawer`,
  `session-starter`, `slash-command-dropdown`, `snippet-chip`,
  `thinking-block`, `tool-calls-summary-bar`,
  `message-model-pill`, `model-combobox`, `model-selector-compact`,
  `provider-icons`, `diffs-provider`, `user-avatar-dropdown`,
  `file-type-icons`

### Hooks
- `hooks/{use-session,use-session-chats,use-sessions,
  use-session-skills,use-session-diff,use-session-files,
  use-session-git-status,use-slash-commands,use-installation-repos,
  use-leaderboard-rank,use-user-preferences,
  use-vercel-repo-projects,use-github-connection-status,
  use-file-suggestions,use-background-chat-notifications,
  use-model-options}.ts(x)`

### Tooling & config
- `drizzle.config.ts`, `scripts/check-migrations.ts` — Drizzle ORM
- `proxy.ts`, `instrumentation-client.ts` — Vercel preview proxy + BotID
- `next.config.ts`: removed `withWorkflow()`, `withBotId()` wrappers
  and Vercel/GitHub image hosts
- `app/layout.tsx`: removed `@vercel/analytics`, removed Vercel-env
  `metadataBase` lookup, renamed default title `Open Agents` →
  `Claw OS Agent`
- `app/providers.tsx`: removed `authClient`, `GitHubReconnectGate`,
  401-driven `signOut()` SWR error handler

### Static assets
- `public/{vercel.svg,next.svg,file.svg,globe.svg,window.svg,
  Submarine.aiff,Submarine.wav,favicon-preview.svg}`
- `public/.well-known/workflow/` — Vercel Workflow manifest

### Docs
- `docs/sandbox-state-persistence-plan.md`,
  `docs/diff-viewer-plan.md`

### Dependencies (`package.json`)
Removed entirely:
`@ai-sdk/{anthropic,openai,react,elevenlabs}`, `@octokit/*`,
`@open-agents/{agent,sandbox}`, `@pierre/*`, `@streamdown/code`,
`@vercel/*`, `arctic`, `better-auth`, `botid`, `drizzle-orm`,
`drizzle-kit`, `ioredis`, `jose`, `postgres`, `server-only`,
`streamdown`, `workflow`, `dotenv`, `@types/bun`.
Removed `db:*` scripts.

## What remains

| Layer            | Source                                                    |
|------------------|-----------------------------------------------------------|
| App shell        | `app/{layout,providers,page}.tsx`                         |
| Chat surface     | `components/chat-shell.tsx` (new — minimal, fetch-based)  |
| Design system    | `components/ui/*`, `app/globals.css`                      |
| Utilities        | `lib/{utils,bridge,models,image-utils,...}.ts`            |
| Local-only API   | `app/api/{chat,models,sessions}/route.ts` (proxy to bridge) |
| Hooks            | `hooks/{use-audio-recording,use-image-attachments,...}.ts` |
| Shared package   | `packages/shared/` (paste-blocks, diff utils, contexts)   |

The `app/api/*` route handlers are **dev-mode proxies** to the local
`cos-agent-bridge` HTTP daemon (`lib/bridge.ts` discovers its port via
`$XDG_RUNTIME_DIR/cos-agent-bridge.port`). In production the bridge
itself serves the exported SPA + `/api/*`, so these route handlers are
bypassed.
