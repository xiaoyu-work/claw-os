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
~/.local/bin/bun run typecheck
~/.local/bin/bun test
~/.local/bin/bun run build
```

Then cargo build -p cos --bin cos will pick up the new dist files
via include_dir!. Commit the regenerated dist/.
Install dependencies with `bun install` only when setting up a checkout or when
the chosen validation reports missing dependencies. If `dist/` already has
unrelated changes, do not overwrite it during validation; use the isolated
build below and integrate regenerated distribution files deliberately.

## Layout

- src/main.tsx          — React entry
- src/App.tsx           — sidebar + inset shell + hash router
- src/lib/api.ts        — fetch + SSE wrappers (token-aware)
- src/lib/notifications.tsx — live notification subscription and browser opt-in
- src/lib/router.ts     — hash-based router (no HTML5 history)
- src/components/       — sidebar, token gate
- src/components/ui/    — shadcn primitives (copied verbatim from OA)
- src/pages/            — Activities, durable chat/tasks, approvals, notification Inbox, raw system events, settings
- src/app/globals.css   — OA's oklch tokens (light + dark + sidebar)

Chat, Tasks, approvals, and Inbox use owner-scoped `clawd` routes. Session
history is read by the user-owned Web process from the same owner partition
the worker writes. Approval decisions invoke the installed polkit helper; the
Web process never gains direct permission-decision authority.

## Activities

`#/activities`, `#/activities/new`, and `#/activities/:id` present the
[shared Activity service](../../../../../docs/activities.md). The UI holds only
fetched views and unsaved forms; the broker owns persistence, state changes,
ownership, and work submission. The authenticated adapters are:

| HTTP route | Broker command |
| --- | --- |
| `GET /api/activities?state=active&limit=100` | `activity.list` |
| `POST /api/activities` | `activity.create` |
| `GET /api/activities/{id}?limit=100` | `activity.get` |
| `POST /api/activities/{id}/update` | `activity.update` |
| `POST /api/activities/{id}/transition` | `activity.transition` |
| `POST /api/activities/{id}/run` | `activity.run` |

Creation saves a goal without starting work. The detail view shows goal,
completion criteria, planning boundaries, inert resource references, job
progress/result previews, and associated sessions. Submit work in a new session
or explicitly continue an associated session. Tasks, approval decisions, and
conversation history open the existing `#/tasks`, `#/approvals`, and
`#/chat/:id` views; Activities introduce no new approval authority.

Completion requires a user-written confirmation note. Successful jobs do not
complete goals automatically. Pausing or cancelling gates subsequent work,
without undoing or stopping current effects; use Tasks to cancel a job.
Completed/cancelled goals must be explicitly reopened before editing or running.
Boundaries are planning text, not enforced permissions; ordinary capabilities
and approval checks remain in force.

Views refetch after mutations, on focus/notification changes, every three
seconds while associated jobs are queued/running/waiting for approval, and
every ten seconds otherwise. Stale reads are aborted and cannot replace a newer
selection. Read errors remain visible; there is no local fallback. Lists and
job projections show at most 100 records.

### Activity validation

From this directory, with existing dependencies, Node.js 22+ and an installed
Chromium-based browser:

```bash
bun run typecheck
bun test test/activities.test.ts test/activity-views.test.tsx
bun run build --outDir .activity-validation/dist
bun run test:browser
```

The browser regression serves that isolated production build, exercises the
token exchange and authenticated mocked API, and requires completed UI actions:
create/list/detail, persistence across reloads, live job progress, existing
approval/session/task flows, continuation, edits, explicit state changes, and
stale read/mutation/creation races. Console errors and unexpected outbound
requests fail the test. It neither contacts a model nor uses real credentials.
Browser profiles stay under `.activity-validation/` and are removed after the
test; the build never touches `dist/`. Set `ACTIVITY_BROWSER` to an installed
browser executable or `ACTIVITY_UI_DIST` to another prebuilt output if needed.
