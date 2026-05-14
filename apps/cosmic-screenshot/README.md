# cosmic-screenshot — agent session bindings

This directory only contains the **manifest** (`app.json`) that
declares `cosmic-screenshot` as an MCP-server App for the kernel
agent. The actual server lives in the desktop binary at
`/usr/bin/cosmic-screenshot` (source: `desktop/screenshot/`).

## How the two halves connect

The desktop binary supports two run modes in the same executable:

1. **Default** — parses `Args` via clap, asks `xdg-desktop-portal`
   for a screenshot, writes the PNG under `~/Pictures` (or the
   `--save-dir`), and optionally posts a freedesktop notification.
2. **MCP server** — entered when `COS_MCP_SERVER=1` is set in the
   environment (the kernel always sets it via
   `core/src/agent/tools/cos_apps_session.rs::bring_up_app`). In
   this mode the binary speaks JSON-RPC MCP over stdio using the
   `cos-mcp-serve` crate and exposes one tool —
   `screenshot.capture` — backed by the same capture flow.

## Why the manifest's `entry` is absolute

`runtime: "binary"` means the kernel does `Command::new(entry)`
verbatim (see `build_command()` in `cos_apps_session.rs`). The path
`/usr/bin/cosmic-screenshot` is where `just install` in
`desktop/screenshot/` drops the binary on a real Claw OS rootfs.

This is why `cos app lint cosmic-screenshot` reports
`session.entry-missing` on a dev machine that hasn't installed the
desktop — the lint correctly flags that the kernel can't spawn the
server. The check passes once the binary is at the expected path.

## Tool surface

| Tool                 | Verb        | Notes                                                |
| -------------------- | ----------- | ---------------------------------------------------- |
| `screenshot.capture` | `fs.write`  | Args: `interactive`, `modal`, `save_dir`. Wild scope. |

Returns a JSON object with `cancelled` and `path` fields.
