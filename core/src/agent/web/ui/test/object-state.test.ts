import { afterEach, expect, mock, spyOn, test } from "bun:test";

import { activityApi } from "../src/lib/activities";
import { api } from "../src/lib/api";
import { readObjectState, readObjectStateEntry, type ObjectStateContent } from "../src/lib/object-state";
import { OBJECT_ENTRY_ID, OBJECT_NEXT_ID, objectStateFixture } from "./object-state-fixtures";
import { receiptFixture } from "./receipt-fixtures";

afterEach(() => mock.restore());
const envelope = (entries: unknown[]) => ({ schema: 1, activity_id: "activity-1", entries });

test("object-state content stays classified data, including linked App uncertainty", () => {
  const contents: ObjectStateContent[] = [
    { kind: "user_statement", text: "User report" },
    { kind: "agent_inference", text: "Possibly ready" },
    { kind: "relation", relation: "depends_on", target: "app://archive/entry?id=review", note: "" },
    { kind: "retracted", reason: "Unsupported" },
    { kind: "app_report", receipt_id: OBJECT_NEXT_ID },
  ];
  for (const content of contents) {
    const entry = objectStateFixture();
    entry.draft.content = content;
    if (content.kind === "retracted") entry.draft.supersedes = OBJECT_NEXT_ID;
    if (content.kind === "app_report") {
      entry.receipt = {
        ...receiptFixture().report, id: OBJECT_NEXT_ID,
        outcome: "indeterminate", result: null, error: "Result was lost",
      };
    }
    expect(readObjectState(envelope([entry]), "activity-1").entries).toEqual([entry]);
  }
});

test("invalid source, identity, bounds and mismatched receipt evidence are refused", () => {
  const entry = objectStateFixture();
  for (const invalid of [
    { ...entry, source: "os_confirmed" },
    { ...entry, verified: true },
    { ...entry, activity_id: "other" },
    { ...entry, owner_uid: -1 },
    { ...entry, owner_uid: 0x1_0000_0000 },
    { ...entry, id: OBJECT_NEXT_ID },
    { ...entry, recorded_at: "yesterday" },
    { ...entry, validity: "verified_fresh" },
    { ...entry, superseded_by: entry.id },
    { ...entry, draft: { ...entry.draft, source: "user_authenticated" } },
    { ...entry, draft: { ...entry.draft, content: { kind: "user_statement", text: "x".repeat(4097) } } },
    { ...entry, draft: { ...entry.draft, content: { kind: "agent_inference", text: "é".repeat(2049) } } },
    { ...entry, draft: { ...entry.draft, content: { kind: "retracted", reason: "bad" } } },
    { ...entry, draft: { ...entry.draft, content: { kind: "app_report", receipt_id: OBJECT_NEXT_ID } } },
    { ...entry, receipt: receiptFixture().report },
  ]) expect(() => readObjectState(envelope([invalid]), "activity-1")).toThrow();
  expect(() => readObjectState(envelope([entry, entry]), "activity-1")).toThrow();
  expect(() => readObjectState({ ...envelope([]), schema: 2 }, "activity-1")).toThrow();
  expect(() => readObjectStateEntry(entry, "another")).toThrow();
});

test("reported time windows do not become semantic truth and corrections keep history", () => {
  const entry = objectStateFixture();
  entry.draft.observed_at = "2026-09-10T00:00:00Z";
  entry.draft.valid_until = "2026-09-11T00:00:00Z";
  entry.validity = "expired";
  entry.superseded_by = OBJECT_NEXT_ID;
  expect(readObjectStateEntry(entry, "activity-1")).toEqual(entry);
  const invalid = structuredClone(entry);
  invalid.draft.valid_until = invalid.draft.observed_at;
  expect(() => readObjectStateEntry(invalid, "activity-1")).toThrow();
  invalid.draft.valid_until = null;
  expect(() => readObjectStateEntry(invalid, "activity-1")).toThrow();
});

test("reads and submissions use the same authenticated Activity and idempotent entry identity", async () => {
  const entry = objectStateFixture();
  const get = spyOn(api, "get").mockResolvedValue(envelope([entry]));
  const post = spyOn(api, "post").mockResolvedValue(entry);
  const signal = new AbortController().signal;
  await activityApi.objectState("activity-1", signal);
  expect(get).toHaveBeenLastCalledWith("/api/activities/activity-1/object-state?limit=100", { signal });
  expect(await activityApi.recordObjectState("activity-1", entry.draft)).toEqual(entry);
  expect(post).toHaveBeenLastCalledWith("/api/activities/activity-1/object-state", { entry: entry.draft });
  const wrong = objectStateFixture(OBJECT_NEXT_ID);
  post.mockResolvedValue(wrong);
  await expect(activityApi.recordObjectState("activity-1", entry.draft)).rejects.toThrow("does not match");
  expect(entry.id).toBe(OBJECT_ENTRY_ID);
});
