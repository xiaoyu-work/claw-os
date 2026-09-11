import { api } from "@/lib/api";

export const ACTIVITY_STATES = ["active", "paused", "completed", "cancelled"] as const;
export type ActivityState = (typeof ACTIVITY_STATES)[number];
export type ActivityResource = { label: string; reference: string };
export type ActivityDraft = {
  title: string;
  goal: string;
  completion_criteria: string;
  boundaries: string;
  resources: ActivityResource[];
};
export type Activity = ActivityDraft & {
  id: string;
  owner_uid: number;
  state: ActivityState;
  completion_note: string | null;
  created_at: string;
  updated_at: string;
};
export type JobStatus = "pending" | "running" | "waiting_approval" | "ok" | "error" | "cancelled";
export type ActivityJob = {
  id: string;
  title: string;
  status: JobStatus;
  session_id: string | null;
  created_at: string;
  finished_at: string | null;
  response: string | null;
  error: string | null;
  waiting_on: string[];
};
export type ActivityDetail = {
  schema: 1;
  activity: Activity;
  jobs: ActivityJob[];
  sessions: string[];
};
export type ActivityRun = {
  prompt?: string;
  session_id?: string;
  max_turns?: number;
  use_memory?: boolean;
};
export type AppObjectRef = {
  app_id: string;
  object_type: string;
  object_id: string;
  revision?: string;
};
export type ActivityObjectAttachment = { label: string; object: AppObjectRef };
export type ObjectDescription = {
  object: AppObjectRef;
  reference: string;
  app_name: string;
  app_version: string;
  object_label: string;
  object_summary: string;
  invocation: { app_id: string; operation: string; args: string[] };
};
export type ActivityObject = { label: string; reference: string } & (
  | { status: "declared"; description: ObjectDescription; error: null }
  | { status: "unavailable" | "invalid"; description: null; error: string }
);
export type ActivityObjects = {
  schema: 1;
  activity_id: string;
  objects: ActivityObject[];
};
type SubmittedJob = {
  id: string;
  status: JobStatus;
  activity_id: string;
  session_id?: string | null;
};

export const activityStateLabels: Record<ActivityState, string> = {
  active: "Active",
  paused: "Paused",
  completed: "Completed",
  cancelled: "Cancelled",
};
export const jobStatusLabels: Record<JobStatus, string> = {
  pending: "Queued",
  running: "Running",
  waiting_approval: "Waiting for approval",
  ok: "Succeeded",
  error: "Failed",
  cancelled: "Cancelled",
};

export function hasLiveActivityJobs(jobs: ActivityJob[]): boolean {
  return jobs.some(({ status }) =>
    status === "pending" || status === "running" || status === "waiting_approval",
  );
}

