import { afterEach, describe, expect, mock, spyOn, test } from "bun:test";

import { api } from "../src/lib/api";
import {
  activityApi,
  hasLiveActivityJobs,
  readActivityDetail,
  readActivityList,
  readActivityObjects,
  type Activity,
  type ActivityDraft,
  type ActivityJob,
  type ObjectDescription,
} from "../src/lib/activities";

const draft: ActivityDraft = {
  title: "Prepare a release",
  goal: "Review the deliverable",
  completion_criteria: "I checked the release",
  boundaries: "Do not publish without approval",
  resources: [{ label: "Notes", reference: "javascript:neverExecute()" }],
};
const activity: Activity = {
  ...draft,
  id: "activity-1",
  owner_uid: 1000,
  state: "active",
  completion_note: null,
  created_at: "2026-09-10T12:00:00Z",
  updated_at: "2026-09-10T12:00:00Z",
};
const job: ActivityJob = {
  id: "job-1",
  title: "Review",
  status: "ok",
  session_id: "session-1",
  created_at: "2026-09-10T12:00:00Z",
  finished_at: "2026-09-10T12:01:00Z",
  response: "The draft is ready",
  error: null,
  waiting_on: [],
};

afterEach(() => mock.restore());

describe("Activity presentation contract", () => {
  test("a successful job leaves Activity completion entirely to the fetched state", () => {
    const detail = readActivityDetail({
      schema: 1, activity, jobs: [job], sessions: ["session-1"],
    }, activity.id);
    expect(detail.activity.state).toBe("active");
    expect(detail.activity.completion_note).toBeNull();
    expect(detail.jobs[0].status).toBe("ok");
    expect(detail.activity.resources).toEqual(draft.resources);
  });

  const objectDescription: ObjectDescription = {
    object: { app_id: "archive", object_type: "entry", object_id: " release.status /?#& ", revision: "rev 2" },
    reference: "app://archive/entry?id=%20release.status%20%2F%3F%23%26%20&revision=rev%202",
    app_name: "Archive",
    app_version: "1.0.0",
    object_label: "Release entry",
    object_summary: '<img src="https://invalid.example" onerror="execute()">',
    invocation: { app_id: "archive", operation: "get", args: ["--revision=rev 2", "--", " release.status /?#& "] },
  };
  const declaredObject = {
    label: "Release status", reference: objectDescription.reference,
    status: "declared", description: objectDescription, error: null,
  };

  describe("Activity App object presentation", () => {
    test("accepts only the broker's declaration statuses and preserves diagnostics", () => {
      const objects = [
        { ...declaredObject, description: { ...objectDescription, provenance: { publisher: "fixture" } } },
        { label: "Missing", reference: "app://missing/entry?id=x", status: "unavailable", description: null, error: "App is quarantined" },
        { label: "Invalid", reference: "app:invalid", status: "invalid", description: null, error: "Noncanonical reference" },
      ];
      const result = readActivityObjects({ schema: 1, activity_id: activity.id, objects }, activity.id);
      expect(result.objects.map((entry) => entry.status)).toEqual(["declared", "unavailable", "invalid"]);
      expect(result.objects[1].error).toBe("App is quarantined");
      expect(result.objects[0].description?.object_summary).toBe(objectDescription.object_summary);
    });

    test("rejects malformed, mismatched, and undeclared descriptions rather than trusting them", () => {
      for (const entry of [
        { ...declaredObject, status: "verified" },
        { ...declaredObject, description: null },
        { ...declaredObject, status: "unavailable", error: "Revoked" },
        { ...declaredObject, status: "invalid", error: "Malformed" },
        { ...declaredObject, description: { ...objectDescription, reference: "another object" } },
        { ...declaredObject, description: { ...objectDescription, object: { ...objectDescription.object, object_id: 2 } } },
        { ...declaredObject, description: { ...objectDescription, invocation: { app_id: "another", operation: "get", args: [] } } },
        { ...declaredObject, description: { ...objectDescription, invocation: { app_id: "archive", operation: "get", args: "shell command" } } },
      ]) {
        expect(() => readActivityObjects({
          schema: 1, activity_id: activity.id, objects: [entry],
        }, activity.id)).toThrow("Invalid App object descriptions");
      }
      expect(() => readActivityObjects({ schema: 1, activity_id: "other", objects: [] }, activity.id)).toThrow();
      expect(() => readActivityObjects({ schema: 2, activity_id: activity.id, objects: [] }, activity.id)).toThrow();
      expect(() => readActivityObjects({ error: "unavailable" }, activity.id)).toThrow();
    });

    test("sends typed opaque fields only and takes the canonical resource from the backend", async () => {
      const attachment = { label: "Release status", object: objectDescription.object };
      const updated = { ...activity, resources: [...activity.resources, { label: attachment.label, reference: objectDescription.reference }] };
      const post = spyOn(api, "post").mockResolvedValue(updated);
      expect(await activityApi.attachObject(activity.id, attachment)).toEqual(updated);
      expect(post).toHaveBeenCalledTimes(1);
      expect(post).toHaveBeenLastCalledWith("/api/activities/activity-1/objects", attachment);
      expect(post.mock.calls[0][1]).not.toHaveProperty("reference");
      expect(post.mock.calls[0][1]).not.toHaveProperty("resources");
      expect(post.mock.calls[0][1]).not.toHaveProperty("owner_uid");
      expect(activity.resources).toEqual(draft.resources);
      const unpinned = {
        label: "Unpinned",
        object: { app_id: "archive", object_type: "entry", object_id: " release.status /?#& " },
      };
      await activityApi.attachObject(activity.id, unpinned);
      expect(post).toHaveBeenLastCalledWith("/api/activities/activity-1/objects", unpinned);
      expect(post.mock.calls.at(-1)?.[1].object).not.toHaveProperty("revision");
      post.mockResolvedValue({ ...updated, id: "another" });
      await expect(activityApi.attachObject(activity.id, attachment)).rejects.toThrow("Invalid attached Activity");
    });

    test("metadata reads use the authenticated adapter and surface failures without resolution", async () => {
      const controller = new AbortController();
      const result = { schema: 1, activity_id: activity.id, objects: [declaredObject] };
      const get = spyOn(api, "get").mockResolvedValue(result);
      const post = spyOn(api, "post");
      expect(await activityApi.objects(activity.id, controller.signal)).toEqual(result);
      expect(get).toHaveBeenCalledTimes(1);
      expect(get).toHaveBeenLastCalledWith("/api/activities/activity-1/objects", { signal: controller.signal });
      expect(post).not.toHaveBeenCalled();
      get.mockRejectedValue(new Error("Object catalogue unavailable"));
      await expect(activityApi.objects(activity.id)).rejects.toThrow("Object catalogue unavailable");
    });
  });

  test("invalid or mismatched responses fail rather than becoming empty or local state", () => {
    expect(() => readActivityList({ error: "unavailable" })).toThrow("Invalid Activity list");
    expect(() => readActivityList({ schema: 2, activities: [] })).toThrow();
    expect(() => readActivityList({ schema: 1, activities: [{ ...activity, state: "running" }] })).toThrow();
    expect(() => readActivityDetail({
      schema: 1, activity, jobs: [job], sessions: [],
    }, "another-activity")).toThrow("Invalid Activity detail");
    expect(() => readActivityDetail({
      schema: 1, activity, jobs: [{ ...job, waiting_on: "approval-1" }], sessions: [],
    }, activity.id)).toThrow();
  });

  test("queued, running, and waiting jobs request live refreshes, not inferred completion", () => {
    for (const status of ["pending", "running", "waiting_approval"] as const) {
      expect(hasLiveActivityJobs([{ ...job, status }])).toBe(true);
    }
    expect(hasLiveActivityJobs([job])).toBe(false);
    expect(hasLiveActivityJobs([{ ...job, status: "error" }])).toBe(false);
    expect(hasLiveActivityJobs([])).toBe(false);
  });

  test("create and edit use the existing authenticated API without owner fields", async () => {
    const post = spyOn(api, "post").mockResolvedValue(activity);
    await activityApi.create(draft);
    expect(post).toHaveBeenLastCalledWith("/api/activities", draft);
    await activityApi.update(activity.id, { ...draft, boundaries: "", resources: [] });
    expect(post).toHaveBeenLastCalledWith("/api/activities/activity-1/update", {
      ...draft, boundaries: "", resources: [],
    });
    expect(post.mock.calls[0][1]).not.toHaveProperty("owner_uid");
  });

  test("state changes forward only the selected state and explicit confirmation note", async () => {
    const post = spyOn(api, "post").mockResolvedValue(activity);
    await activityApi.transition(activity.id, "paused");
    expect(post).toHaveBeenLastCalledWith("/api/activities/activity-1/transition", { state: "paused" });
    await activityApi.transition(activity.id, "completed", "I verified the result.");
    expect(post).toHaveBeenLastCalledWith("/api/activities/activity-1/transition", {
      state: "completed", completion_note: "I verified the result.",
    });
  });

  test("continuation forwards the existing session and returns the submitted job", async () => {
    const submitted = { id: "job-2", status: "pending", activity_id: activity.id, session_id: "session-1" };
    const post = spyOn(api, "post").mockResolvedValue(submitted);
    expect(await activityApi.run(activity.id, {
      prompt: "Continue", session_id: "session-1", use_memory: false,
    })).toEqual(submitted);
    expect(post).toHaveBeenLastCalledWith("/api/activities/activity-1/run", {
      prompt: "Continue", session_id: "session-1", use_memory: false,
    });
    post.mockResolvedValue({ ...submitted, activity_id: "other" });
    await expect(activityApi.run(activity.id, {})).rejects.toThrow("Invalid submitted Activity job");
  });

  test("reads forward filters and abort signals, and expose broker failures", async () => {
    const controller = new AbortController();
    const get = spyOn(api, "get").mockResolvedValue({ schema: 1, activities: [activity] });
    expect(await activityApi.list("paused", controller.signal)).toEqual([activity]);
    expect(get).toHaveBeenLastCalledWith("/api/activities?limit=100&state=paused", {
      signal: controller.signal,
    });
    get.mockRejectedValue(new Error("Activity service unavailable"));
    await expect(activityApi.get(activity.id)).rejects.toThrow("Activity service unavailable");
    get.mockResolvedValue({ schema: 1, activity: { ...activity, id: "id with space" }, jobs: [], sessions: [] });
    await activityApi.get("id with space", controller.signal);
    expect(get).toHaveBeenLastCalledWith("/api/activities/id%20with%20space?limit=100", {
      signal: controller.signal,
    });
  });
});
