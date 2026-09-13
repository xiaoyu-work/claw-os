import { afterEach, describe, expect, mock, spyOn, test } from "bun:test";

import { activityApi } from "../src/lib/activities";
import { readActivityReceipts } from "../src/lib/activity-receipts";
import { api } from "../src/lib/api";
import { receiptFixture } from "./receipt-fixtures";

const receipt = receiptFixture();
const envelope = (receipts: unknown[]) => ({ schema: 1, activity_id: "activity-1", receipts });

afterEach(() => mock.restore());

describe("Activity receipt read contract", () => {
  test("preserves caller outcomes, uncertain results and declaration failures without inference", () => {
    const receipts = [
      receipt,
      {
        ...receiptFixture("receipt-error"),
        report: { ...receipt.report, outcome: "reported_error", result: null, error: "The App reported an error" },
        declaration: null, declaration_error: "Reported package is unavailable",
      },
      {
        ...receiptFixture("receipt-uncertain"),
        report: { ...receipt.report, outcome: "indeterminate", error: "Result could not be captured" },
      },
    ];
    const result = readActivityReceipts(envelope(receipts), "activity-1");
    expect(result.receipts.map((item) => item.report.outcome)).toEqual(["returned", "reported_error", "indeterminate"]);
    expect(result.receipts[1].declaration_error).toBe("Reported package is unavailable");
    expect(result.receipts[0].report.result).toEqual(receipt.report.result);
    expect(result.receipts[2].report.result).not.toBeNull();
  });

  test("rejects forged provenance, wrong Activity identity and invalid recording metadata", () => {
    for (const invalid of [
      { ...receipt, source: "os_confirmed" },
      { ...receipt, source: undefined },
      { ...receipt, activity_id: "another" },
      { ...receipt, id: "" },
      { ...receipt, owner_uid: -1 },
      { ...receipt, owner_uid: "1000" },
      { ...receipt, owner_uid: 0x1_0000_0000 },
      { ...receipt, received_at: "not a timestamp" },
      { ...receipt, report: { ...receipt.report, id: "" } },
      { ...receipt, report: { ...receipt.report, outcome: "success_applied" } },
    ]) {
      expect(() => readActivityReceipts(envelope([invalid]), "activity-1")).toThrow("Invalid Activity receipts");
    }
    expect(() => readActivityReceipts({ ...envelope([]), activity_id: "other" }, "activity-1")).toThrow();
    expect(() => readActivityReceipts({ ...envelope([]), schema: 2 }, "activity-1")).toThrow();
    expect(() => readActivityReceipts(envelope([receipt, receipt]), "activity-1")).toThrow();
  });

  test("requires exactly one declaration or declaration error and validates result shapes", () => {
    for (const invalid of [
      { ...receipt, declaration: null, declaration_error: null },
      { ...receipt, declaration_error: "Cannot both match and fail" },
      { ...receipt, declaration: { ...receipt.declaration, effects: [{ kind: "os_applied" }] } },
      { ...receipt, report: { ...receipt.report, result: { ...receipt.report.result, kind: "html" } } },
      { ...receipt, report: { ...receipt.report, result: { ...receipt.report.result, bytes: -1 } } },
      { ...receipt, report: { ...receipt.report, result: { ...receipt.report.result, bytes: 1.5 } } },
      { ...receipt, report: { ...receipt.report, result: { ...receipt.report.result, bytes: Number.MAX_SAFE_INTEGER + 1 } } },
      { ...receipt, report: { ...receipt.report, result: { ...receipt.report.result, preview: {} } } },
      { ...receipt, report: { ...receipt.report, result: { ...receipt.report.result, preview_truncated: "true" } } },
    ]) {
      expect(() => readActivityReceipts(envelope([invalid]), "activity-1")).toThrow();
    }
    for (const kind of ["json", "text", "empty"]) {
      const value = { ...receipt, report: { ...receipt.report, result: { ...receipt.report.result, kind } } };
      expect(readActivityReceipts(envelope([value]), "activity-1").receipts).toHaveLength(1);
    }
  });

  test("reads only through the authenticated endpoint and forwards limits/signals, never authors receipts", async () => {
    const get = spyOn(api, "get").mockResolvedValue(envelope([receipt]));
    const post = spyOn(api, "post");
    const controller = new AbortController();
    expect(await activityApi.receipts("activity-1", controller.signal)).toEqual(envelope([receipt]));
    expect(get).toHaveBeenLastCalledWith("/api/activities/activity-1/receipts?limit=100", { signal: controller.signal });
    expect(post).not.toHaveBeenCalled();
    get.mockRejectedValue(new Error("Receipt ledger unavailable"));
    await expect(activityApi.receipts("activity-1")).rejects.toThrow("Receipt ledger unavailable");
  });
});
