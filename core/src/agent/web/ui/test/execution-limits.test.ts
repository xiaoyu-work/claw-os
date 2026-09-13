import { afterEach, expect, mock, spyOn, test } from "bun:test";
import { api } from "../src/lib/api";
import {
  executionLimitsApi, readExecutionLimits, readExecutionLimitsView,
  type ActivityExecutionLimits,
} from "../src/lib/execution-limits";

afterEach(() => mock.restore());
const policy = (): ActivityExecutionLimits => ({
  activity_id: "activity-1", owner_uid: 1000, revision: 1, enabled: true,
  limits: { max_attempts: 10, max_turns_per_attempt: 4, expires_at: "2099-01-01T00:00:00Z" },
  used_attempts: 2, created_at: "2026-09-11T00:00:00Z", updated_at: "2026-09-11T00:00:00Z",
});

test("execution limits are bounded policy data with explicit absence and preserved counters", () => {
  expect(readExecutionLimitsView({ schema: 1, activity_id: "activity-1", execution_limits: null }, "activity-1").execution_limits).toBeNull();
  const lowered = { ...policy(), limits: { ...policy().limits, max_attempts: 1 } };
  expect(readExecutionLimits(lowered, "activity-1").used_attempts).toBe(2);
  for (const invalid of [
    { ...policy(), owner_uid: -1 },
    { ...policy(), revision: 0 },
    { ...policy(), enabled: "yes" },
    { ...policy(), used_attempts: 1001 },
    { ...policy(), limits: { ...policy().limits, max_turns_per_attempt: 101 } },
    { ...policy(), limits: { ...policy().limits, expires_at: "forever" } },
  ]) expect(() => readExecutionLimits(invalid, "activity-1")).toThrow();
  expect(() => readExecutionLimits(policy(), "another")).toThrow();
});

test("policy writes use CAS revisions without owner, usage or permission selectors", async () => {
  const next = { ...policy(), revision: 2, enabled: false };
  const post = spyOn(api, "post").mockResolvedValue(next);
  expect(await executionLimitsApi.set("activity-1", 1, next.limits)).toEqual(next);
  expect(post).toHaveBeenLastCalledWith("/api/activities/activity-1/execution-limits", {
    expected_revision: 1, limits: next.limits,
  });
  expect(await executionLimitsApi.enable("activity-1", 1, false)).toEqual(next);
  expect(post).toHaveBeenLastCalledWith("/api/activities/activity-1/execution-limits/enabled", {
    expected_revision: 1, enabled: false,
  });
  post.mockResolvedValue(policy());
  await expect(executionLimitsApi.set("activity-1", 1, next.limits)).rejects.toThrow("revision");
  await expect(executionLimitsApi.enable("activity-1", 1, false)).rejects.toThrow("acknowledgement");
});

test("equivalent timestamp normalization is accepted but changed limits are refused", async () => {
  const next = { ...policy(), revision: 2 };
  const post = spyOn(api, "post").mockResolvedValue(next);
  await executionLimitsApi.set("activity-1", 1, { ...next.limits, expires_at: "2099-01-01T01:00:00+01:00" });
  post.mockResolvedValue({ ...next, limits: { ...next.limits, max_attempts: 99 } });
  await expect(executionLimitsApi.set("activity-1", 1, next.limits)).rejects.toThrow("revision");
});
