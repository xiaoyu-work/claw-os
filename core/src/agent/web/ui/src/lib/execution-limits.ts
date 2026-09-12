import { api } from "@/lib/api";
import { record } from "@/lib/api-shapes";
import { recordedTime } from "@/lib/activity-receipts";

export type ExecutionLimitsDraft = {
  max_attempts: number;
  max_turns_per_attempt: number;
  expires_at: string;
};
export type ActivityExecutionLimits = {
  activity_id: string;
  owner_uid: number;
  revision: number;
  enabled: boolean;
  limits: ExecutionLimitsDraft;
  used_attempts: number;
  created_at: string;
  updated_at: string;
};
export type ExecutionLimitsView = {
  schema: 1;
  activity_id: string;
  execution_limits: ActivityExecutionLimits | null;
};

const integer = (value: unknown, minimum: number, maximum: number): value is number =>
  typeof value === "number" && Number.isSafeInteger(value) && value >= minimum && value <= maximum;

export function readExecutionLimits(value: unknown, id: string): ActivityExecutionLimits {
  if (!record(value) || value.activity_id !== id || !integer(value.owner_uid, 0, 0xffff_ffff)
    || !integer(value.revision, 1, Number.MAX_SAFE_INTEGER) || typeof value.enabled !== "boolean"
    || !integer(value.used_attempts, 0, 1000) || !recordedTime(value.created_at)
    || !recordedTime(value.updated_at) || !record(value.limits)
    || !integer(value.limits.max_attempts, 1, 1000) || !integer(value.limits.max_turns_per_attempt, 1, 100)
    || !recordedTime(value.limits.expires_at)) {
    throw new Error("Invalid Activity execution limits from the server.");
  }
  return {
    activity_id: id, owner_uid: value.owner_uid, revision: value.revision, enabled: value.enabled,
    limits: {
      max_attempts: value.limits.max_attempts, max_turns_per_attempt: value.limits.max_turns_per_attempt,
      expires_at: value.limits.expires_at,
    },
    used_attempts: value.used_attempts, created_at: value.created_at, updated_at: value.updated_at,
  };
}

export function readExecutionLimitsView(value: unknown, id: string): ExecutionLimitsView {
  if (!record(value) || value.schema !== 1 || value.activity_id !== id) {
    throw new Error("Invalid Activity execution-limit view from the server.");
  }
  return {
    schema: 1, activity_id: id,
    execution_limits: value.execution_limits === null ? null : readExecutionLimits(value.execution_limits, id),
  };
}

const endpoint = (id: string) => `/api/activities/${encodeURIComponent(id)}/execution-limits`;
export const executionLimitsApi = {
  get: async (id: string, signal?: AbortSignal) => readExecutionLimitsView(
    await api.get<unknown>(endpoint(id), { signal }), id,
  ),
  set: async (id: string, revision: number | null, limits: ExecutionLimitsDraft) => {
    const saved = readExecutionLimits(
      await api.post<unknown>(endpoint(id), { expected_revision: revision, limits }), id,
    );
    if (saved.revision !== (revision ?? 0) + 1
      || saved.limits.max_attempts !== limits.max_attempts
      || saved.limits.max_turns_per_attempt !== limits.max_turns_per_attempt
      || Date.parse(saved.limits.expires_at) !== Date.parse(limits.expires_at)) {
      throw new Error("Execution-limit revision did not match the requested update. Refresh before retrying.");
    }
    return saved;
  },
  enable: async (id: string, revision: number, enabled: boolean) => {
    const saved = readExecutionLimits(
      await api.post<unknown>(`${endpoint(id)}/enabled`, { expected_revision: revision, enabled }), id,
    );
    if (saved.revision !== revision + 1 || saved.enabled !== enabled) {
      throw new Error("Execution-limit acknowledgement did not match the requested state. Refresh before retrying.");
    }
    return saved;
  },
};
