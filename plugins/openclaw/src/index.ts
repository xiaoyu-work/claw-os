// Claw OS integration plugin for OpenClaw.
//
// Registers OS-level tools as first-class OpenClaw tools, replacing
// built-in implementations (Playwright, node:fs, child_process) with
// cos commands that leverage the OS's structured JSON interface,
// sandboxing, checkpoints, and audit trail.

import { definePluginEntry, type AnyAgentTool } from "openclaw/plugin-sdk/plugin-entry";
import { createCosWebReadTool } from "./tools/web-read.js";
import { createCosExecTool } from "./tools/exec.js";
import { createCosFsTool } from "./tools/fs.js";
import { createCosNetTool } from "./tools/net.js";
import { createCosDocTool } from "./tools/doc.js";
import { createCosCheckpointTool } from "./tools/checkpoint.js";

export default definePluginEntry({
  id: "claw-os",
  name: "Claw OS",
  description:
    "Integrates Claw OS system capabilities as first-class agent tools. " +
    "Replaces built-in browser, file system, and execution tools with " +
    "OS-level equivalents that provide structured JSON output, automatic " +
    "audit logging, sandbox isolation, and checkpoint/rollback support.",
  register(api) {
    api.registerTool(createCosWebReadTool() as AnyAgentTool);
    api.registerTool(createCosExecTool() as AnyAgentTool);
    api.registerTool(createCosFsTool() as AnyAgentTool);
    api.registerTool(createCosNetTool() as AnyAgentTool);
    api.registerTool(createCosDocTool() as AnyAgentTool);
    api.registerTool(createCosCheckpointTool() as AnyAgentTool);
  },
});
