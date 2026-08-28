import { describe, expect, test } from "bun:test";

import {
  mergeNotificationRecords,
  notificationActionRoute,
  type NotificationRecord,
} from "../src/lib/notifications";

function record(overrides: Partial<NotificationRecord> = {}): NotificationRecord {
  return {
    schema: 1,
    sequence: 1,
    id: "notif-1",
    owner_uid: 1000,
    source: "agent",
    kind: "agent.completed",
    severity: "info",
    title: "Done",
    body: "Finished.",
    delivery_policy: "immediate",
    state: "unread",
    occurrences: 1,
    created_at_ms: 1,
    updated_at_ms: 1,
    actions: [],
    deliveries: [],
    ...overrides,
  };
}

describe("notification state", () => {
  test("updates an existing notification without duplicating it", () => {
    const updated = record({ body: "Updated", occurrences: 2, updated_at_ms: 2 });
    const merged = mergeNotificationRecords([record()], updated);
    expect(merged).toHaveLength(1);
    expect(merged[0].body).toBe("Updated");
    expect(merged[0].occurrences).toBe(2);
  });

  test("removes dismissed notifications", () => {
    expect(
      mergeNotificationRecords([record()], record({ state: "dismissed" })),
    ).toEqual([]);
  });

  test("maps trusted notification actions into local routes", () => {
    expect(notificationActionRoute("clawos://agent/approvals")).toBe("/approvals");
    expect(notificationActionRoute("clawos://agent/session/session-1")).toBe(
      "/chat/session-1",
    );
    expect(notificationActionRoute("https://example.com", "task-1")).toBe("/tasks");
    expect(notificationActionRoute("clawos://agent/session/../bad")).toBeNull();
  });
});