function record(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function nullableString(value: unknown): value is string | null {
  return value === null || typeof value === "string";
}

function strings(value: unknown): value is string[] {
  return Array.isArray(value) && value.every((item) => typeof item === "string");
}

export function isActivityState(value: unknown): value is ActivityState {
  return ACTIVITY_STATES.some((state) => state === value);
}

function isJobStatus(value: unknown): value is JobStatus {
  return value === "pending" || value === "running" || value === "waiting_approval"
    || value === "ok" || value === "error" || value === "cancelled";
}

function isActivity(value: unknown): value is Activity {
  return record(value)
    && typeof value.id === "string"
    && typeof value.owner_uid === "number"
    && typeof value.title === "string"
    && typeof value.goal === "string"
    && typeof value.completion_criteria === "string"
    && typeof value.boundaries === "string"
    && Array.isArray(value.resources)
    && value.resources.every((item) =>
      record(item) && typeof item.label === "string" && typeof item.reference === "string",
    )
    && isActivityState(value.state)
    && nullableString(value.completion_note)
    && typeof value.created_at === "string"
    && typeof value.updated_at === "string";
}

function isActivityJob(value: unknown): value is ActivityJob {
  return record(value)
    && typeof value.id === "string"
    && typeof value.title === "string"
    && isJobStatus(value.status)
    && nullableString(value.session_id)
    && typeof value.created_at === "string"
    && nullableString(value.finished_at)
    && nullableString(value.response)
    && nullableString(value.error)
    && strings(value.waiting_on);
}

export function readActivity(value: unknown): Activity {
  if (!isActivity(value)) throw new Error("Invalid Activity response from the server.");
  return value;
}

export function readActivityList(value: unknown): Activity[] {
  if (!record(value) || value.schema !== 1 || !Array.isArray(value.activities)
      || !value.activities.every(isActivity)) {
    throw new Error("Invalid Activity list from the server.");
  }
  return value.activities;
}

export function readActivityDetail(value: unknown, id: string): ActivityDetail {
  if (!record(value) || value.schema !== 1 || !isActivity(value.activity)
      || value.activity.id !== id || !Array.isArray(value.jobs)
      || !value.jobs.every(isActivityJob) || !strings(value.sessions)) {
    throw new Error("Invalid Activity detail from the server.");
  }
  return { schema: 1, activity: value.activity, jobs: value.jobs, sessions: value.sessions };
}

function isObjectDescription(value: unknown): value is ObjectDescription {
  return record(value) && record(value.object)
    && typeof value.object.app_id === "string"
    && typeof value.object.object_type === "string"
    && typeof value.object.object_id === "string"
    && (value.object.revision === undefined || typeof value.object.revision === "string")
    && typeof value.reference === "string"
    && typeof value.app_name === "string"
    && typeof value.app_version === "string"
    && typeof value.object_label === "string"
    && typeof value.object_summary === "string"
    && record(value.invocation)
    && value.invocation.app_id === value.object.app_id
    && typeof value.invocation.operation === "string"
    && strings(value.invocation.args);
}

function isActivityObject(value: unknown): value is ActivityObject {
  if (!record(value) || typeof value.label !== "string" || typeof value.reference !== "string") {
    return false;
  }
  if (value.status === "declared") {
    return value.error === null && isObjectDescription(value.description)
      && value.description.reference === value.reference;
  }
  return (value.status === "unavailable" || value.status === "invalid")
    && value.description === null && typeof value.error === "string";
}

export function readActivityObjects(value: unknown, id: string): ActivityObjects {
  if (!record(value) || value.schema !== 1 || value.activity_id !== id
      || !Array.isArray(value.objects) || !value.objects.every(isActivityObject)) {
    throw new Error("Invalid App object descriptions from the server.");
  }
  return { schema: 1, activity_id: id, objects: value.objects };
}

function readSubmittedJob(value: unknown, id: string): SubmittedJob {
  if (!record(value) || typeof value.id !== "string" || !isJobStatus(value.status)
      || value.activity_id !== id
      || (value.session_id !== undefined && !nullableString(value.session_id))) {
    throw new Error("Invalid submitted Activity job from the server. Refresh before retrying.");
  }
  return {
    id: value.id,
    status: value.status,
    activity_id: id,
    session_id: value.session_id,
  };
}

const activityPath = (id: string) => `/api/activities/${encodeURIComponent(id)}`;

export const activityApi = {
  list: async (state?: ActivityState, signal?: AbortSignal) => readActivityList(
    await api.get<unknown>(`/api/activities?limit=100${state ? `&state=${state}` : ""}`, { signal }),
  ),
  get: async (id: string, signal?: AbortSignal) => readActivityDetail(
    await api.get<unknown>(`${activityPath(id)}?limit=100`, { signal }), id,
  ),
  create: async (draft: ActivityDraft) => readActivity(
    await api.post<unknown>("/api/activities", draft),
  ),
  update: async (id: string, draft: ActivityDraft) => readActivity(
    await api.post<unknown>(`${activityPath(id)}/update`, draft),
  ),
  transition: async (id: string, state: ActivityState, completionNote?: string) => readActivity(
    await api.post<unknown>(`${activityPath(id)}/transition`, {
      state,
      ...(completionNote === undefined ? {} : { completion_note: completionNote }),
    }),
  ),
  run: async (id: string, input: ActivityRun) => readSubmittedJob(
    await api.post<unknown>(`${activityPath(id)}/run`, input), id,
  ),
  objects: async (id: string, signal?: AbortSignal) => readActivityObjects(
    await api.get<unknown>(`${activityPath(id)}/objects`, { signal }), id,
  ),
  attachObject: async (id: string, attachment: ActivityObjectAttachment) => {
    const activity = readActivity(await api.post<unknown>(`${activityPath(id)}/objects`, attachment));
    if (activity.id !== id) throw new Error("Invalid attached Activity from the server. Refresh before retrying.");
    return activity;
  },
};
