// cos exec run — execute shell commands with structured output.
// Replaces OpenClaw's bash-tools with OS-managed execution.

import { Type } from "@sinclair/typebox";
import { cos } from "../cos.js";

const schema = Type.Object(
  {
    command: Type.String({
      description: "Shell command to execute.",
    }),
    timeout: Type.Optional(
      Type.Number({
        description: "Timeout in seconds (default: 300).",
      }),
    ),
  },
  { additionalProperties: false },
);

export function createCosExecTool() {
  return {
    name: "cos_exec",
    label: "Execute Command (Claw OS)",
    description:
      "Execute a shell command via the OS process manager. " +
      "Returns structured JSON with exit code, stdout, stderr. " +
      "All executions are automatically audited by the OS. " +
      "Includes guardrails: rapid respawn detection and destructive command warnings.",
    parameters: schema,
    async execute(toolCallId: string, rawParams: Record<string, unknown>) {
      const { command, timeout } = rawParams as { command: string; timeout?: number };
      const args = ["--shell", "bash", command];
      if (timeout) args.unshift("--timeout", String(timeout));
      const result = cos("exec", "run", args);
      return { content: JSON.stringify(result, null, 2) };
    },
  };
}
