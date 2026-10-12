import { expect, test } from "bun:test";
import { applyFrame, markToolsUnconfirmed } from "../src/lib/chat-presentation";
import { EVIDENCE_CAVEAT } from "../src/lib/chat-evidence";
import type { ChatMessage } from "../src/lib/chat-history";

function message(): ChatMessage {
  return { id: "answer", role: "assistant", text: "", attachments: [], tools: [], reasoning: [], warnings: [], status: "streaming" };
}

test("provider announcements prepare; runtime dispatch runs; detach does not invent completion", () => {
  const msg = message();
  applyFrame(msg, "tool_use_start", { id: "tool-1", name: "observe" });
  expect(msg.tools[0].phase).toBe("preparing");
  applyFrame(msg, "tool_use", { id: "tool-1", name: "observe" });
  expect(msg.tools[0].phase).toBe("preparing");
  applyFrame(msg, "tool_start", { id: "tool-1", name: "observe" });
  expect(msg.tools[0].phase).toBe("running");
  markToolsUnconfirmed(msg);
  expect(msg.tools[0].phase).toBe("unconfirmed");
  expect(msg.tools[0].finished).toBe(false);
  applyFrame(msg, "tool_result", { id: "tool-1", name: "observe", ok: false });
  expect(msg.tools[0].phase).toBe("failed");
  applyFrame(msg, "done", {});
  expect(msg.tools[0].phase).toBe("failed");
});

test("missing tool outcomes and unsupported evidence stay visibly unconfirmed", () => {
  const msg = message();
  applyFrame(msg, "tool_result", { id: "tool-1", name: "observe", output: "PRIVATE_BODY" });
  expect(msg.tools[0].phase).toBe("unconfirmed");
  expect(msg.warnings).toContain("Tool result omitted its confirmed outcome.");
  expect(JSON.stringify(msg)).not.toContain("PRIVATE_BODY");
  applyFrame(msg, "evidence", { status: "verified", required: true });
  expect(msg.evidence).toBeUndefined();
  expect(msg.warnings.length).toBe(2);
});

test("evidence presents source binding, never model confidence or goal-completion authority", () => {
  const msg = message();
  applyFrame(msg, "evidence", {
    status: "verified", required: true, verified_claims: 1, total_claims: 1,
    source_count: 1, sources: [{
      tool_call_id: "tool-1", tool_name: "observe", is_error: false,
      result_sha256: "0".repeat(64), result_bytes: 128,
    }],
    warnings: [], interpretation: "One citation is bound to its retained tool result.", truncated: false,
    claim_confidence: 1, goal_completed: true, claims: [{ claim: "PRIVATE_CLAIM" }],
  });
  expect(msg.evidence?.sources[0].tool_call_id).toBe("tool-1");
  expect(JSON.stringify(msg.evidence)).not.toContain("goal_completed");
  expect(JSON.stringify(msg.evidence)).not.toContain("claim_confidence");
  expect(JSON.stringify(msg.evidence)).not.toContain("PRIVATE_CLAIM");
  expect(EVIDENCE_CAVEAT).toContain("does not prove");
});
