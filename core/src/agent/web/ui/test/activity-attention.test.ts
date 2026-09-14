import { describe, expect, test } from "bun:test";

import { readActivityAttention } from "../src/lib/activity-attention";

const id = "00000000-0000-4000-8000-000000000001";
const empty = {
  schema: 1,
  activity_id: id,
  activity_state: "active",
  limit: 100,
  counts: {
    queued: 0, running: 0, waiting: 0, completed: 0, failed: 0,
    cancelled: 0, indeterminate: 0, pending_decisions: 0,
    unavailable_decisions: 0, unread_notifications: 0,
  },
  decisions: [],
  issues: [],
  notifications: [],
  totals: { decisions: 0, issues: 0, notifications: 0 },
  has_more: { decisions: false, issues: false, notifications: false },
};

describe("Activity attention contract", () => {
  test("accepts the shared read-only shape", () => {
    expect(readActivityAttention(empty, id)).toEqual(empty);
  });

  test("rejects leaked unavailable decision fields and false truncation", () => {
    expect(() => readActivityAttention({
      ...empty,
      decisions: [{
        id: "ap-1", job_id: "job-1", session_id: null, status: "unavailable",
        requested_at: null, verb: "fs.write", scope: null, risk: null,
        reason: null, review_id: null, error: "Unavailable",
      }],
      totals: { ...empty.totals, decisions: 1 },
    }, id)).toThrow();
    expect(() => readActivityAttention({
      ...empty,
      has_more: { ...empty.has_more, issues: true },
    }, id)).toThrow();
  });

  test("keeps notification acknowledgement distinct from decisions", () => {
    const view = readActivityAttention({
      ...empty,
      notifications: [{
        id: "note-1", source: "agent.task", kind: "waiting", severity: "error",
        title: "Review", body: "Open the task", task_id: "job-1", session_id: null,
        state: "acknowledged", updated_at_ms: 1,
      }],
      totals: { ...empty.totals, notifications: 1 },
    }, id);
    expect(view.notifications[0].state).toBe("acknowledged");
    expect(view.decisions).toEqual([]);
  });

  test("rejects every detail collection above the declared limit", () => {
    const fixtures = {
      decisions: {
        id: "ap-1", job_id: "job-1", session_id: null, status: "pending",
        requested_at: 1, verb: "fs.read", scope: { kind: "path", value: "/tmp" },
        risk: "low", reason: "Read", review_id: "ap-1", error: null,
      },
      issues: {
        job_id: "job-1", session_id: null, kind: "failed", status: "error",
        execution_phase: "failed", title: "Failed", created_at: "now",
        finished_at: null, message: "Failed",
      },
      notifications: {
        id: "note-1", source: "agent.task", kind: "failed", severity: "error",
        title: "Failed", body: "Inspect the task", task_id: "job-1",
        state: "unread", updated_at_ms: 1,
      },
    };
    for (const [field, fixture] of Object.entries(fixtures)) {
      expect(() => readActivityAttention({
        ...empty,
        limit: 1,
        [field]: [fixture, { ...fixture, id: "second", job_id: "job-2" }],
        totals: { ...empty.totals, [field]: 2 },
      }, id)).toThrow();
    }
  });
});
