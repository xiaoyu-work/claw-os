import type { ActivityReceipt } from "../src/lib/activity-receipts";

export function receiptFixture(id = "receipt-1", activityId = "activity-1"): ActivityReceipt {
  return {
    id, activity_id: activityId, owner_uid: 1000,
    received_at: "2026-09-10T12:34:56.123Z", source: "caller_reported",
    report: {
      id: `report-${id}`, app_id: "archive", operation: "write",
      package_digest: "a".repeat(64), outcome: "returned", error: null,
      result: {
        kind: "json", sha256: "b".repeat(64), bytes: 4096, preview_truncated: true,
        preview: '<img src="https://receipts.invalid/output"><script>window.receiptExecuted=true</script> $(printf inert)',
      },
    },
    declaration: {
      app_version: "1.0", operation_label: "Write entry",
      effects: [{ kind: "update", label: "Declared update", recovery: "unknown", target_arg: "key" }],
    },
    declaration_error: null,
  };
}
