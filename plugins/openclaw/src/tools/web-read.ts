// cos web read — fetch any URL as clean Markdown with full JS rendering.
// Replaces OpenClaw's Playwright-based browser tool for content extraction.

import { Type } from "@sinclair/typebox";
import { cos } from "../cos.js";

const schema = Type.Object(
  {
    url: Type.String({ description: "The URL to fetch and convert to Markdown." }),
  },
  { additionalProperties: false },
);

export function createCosWebReadTool() {
  return {
    name: "cos_web_read",
    label: "Web Read (Claw OS)",
    description:
      "Fetch a URL and return its content as clean Markdown. " +
      "Uses the OS built-in browser engine with full JavaScript rendering, " +
      "Readability content extraction, and automatic link resolution. " +
      "Returns structured JSON with title, content, and links.",
    parameters: schema,
    async execute(toolCallId: string, rawParams: Record<string, unknown>) {
      const { url } = rawParams as { url: string };
      const result = cos("web", "read", [url]);
      return { content: JSON.stringify(result, null, 2) };
    },
  };
}
