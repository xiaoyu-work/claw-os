# Web Desktop Module

## Purpose

`web/` contains the React/Vite Linux desktop served at the Claw OS GitHub
Pages root. It is based on
[`xiaoyu-work/linux_web`](https://github.com/xiaoyu-work/linux_web) commit
`cc0823b`; the full application set and desktop behavior are retained.

## Responsibilities

- Render the boot sequence, login screen, desktop, panels, window manager, and
  complete copied application registry.
- Open the current Claw OS marketing website from the **Claw OS Website**
  desktop shortcut in the built-in browser.
- Keep the existing marketing site self-contained under `public/site/`.
- Build a static Pages artifact with Vite.
- Generate the social preview image from repository-owned brand assets.

## Key Files

| Path | Role |
| --- | --- |
| `src/App.tsx` | Desktop composition and application window layer |
| `src/components/Desktop.tsx` | Desktop shortcuts and context menu |
| `src/components/WindowFrame.tsx` | Window movement, resizing, and controls |
| `src/components/AppRegistry.tsx` | Application implementation registry |
| `src/apps/Browser/index.tsx` | Browser that opens the embedded Claw OS site |
| `public/site/` | Existing agent-native marketing website |
| `vite.config.ts` | Vite build and shared brand-asset composition |
| `gen-og.py` | Generates `../assets/brand/og.png` |
| `../packaging/apt-repo/build-repo.sh` | Copies `dist/` into the Pages/APT artifact |

## Dependencies

The desktop uses React, TypeScript, Vite, Tailwind CSS, Zustand, Framer Motion,
Lucide, and the copied shadcn components declared in `package.json`. The Vite
build copies shared brand assets from `../assets/brand/` into `dist/`.

The Pages composition workflow builds `web/` as an independent artifact and
assembles the signed APT repository separately. The final Pages directory puts
the web artifact at `/` and APT metadata at `/dists` and `/pool`; neither is
included in the other. Build-time `@@GIT_SHA@@` and `@@SUITE@@` tokens are
replaced in the embedded marketing site during composition.

## Validation

From the repository root:

```bash
bash -n packaging/apt-repo/build-repo.sh
npm ci --prefix web --replace-registry-host=always
npm run build --prefix web
python3 web/gen-og.py
```

In a real browser, exercise boot, login, the **Claw OS Website** shortcut,
browser navigation, window controls, and the embedded site's six guided
scenarios at desktop and mobile widths. Fail on console errors, broken assets,
missing routes, incorrect maximized bounds, or viewport overflow.
