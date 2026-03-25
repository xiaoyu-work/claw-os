// cos app doc read — universal document reader.
// Replaces OpenClaw's pdfjs-dist with OS-level document extraction.

import { Type } from "@sinclair/typebox";
import { cosApp } from "../cos.js";

const schema = Type.Object(
  {
    path: Type.String({
      description:
        "Path to document file (PDF, DOCX, XLSX, CSV, or image).",
    }),
  },
  { additionalProperties: false },
);

export function createCosDocTool() {
  return {
    name: "cos_doc_read",
    label: "Document Reader (Claw OS)",
    description:
      "Read any document format (PDF, DOCX, XLSX, CSV, images) and " +
      "return structured text. Uses the OS built-in document engine — " +
      "no need for format-specific libraries.",
    parameters: schema,
    async execute(toolCallId: string, rawParams: Record<string, unknown>) {
      const { path } = rawParams as { path: string };
      const result = cosApp("doc", "read", [path]);
      return { content: JSON.stringify(result, null, 2) };
    },
  };
}
