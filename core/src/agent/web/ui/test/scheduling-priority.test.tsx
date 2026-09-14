import { afterEach, beforeEach, expect, mock, spyOn, test } from "bun:test";
import { JSDOM } from "jsdom";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";

import { ActivitySchedulingPriorityPanel } from "../src/components/activity-scheduling-priority";
import { useActivitySchedulingPriority } from "../src/hooks/use-activities";
import { api } from "../src/lib/api";
import {
  readSchedulingPolicy,
  readSchedulingPriorityView,
  schedulingPriorityApi,
  type ActivitySchedulingPolicy,
} from "../src/lib/scheduling-priority";

const policy = (): ActivitySchedulingPolicy => ({
  activity_id: "activity-1",
  owner_uid: 1000,
  revision: "9007199254740993",
  priority: "foreground",
  created_at: "2026-09-13T00:00:00Z",
  updated_at: "2026-09-13T01:00:00Z",
});

test("scheduling priority uses a closed precision-safe owner-scoped contract", async () => {
  expect(readSchedulingPriorityView({
    schema: 1, activity_id: "activity-1", scheduling_policy: null,
  }, "activity-1").scheduling_policy).toBeNull();
  expect(readSchedulingPolicy(policy(), "activity-1").revision).toBe("9007199254740993");
  for (const invalid of [
    { ...policy(), owner_uid: -1 },
    { ...policy(), revision: 7 },
    { ...policy(), priority: "urgent" },
    { ...policy(), created_at: "invalid" },
    { ...policy(), authority: true },
  ]) expect(() => readSchedulingPolicy(invalid, "activity-1")).toThrow();
  expect(() => readSchedulingPriorityView({
    schema: 1, activity_id: "activity-1",
  }, "activity-1")).toThrow();

  const saved = { ...policy(), revision: "9007199254740994", priority: "background" as const };
  const post = spyOn(api, "post").mockResolvedValue(saved);
  expect(await schedulingPriorityApi.set(
    "activity-1", 1000, "9007199254740993", "background", policy(),
  )).toEqual(saved);
  expect(post).toHaveBeenCalledWith("/api/activities/activity-1/scheduling-priority", {
    expected_revision: "9007199254740993",
    priority: "background",
  });
  expect(JSON.stringify(post.mock.calls)).not.toContain("owner_uid");
  expect(JSON.stringify(post.mock.calls)).not.toContain("job");
  expect(JSON.stringify(post.mock.calls)).not.toContain("preempt");
  post.mockResolvedValue({ ...saved, revision: "9007199254740995" });
  await expect(schedulingPriorityApi.set(
    "activity-1", 1000, "9007199254740993", "background", policy(),
  )).rejects.toThrow("exact revision");
});

let dom: JSDOM;
let root: Root;
let container: HTMLElement;
let previous: PropertyDescriptorMap;

beforeEach(() => {
  previous = Object.getOwnPropertyDescriptors(globalThis);
  dom = new JSDOM('<div id="root"></div>', { url: "http://localhost/" });
  Object.defineProperties(globalThis, {
    window: { configurable: true, value: dom.window },
    document: { configurable: true, value: dom.window.document },
    IS_REACT_ACT_ENVIRONMENT: { configurable: true, value: true },
  });
  container = dom.window.document.getElementById("root")!;
  root = createRoot(container);
});

afterEach(async () => {
  if (root) await act(async () => root.unmount());
  if (dom) dom.window.close();
  for (const key of ["window", "document", "IS_REACT_ACT_ENVIRONMENT"]) {
    if (previous?.[key]) Object.defineProperty(globalThis, key, previous[key]);
    else Reflect.deleteProperty(globalThis, key);
  }
  mock.restore();
});

function button(text: string): HTMLButtonElement {
  const element = Array.from(container.querySelectorAll("button"))
    .find((item) => item.textContent?.trim() === text);
  if (!element) throw new Error(`Missing button: ${text}`);
  return element;
}

function Panel() {
  const view = useActivitySchedulingPriority("activity-1");
  return <ActivitySchedulingPriorityPanel activityId="activity-1" ownerUid={1000}
    editable disabled={false} view={view} mutate={async (action) => {
      try {
        await action();
        return (await view.refresh()) !== null;
      } catch {
        return false;
      }
    }} />;
}

test("priority controls explain admission semantics and retain exact-CAS drafts on failure", async () => {
  spyOn(schedulingPriorityApi, "get").mockResolvedValue({
    schema: 1, activity_id: "activity-1", scheduling_policy: policy(),
  });
  const save = spyOn(schedulingPriorityApi, "set")
    .mockRejectedValue(new Error("Scheduling policy revision conflict"));
  await act(async () => root.render(<Panel />));
  expect(container.textContent).toContain("30 minutes");
  expect(container.textContent).toContain("grants no authority");
  expect(container.textContent).toContain("Current priority: Foreground");
  await act(async () => button("Edit scheduling priority").click());
  await act(async () => button("Background").click());
  await act(async () => button("Save scheduling priority").click());
  expect(save).toHaveBeenCalledWith(
    "activity-1", 1000, "9007199254740993", "background", policy(),
  );
  expect(button("Background").getAttribute("aria-pressed")).toBe("true");
  expect(container.textContent).toContain("Editing revision 9007199254740993");
});
