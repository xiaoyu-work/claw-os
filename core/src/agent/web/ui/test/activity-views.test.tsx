import { afterEach, beforeEach, expect, mock, spyOn, test } from "bun:test";
import { JSDOM } from "jsdom";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";

import { OperationEffects } from "../src/components/operation-effects";
import { ActivityReceiptsPanel } from "../src/components/activity-receipts";
import { useActivity, useActivities, useActivityObjects, useActivityReceipts } from "../src/hooks/use-activities";
import { activityApi, type ActivityDetail, type ActivityObjects, type ActivityState } from "../src/lib/activities";
import type { ActivityReceipts } from "../src/lib/activity-receipts";
import type { OperationPreview } from "../src/lib/operation-preview";
import { receiptFixture } from "./receipt-fixtures";

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

function ReceiptsView({ id }: { id: string }) {
  return <ActivityReceiptsPanel view={useActivityReceipts(id)} ownerUid={1000} />;
}

test("receipts display inert caller reports and recording metadata without authoring or authority", async () => {
  const receipt = receiptFixture();
  spyOn(activityApi, "receipts").mockResolvedValue({ schema: 1, activity_id: "activity-1", receipts: [receipt] });
  const transition = spyOn(activityApi, "transition");
  const run = spyOn(activityApi, "run");
  await act(async () => root.render(<ReceiptsView id="activity-1" />));
  expect(container.textContent).toContain("Returned (caller report)");
  expect(container.textContent).toContain("Caller-reported (not an execution attestation)");
  expect(container.textContent).toContain("Preview truncated");
  expect(container.textContent).toContain("not OS execution proof");
  expect(container.textContent).toContain("App-declared update");
  expect(container.textContent).toContain("Authenticated at recording time only");
  expect(container.textContent).toContain("does not establish current App validity");
  expect(container.querySelector("time")?.getAttribute("datetime")).toBe(receipt.received_at);
  expect(container.querySelector("pre")?.textContent).toBe(receipt.report.result?.preview);
  expect(container.querySelectorAll("img,script,a,input,textarea")).toHaveLength(0);
  expect(container.querySelectorAll("button")).toHaveLength(1);
  expect(transition).not.toHaveBeenCalled();
  expect(run).not.toHaveBeenCalled();
});

test("receipt errors and indeterminate outcomes remain visible without implying no side effects", async () => {
  const receipt = receiptFixture();
  spyOn(activityApi, "receipts").mockResolvedValue({
    schema: 1, activity_id: "activity-1",
    receipts: [{
      ...receipt, report: { ...receipt.report, outcome: "reported_error", result: null, error: "Reported failure" },
      declaration: null, declaration_error: "Matching App package unavailable",
    }, {
      ...receiptFixture("uncertain"), report: { ...receipt.report, outcome: "indeterminate", error: "Capture uncertain" },
    }],
  });
  await act(async () => root.render(<ReceiptsView id="activity-1" />));
  expect(container.textContent).toContain("Reported error (caller report)");
  expect(container.textContent).toContain("Indeterminate (caller report)");
  expect(container.textContent).toContain("Matching App package unavailable");
  expect(container.textContent).toContain("does not prove that no side effect occurred");
});

test("receipt owner mismatches and read failures hide prior reports", async () => {
  const receipt = receiptFixture();
  const read = spyOn(activityApi, "receipts").mockResolvedValue({
    schema: 1, activity_id: "activity-1", receipts: [{ ...receipt, owner_uid: 1001 }],
  });
  await act(async () => root.render(<ReceiptsView id="activity-1" />));
  expect(container.querySelector('[role="alert"]')?.textContent).toContain("Receipt owner does not match");
  expect(container.querySelectorAll("article")).toHaveLength(0);
  read.mockResolvedValue({ schema: 1, activity_id: "activity-1", receipts: [receipt] });
  await act(async () => container.querySelector("button")!.click());
  expect(container.querySelectorAll("article")).toHaveLength(1);
  read.mockRejectedValue(new Error("Receipt ledger unavailable"));
  await act(async () => container.querySelector("button")!.click());
  expect(container.querySelector('[role="alert"]')?.textContent).toContain("Receipt ledger unavailable");
  expect(container.querySelectorAll("article")).toHaveLength(0);
});

