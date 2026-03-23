# Claw OS Plugin for OpenClaw

Integrates Claw OS system capabilities as first-class OpenClaw tools.

When this plugin is enabled, OpenClaw uses the OS built-in browser engine, file system, process manager, and HTTP client instead of its own implementations. This gives the agent:

- **Structured JSON output** from every operation
- **Automatic audit logging** of all actions
- **Checkpoint/rollback** — undo any file changes instantly
- **Sandbox isolation** and permission tiers
- **Error recovery hints** — structured suggestions when operations fail

## Registered Tools

| Tool | Replaces | What It Does |
|------|----------|-------------|
| `cos_web_read` | Playwright browser | Fetch URL → Markdown with full JS rendering |
| `cos_exec` | bash-tools | Execute shell commands with guardrails |
| `cos_fs` | fs-bridge | File operations (ls, read, write, search, rm) |
| `cos_net_fetch` | web-fetch / undici | HTTP requests |
| `cos_doc_read` | pdfjs-dist | Read PDF, DOCX, XLSX, CSV as text |
| `cos_checkpoint` | *(new)* | Snapshot, diff, and rollback the workspace |

## Install

From within a Claw OS container:

```bash
# Install OpenClaw
npm install -g openclaw@latest

# Install this plugin
openclaw plugin install /usr/lib/cos/plugins/openclaw
```

Or if running OpenClaw outside the container, point to the plugin directory:

```bash
openclaw plugin install ./plugins/openclaw
```

## Usage

Once installed, the tools are automatically available to the agent. No configuration needed.

The agent will see tools like `cos_web_read`, `cos_exec`, `cos_fs` in its tool list and use them naturally. The `cos_checkpoint` tool is unique to Claw OS — it lets the agent snapshot and rollback the workspace, which is not possible on a regular OS.

## Development

```bash
cd plugins/openclaw
npm install
npm run build
```

## Requirements

- Claw OS (the `cos` binary must be in PATH)
- OpenClaw v2024.1+
- Node.js 22+
