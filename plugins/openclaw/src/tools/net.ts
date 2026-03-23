// cos net fetch — HTTP client with structured response.
// Replaces OpenClaw's undici/web-fetch with OS-level HTTP.

import { Type } from "@sinclair/typebox";
import { cos } from "../cos.js";

const schema = Type.Object(
  {
    url: Type.String({ description: "URL to fetch." }),
    method: Type.Optional(
      Type.Union(
        [
          Type.Literal("GET"),
          Type.Literal("POST"),
          Type.Literal("PUT"),
          Type.Literal("DELETE"),
        ],
        { description: "HTTP method (default: GET)." },
      ),
    ),
    data: Type.Optional(
      Type.String({ description: "Request body (JSON string for POST/PUT)." }),
    ),
  },
  { additionalProperties: false },
);

export function createCosNetTool() {
  return {
    name: "cos_net_fetch",
    label: "HTTP Fetch (Claw OS)",
    description:
      "Make HTTP requests via the OS. Returns structured JSON with " +
      "status code, headers, and body. All requests are audited.",
    parameters: schema,
    async execute(toolCallId: string, rawParams: Record<string, unknown>) {
      const { url, method, data } = rawParams as {
        url: string;
        method?: string;
        data?: string;
      };
      const args: string[] = [url];
      if (method) args.push("--method", method);
      if (data) args.push("--data", data);
      const result = cos("net", "fetch", args);
      return { content: JSON.stringify(result, null, 2) };
    },
  };
}
