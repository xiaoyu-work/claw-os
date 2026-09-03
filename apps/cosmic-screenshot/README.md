# cosmic-screenshot — native MCP App

This directory only contains the **manifest** (`app.json`) that
declares `cosmic-screenshot` as an MCP-server App for the kernel
agent. The actual server lives in the desktop binary at
`/usr/bin/cosmic-screenshot` (source: `desktop/screenshot/`).

## How the two halves connect

The desktop binary supports two run modes in the same executable:

1. **Default** — parses `Args` via clap, asks `xdg-desktop-portal`
   for a screenshot, writes the PNG under the XDG Pictures directory (or the
   `--save-dir`), and optionally posts a freedesktop notification.
2. **MCP server** — entered when `COS_MCP_SERVER=1` is set in the
   environment (the kernel always sets it via
   `core/src/agent/tools/cos_apps_session.rs::bring_up_app`). In
   this mode the binary speaks JSON-RPC MCP over stdio using the
   `claw_os_sdk::mcp` module. The verified `app.json.mcp.tools`
   declaration supplies the tool description and input schema.

## Why the manifest's `entry` is absolute

`runtime: "binary"` means the kernel executes the entry directly. The
path `/usr/bin/cosmic-screenshot` is where `just install` in
`desktop/screenshot/` drops the binary on a real Claw OS rootfs.

An absolute `mcp.entry` is not something any App may declare. The
kernel keeps a fixed table in `core/src/worker/trusted_desktop.rs`
that names trusted vendor App ids and the exact system programs they may
point at; every other App is refused. An allowlisted path is honoured only
when the package verified through vendor
provenance and both the package tree and the binary are root-owned and
not group/world-writable.

This is why `cos app lint cosmic-screenshot` reports
`session.entry-missing` on a dev machine that hasn't installed the
desktop — the lint correctly flags that the kernel can't launch the
server. The check passes once the binary is at the expected path.

## Sandbox and transports

The server runs inside the hostile-worker sandbox in the
`TrustedDesktopSession` tier: private mount, PID, IPC, UTS, user and
network namespaces, the strict seccomp filter, a resource governor, no
egress and no host paths — plus a bind mount of the owner's session-bus
socket, which is what `ashpd` needs to reach `xdg-desktop-portal` and
`org.freedesktop.Notifications`. No Wayland socket is granted: MCP mode
never initialises libcosmic.

## Tool surface

| Tool                 | Verb        | Notes                                                          |
| -------------------- | ----------- | -------------------------------------------------------------- |
| `screenshot.capture` | `fs.write`  | Args: `interactive`, `modal`, optional `save_dir`. |

When `save_dir` is omitted or unusable, the handler resolves the XDG Pictures
directory at runtime; the manifest deliberately does not substitute a literal
`~/Pictures` argument default. An explicit destination receives a separate
argument-bound write grant.

Returns a JSON object with `cancelled` and `path` fields.
