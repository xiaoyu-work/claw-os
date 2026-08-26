# Website Module

## Purpose

`web/` contains the framework-free Claw OS marketing website served from the
same GitHub Pages origin as the signed APT repository.

## Responsibilities

- Explain the agent-native OS vision and supported installation paths.
- Provide an accessible, responsive, interactive Claw OS product experience.
- Keep page behavior self-contained in plain HTML, CSS, and JavaScript.
- Generate the social preview image from repository-owned brand assets.

## Key Files

| Path | Role |
| --- | --- |
| `index.html` | Page structure, product copy, and interactive demo shell |
| `style.css` | Responsive visual system and demo presentation |
| `app.js` | Navigation, copy controls, and guided demo state |
| `gen-og.py` | Generates `../assets/brand/og.png` |
| `../packaging/apt-repo/build-repo.sh` | Copies the site into the Pages/APT artifact |

## Dependencies

The website uses shared brand assets from `../assets/brand/`. The APT
repository builder copies those assets and the browser-facing files in this
directory into `build/apt-repo/`, then replaces `@@GIT_SHA@@` and `@@SUITE@@`
tokens before Pages deployment.

## Validation

From the repository root:

```bash
bash -n packaging/apt-repo/build-repo.sh
python3 web/gen-og.py
```

Load the composed site in a real browser at desktop and mobile widths. Exercise
every guided scenario through prompt, approval, tool execution, result, and
audit state; fail on console errors, broken assets, missing anchors, or
horizontal overflow.
