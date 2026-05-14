# `clawos-agent-web`

The React/Next.js UI for the ClawOS Agent App (`com.clawos.Agent`).

## Origin

Vendored from [`vercel-labs/open-agents`](https://github.com/vercel-labs/open-agents),
specifically `apps/web/` and the supporting workspace packages
`packages/shared/` and `packages/tsconfig/`. The original MIT license
text is preserved at `./LICENSE`. The repo-level
[`/NOTICE`](../../../NOTICE) carries the upstream attribution required
by the MIT terms.

## What lives here

```
desktop/agent/web/
├── app/                # Next.js App Router
├── components/         # React components (sidebar, composer, message list, …)
├── hooks/              # Custom hooks
├── lib/                # Auth, DB, GitHub, sandbox, AI glue (mostly stripped — see below)
├── public/             # Static assets (icons, fonts)
├── packages/
│   ├── shared/         # Diff, paste-block, tool-state utilities; reasoning/todo contexts
│   └── tsconfig/       # Base tsconfig fragments
├── package.json        # Bun workspace root for this subtree
└── LICENSE             # Upstream MIT
```

## How it is wired into ClawOS

- This subtree builds to a static SPA (`next build` / `next export`).
- The bundle is served by `cos-agent-bridge`
  (`desktop/agent/bridge/`) as static files at
  `http://127.0.0.1:$PORT/`.
- All `/api/*` calls hit the same bridge process, which proxies them
  to `cos agent` via subprocess.
- No external network egress; no Vercel; no sandbox VM; no GitHub.

## What was removed from the upstream

See [`STRIPPED.md`](./STRIPPED.md) for the explicit list of files,
imports, and dependencies dropped during the `strip-cloud-features`
todo. The short version: GitHub integration, sandbox VM lifecycle,
PR/branch tooling, better-auth + Drizzle/Postgres + ioredis, Vercel
analytics, and the Workflow SDK.

## Building

```
bun install
bun run build       # static export
```

The result is dropped into `out/` and the bridge serves it from
`/usr/share/cos-agent/web/` at install time.
