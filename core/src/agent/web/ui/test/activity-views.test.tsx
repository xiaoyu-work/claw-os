import { afterEach, beforeEach, expect, mock, spyOn, test } from "bun:test";
import { JSDOM } from "jsdom";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";

import { useActivity, useActivities, useActivityObjects } from "../src/hooks/use-activities";
import { activityApi, type ActivityDetail, type ActivityObjects, type ActivityState } from "../src/lib/activities";

function detail(id: string): ActivityDetail {
  return {
    schema: 1,
    activity: {
      id, owner_uid: 1000, title: id, goal: "Review", completion_criteria: "",
      boundaries: "", resources: [], state: "active", completion_note: null,
      created_at: "2026-09-10T12:00:00Z", updated_at: "2026-09-10T12:00:00Z",
    },
    jobs: [], sessions: [],
  };
}

function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (reason: Error) => void;
  const promise = new Promise<T>((done, fail) => { resolve = done; reject = fail; });
  return { promise, resolve, reject };
}

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
  await act(async () => root.unmount());
  dom.window.close();
  for (const key of ["window", "document", "IS_REACT_ACT_ENVIRONMENT"]) {
    if (previous[key]) Object.defineProperty(globalThis, key, previous[key]);
    else Reflect.deleteProperty(globalThis, key);
  }
  mock.restore();
});

function DetailView({ id }: { id: string }) {
  const view = useActivity(id);
  return <div>{view.data?.activity.title ?? "Loading"}{view.error}</div>;
}

function ListView({ state }: { state: ActivityState }) {
  const view = useActivities(state);
  return <div>{view.data?.map((item) => item.title).join(",") ?? "Loading"}{view.error}</div>;
}

function ObjectsView({ id }: { id: string }) {
  const view = useActivityObjects(id);
  return <div>{view.data?.activity_id ?? "Loading"}{view.error}</div>;
}

test("an old object description response cannot replace the selected Activity's metadata", async () => {
  const first = deferred<ActivityObjects>();
  let previousSignal: AbortSignal | undefined;
  spyOn(activityApi, "objects").mockImplementation((id, signal) => {
    if (id === "first") {
      previousSignal = signal;
      return first.promise;
    }
    return Promise.resolve({ schema: 1, activity_id: id, objects: [] });
  });
  await act(async () => root.render(<ObjectsView id="first" />));
  await act(async () => root.render(<ObjectsView id="second" />));
  expect(previousSignal?.aborted).toBe(true);
  expect(container.textContent).toBe("second");
  await act(async () => first.resolve({ schema: 1, activity_id: "first", objects: [] }));
  expect(container.textContent).toBe("second");
});

test("an older Activity response cannot overwrite a newly selected Activity", async () => {
  const first = deferred<ActivityDetail>();
  const second = deferred<ActivityDetail>();
  const signals: AbortSignal[] = [];
  spyOn(activityApi, "get").mockImplementation((id, signal) => {
    signals.push(signal!);
    return id === "first" ? first.promise : second.promise;
  });

  await act(async () => root.render(<DetailView id="first" />));
  await act(async () => root.render(<DetailView id="second" />));
  expect(signals[0].aborted).toBe(true);
  await act(async () => second.resolve(detail("second")));
  expect(container.textContent).toBe("second");
  await act(async () => first.resolve(detail("first")));
  expect(container.textContent).toBe("second");
});

test("stale errors from an old filter do not replace the current list or its error state", async () => {
  const old = deferred<ReturnType<typeof detail>["activity"][]>();
  spyOn(activityApi, "list").mockImplementation((state) =>
    state === "active" ? old.promise : Promise.resolve([detail("paused goal").activity]),
  );
  await act(async () => root.render(<ListView state="active" />));
  await act(async () => root.render(<ListView state="paused" />));
  expect(container.textContent).toBe("paused goal");
  await act(async () => old.reject(new Error("Stale read failed")));
  expect(container.textContent).toBe("paused goal");
});

test("read failures are visible and a successful explicit refresh clears them", async () => {
  const get = spyOn(activityApi, "get").mockRejectedValue(new Error("Broker unavailable"));
  await act(async () => root.render(<DetailView id="first" />));
  expect(container.textContent).toContain("Broker unavailable");
  get.mockResolvedValue(detail("first"));
  await act(async () => dom.window.dispatchEvent(new dom.window.Event("focus")));
  expect(container.textContent).toBe("first");
});