test("an older receipt response cannot replace a newly selected Activity's reports", async () => {
  const first = deferred<ActivityReceipts>();
  let oldSignal: AbortSignal | undefined;
  spyOn(activityApi, "receipts").mockImplementation((id, signal) => {
    if (id === "first") { oldSignal = signal; return first.promise; }
    return Promise.resolve({ schema: 1, activity_id: id, receipts: [receiptFixture("second-receipt", id)] });
  });
  await act(async () => root.render(<ReceiptsView id="first" />));
  await act(async () => root.render(<ReceiptsView id="second" />));
  expect(oldSignal?.aborted).toBe(true);
  await act(async () => first.resolve({ schema: 1, activity_id: "first", receipts: [receiptFixture("first-receipt", "first")] }));
  expect(container.textContent).toContain("second-receipt");
  expect(container.textContent).not.toContain("first-receipt");
});

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

const previewProps = {
  activityId: "activity-1", objectReference: "app://archive/entry?id=first", appVersion: "1.0",
  invocation: { app_id: "archive", operation: "get", args: ["--message=ARGUMENT_ONLY_PAYLOAD; $(printf inert)"] },
};
function effects(label = "Declared removal"): OperationPreview {
  return {
    schema: 1, app_id: "archive", app_name: "Archive", app_version: "1.0",
    package_digest: "fixture-digest", operation: "get", operation_label: "Inspect entry",
    effects_declared: true,
    effects: [{
      kind: "delete", label, recovery: "irreversible", target_arg: "path", target_kind: "path",
      requested_targets: ["../requested/draft.txt"], target_state: "requested",
    }],
    unresolved_arguments: ["provider"],
    authorization_checked: false, executed: false, effects_confirmed: false,
    notes: ["Metadata only"],
  };
}

async function clickPreview() {
  const button = container.querySelector("button");
  expect(button).not.toBeNull();
  await act(async () => button!.click());
}

test("effect previews are explicit, inert metadata rather than argument content or authority", async () => {
  const preview = spyOn(activityApi, "previewOperation").mockResolvedValue(effects('<img src="https://invalid.example">'));
  await act(async () => root.render(<OperationEffects {...previewProps} />));
  await act(async () => dom.window.dispatchEvent(new dom.window.Event("focus")));
  expect(preview).not.toHaveBeenCalled();
  await clickPreview();
  expect(preview).toHaveBeenCalledTimes(1);
  expect(container.textContent).toContain("App-declared delete");
  expect(container.textContent).toContain("Irreversible (App-declared)");
  expect(container.textContent).toContain("Authorization not checked");
  expect(container.textContent).toContain("not final canonical paths");
  expect(container.textContent).toContain("Unresolved runtime arguments");
  expect(container.textContent).not.toContain("ARGUMENT_ONLY_PAYLOAD");
  expect(container.querySelectorAll("img,script,a")).toHaveLength(0);
  expect(container.querySelectorAll("button")).toHaveLength(1);
});

test("preview errors hide earlier effects and missing declarations stay explicitly unknown", async () => {
  const preview = spyOn(activityApi, "previewOperation").mockResolvedValue(effects());
  await act(async () => root.render(<OperationEffects {...previewProps} />));
  await clickPreview();
  expect(container.textContent).toContain("Declared removal");
  preview.mockRejectedValue(new Error("App not trusted"));
  await clickPreview();
  expect(container.querySelector('[role="alert"]')?.textContent).toBe("App not trusted");
  expect(container.textContent).not.toContain("Declared removal");
  preview.mockResolvedValue({ ...effects(), effects_declared: false, effects: [] });
  await clickPreview();
  expect(container.textContent).toContain("Effects unknown");
  expect(container.textContent).toContain("not a read-only guarantee");
});

for (const { name, next } of [
  { name: "Activity", next: { ...previewProps, activityId: "activity-2" } },
  { name: "object", next: { ...previewProps, objectReference: "app://archive/entry?id=second" } },
  { name: "App version", next: { ...previewProps, appVersion: "2.0" } },
  { name: "invocation", next: { ...previewProps, invocation: { ...previewProps.invocation, args: ["different"] } } },
]) {
  test(`a late preview cannot overwrite a changed ${name}`, async () => {
    const first = deferred<OperationPreview>();
    let oldSignal: AbortSignal | undefined;
    const preview = spyOn(activityApi, "previewOperation").mockImplementationOnce((_id, _input, signal) => {
      oldSignal = signal;
      return first.promise;
    }).mockResolvedValue(effects("Current effect"));
    await act(async () => root.render(<OperationEffects {...previewProps} />));
    await clickPreview();
    await act(async () => root.render(<OperationEffects {...next} />));
    expect(oldSignal?.aborted).toBe(true);
    expect(preview).toHaveBeenCalledTimes(1);
    await clickPreview();
    expect(container.textContent).toContain("Current effect");
    await act(async () => first.resolve(effects("Obsolete effect")));
    expect(container.textContent).not.toContain("Obsolete effect");
    expect(container.textContent).toContain("Current effect");
  });
}
