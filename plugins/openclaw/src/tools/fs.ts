// cos app fs — structured file system operations.
// Replaces OpenClaw's internal fs-bridge with OS-level file management.

import { Type } from "@sinclair/typebox";
import { cosApp } from "../cos.js";

const schema = Type.Object(
  {
    operation: Type.Union(
      [
        Type.Literal("ls"),
        Type.Literal("read"),
        Type.Literal("write"),
        Type.Literal("stat"),
        Type.Literal("search"),
        Type.Literal("mkdir"),
        Type.Literal("rm"),
      ],
      {
        description:
          "File operation: ls (list), read, write, stat (metadata), search, mkdir, rm.",
      },
    ),
    path: Type.String({ description: "File or directory path." }),
    content: Type.Optional(
      Type.String({ description: "Content to write (for write operation)." }),
    ),
    pattern: Type.Optional(
      Type.String({ description: "Search pattern (for search operation)." }),
    ),
  },
  { additionalProperties: false },
);

export function createCosFsTool() {
  return {
    name: "cos_fs",
    label: "File System (Claw OS)",
    description:
      "Perform file system operations via the OS. Returns structured JSON " +
      "with metadata. All operations are audited and respect tier/scope permissions. " +
      "Operations: ls, read, write, stat, search, mkdir, rm.",
    parameters: schema,
    async execute(toolCallId: string, rawParams: Record<string, unknown>) {
      const { operation, path, content, pattern } = rawParams as {
        operation: string;
        path: string;
        content?: string;
        pattern?: string;
      };

      const args: string[] = [path];
      if (operation === "write" && content) {
        args.push("--content", content);
      }
      if (operation === "search" && pattern) {
        // search expects: cos app fs search <pattern> <path>
        const result = cosApp("fs", "search", [pattern, path]);
        return { content: JSON.stringify(result, null, 2) };
      }

      const result = cosApp("fs", operation, args);
      return { content: JSON.stringify(result, null, 2) };
    },
  };
}
