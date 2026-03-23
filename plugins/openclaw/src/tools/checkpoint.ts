// cos checkpoint — OS-level undo/rollback.
// This is unique to Claw OS — no equivalent in OpenClaw.

import { Type } from "@sinclair/typebox";
import { cos } from "../cos.js";

const schema = Type.Object(
  {
    action: Type.Union(
      [
        Type.Literal("create"),
        Type.Literal("diff"),
        Type.Literal("rollback"),
        Type.Literal("list"),
        Type.Literal("status"),
      ],
      {
        description:
          "Checkpoint action: create (snapshot), diff (show changes), " +
          "rollback (undo), list (all checkpoints), status (overlay info).",
      },
    ),
    description: Type.Optional(
      Type.String({
        description: "Description for the checkpoint (used with create).",
      }),
    ),
    id: Type.Optional(
      Type.String({
        description: "Checkpoint ID to rollback to (used with rollback).",
      }),
    ),
  },
  { additionalProperties: false },
);

export function createCosCheckpointTool() {
  return {
    name: "cos_checkpoint",
    label: "Checkpoint (Claw OS)",
    description:
      "Snapshot, diff, and rollback the workspace. Powered by OverlayFS — " +
      "every file change is captured at the OS level regardless of how it " +
      "was made. Use 'create' before risky operations, 'diff' to see what " +
      "changed, and 'rollback' to undo everything instantly.",
    parameters: schema,
    async execute(toolCallId: string, rawParams: Record<string, unknown>) {
      const { action, description, id } = rawParams as {
        action: string;
        description?: string;
        id?: string;
      };
      const args: string[] = [];
      if (action === "create" && description) args.push(description);
      if (action === "rollback" && id) args.push(id);
      const result = cos("checkpoint", action, args);
      return { content: JSON.stringify(result, null, 2) };
    },
  };
}
