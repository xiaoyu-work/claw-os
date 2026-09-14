import { api } from "@/lib/api";
import { record } from "@/lib/api-shapes";
import { recordedTime } from "@/lib/activity-receipts";
import { decimalInteger } from "@/lib/monetary-budget";

const MAX_U64 = 18_446_744_073_709_551_615n;

export const schedulingPriorities = ["foreground", "standard", "background"] as const;
export type ActivitySchedulingPriority = typeof schedulingPriorities[number];

export type ActivitySchedulingPolicy = {
  activity_id: string;
  owner_uid: number;
  revision: string;
  priority: ActivitySchedulingPriority;
  created_at: string;
  updated_at: string;
};

export type SchedulingPriorityView = {
  schema: 1;
  activity_id: string;
  scheduling_policy: ActivitySchedulingPolicy | null;
};

const exact = (value: Record<string, unknown>, keys: string[]) =>
  Object.keys(value).length === keys.length
  && keys.every((key) => Object.prototype.hasOwnProperty.call(value, key));

export function isSchedulingPriority(value: unknown): value is ActivitySchedulingPriority {
  return typeof value === "string"
    && schedulingPriorities.includes(value as ActivitySchedulingPriority);
}

export function readSchedulingPolicy(value: unknown, id: string): ActivitySchedulingPolicy {
  if (!record(value)
    || !exact(value, [
      "activity_id", "owner_uid", "revision", "priority", "created_at", "updated_at",
    ])
    || value.activity_id !== id
    || typeof value.owner_uid !== "number"
    || !Number.isSafeInteger(value.owner_uid)
    || value.owner_uid < 0
    || value.owner_uid > 0xffff_ffff
    || !decimalInteger(value.revision, 1n, MAX_U64)
    || !isSchedulingPriority(value.priority)
    || !recordedTime(value.created_at)
    || !recordedTime(value.updated_at)) {
    throw new Error("Invalid Activity scheduling priority from the server.");
  }
  return value as ActivitySchedulingPolicy;
}

export function readSchedulingPriorityView(value: unknown, id: string): SchedulingPriorityView {
  if (!record(value)
    || !exact(value, ["schema", "activity_id", "scheduling_policy"])
    || value.schema !== 1
    || value.activity_id !== id
    || (value.scheduling_policy !== null && !record(value.scheduling_policy))) {
    throw new Error("Invalid Activity scheduling-priority view from the server.");
  }
  return {
    schema: 1,
    activity_id: id,
    scheduling_policy: value.scheduling_policy === null
      ? null
      : readSchedulingPolicy(value.scheduling_policy, id),
  };
}

const endpoint = (id: string) =>
  `/api/activities/${encodeURIComponent(id)}/scheduling-priority`;

function nextRevision(revision: string | null): string {
  const next = BigInt(revision ?? "0") + 1n;
  if (next > MAX_U64) throw new Error("Scheduling-priority revision cannot be incremented.");
  return next.toString();
}

export const schedulingPriorityApi = {
  get: async (id: string, signal?: AbortSignal) => readSchedulingPriorityView(
    await api.get<unknown>(endpoint(id), { signal }),
    id,
  ),
  set: async (
    id: string,
    ownerUid: number,
    revision: string | null,
    priority: ActivitySchedulingPriority,
    previous?: ActivitySchedulingPolicy,
  ) => {
    if (!isSchedulingPriority(priority)) {
      throw new Error("Select foreground, standard, or background scheduling priority.");
    }
    const saved = readSchedulingPolicy(
      await api.post<unknown>(endpoint(id), {
        expected_revision: revision,
        priority,
      }),
      id,
    );
    if (saved.owner_uid !== ownerUid
      || saved.revision !== nextRevision(revision)
      || saved.priority !== priority
      || (previous && saved.created_at !== previous.created_at)) {
      throw new Error(
        "Scheduling-priority acknowledgement did not preserve the owner, creation identity, submitted priority, or exact revision. Refresh before retrying.",
      );
    }
    return saved;
  },
};
