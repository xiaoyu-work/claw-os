# Claw OS Web Desktop

The GitHub Pages root is a complete browser-based Linux desktop imported from
[`xiaoyu-work/linux_web`](https://github.com/xiaoyu-work/linux_web). It retains
the original boot, login, desktop, window manager, settings, utilities, games,
and application registry.

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
