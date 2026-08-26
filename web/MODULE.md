# Web Desktop Module

## Purpose

`web/` contains the React/Vite Linux desktop served at the Claw OS GitHub
Pages root. It is based on
[`xiaoyu-work/linux_web`](https://github.com/xiaoyu-work/linux_web) commit
`cc0823b`; the full application set and desktop behavior are retained.

## Responsibilities

- Open directly into the desktop without a boot or login gate, then render the
  panels and window manager.
- Mirror the first-party native desktop app set with lightweight web demos for
  Agent, Files, Text Editor, App Store, Media Player, Screenshot, and Settings.
- Enforce a click-by-click first-run spotlight from the Agent desktop icon
  through plan review, scoped approval, visible tools, and audited result.
- Keep all six Agent scenarios available after the guided path, together with
  the freeform chat demo.
- Give Files a grounded AI conversation panel for natural-language search,
  summaries, storage analysis, duplicate detection, and safe organization
  previews.
- Provide scripted AI-style assistants in App Store, Settings, and Browser for
  app recommendations, direct demo-setting changes, page explanations, and
  in-page navigation without external model calls.
- Reuse the native Claw OS application icons under `public/app-icons/`.
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
| `src/components/AppIcon.tsx` | Shared native-icon and Lucide icon renderer |
| `src/components/GuideOverlay.tsx` | Blocking spotlight for the first-run Agent path |
| `src/components/ScriptedAssistantPanel.tsx` | Shared chat UI for local scripted app assistants |
| `src/apps/Agent/index.tsx` | Guided system Agent demo |
| `src/apps/FileManager/FilesAiPanel.tsx` | Grounded Files AI chat and quick actions |
| `src/apps/AppStore/StoreAiPanel.tsx` | Scripted app-need recommendations |
| `src/apps/Settings/SettingsAiPanel.tsx` | Natural-language demo setting changes |
| `src/apps/Browser/BrowserAiPanel.tsx` | Scripted page help and navigation |
| `src/apps/AppStore/index.tsx` | First-party application catalog demo |
| `src/apps/MediaPlayer/index.tsx` | Local media-player demo |
| `src/apps/Screenshot/index.tsx` | Screenshot workflow demo |
| `src/apps/Browser/index.tsx` | Browser that opens the embedded Claw OS site |
| `public/app-icons/` | Icons shared with native desktop applications |
| `public/site/` | Existing agent-native marketing website |
| `vite.config.ts` | Vite build and shared brand-asset composition |
| `gen-og.py` | Generates `../assets/brand/og.png` |
| `../packaging/apt-repo/build-repo.sh` | Copies `dist/` into the Pages/APT artifact |

## Dependencies

The desktop uses React, TypeScript, Vite, Tailwind CSS, Zustand, Framer Motion,
Lucide, and the copied shadcn components declared in `package.json`. The Vite
build copies shared brand assets from `../assets/brand/` into `dist/`.

The native application icons retain the provenance and licenses documented in
`public/app-icons/README.md` and their owning `desktop/` components.

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

In a real browser, confirm the root opens directly into the desktop and blocks
input outside the highlighted target. Complete the seven-step Agent path, then
exercise all six scenarios, freeform Agent chat, every registered app, App
Store updates, media controls, screenshot options, Settings About, the
**Claw OS Website** shortcut, browser navigation, and window controls at
desktop and mobile widths. In Files, exercise the AI quick actions, selected
file summaries, natural-language search, result navigation, and read-only
organization preview. Also verify App Finder recommendations and app opening,
Settings Assistant changes, and Browser Assistant answers and section
navigation. Fail on console errors, broken assets, missing routes, inaccessible
controls, incorrect window bounds, or viewport overflow.
