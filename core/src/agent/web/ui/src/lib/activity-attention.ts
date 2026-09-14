import { api } from "@/lib/api";
import { nullableString, record } from "@/lib/api-shapes";
import { isActivityState, type ActivityState } from "@/lib/activities";

export type ActivityAttentionCounts = {
  queued: number;
  running: number;
  waiting: number;
  completed: number;
  failed: number;
  cancelled: number;
  indeterminate: number;
  pending_decisions: number;
  unavailable_decisions: number;
  unread_notifications: number;
};

export type ActivityDecisionStatus = "pending" | "approved" | "denied" | "unavailable";
export type ActivityAttentionDecision = {
  id: string;
  job_id: string;
  session_id: string | null;
  status: ActivityDecisionStatus;
  requested_at: number | null;
  verb: string | null;
  scope: Record<string, unknown> | null;
  risk: "low" | "medium" | "high" | "critical" | null;
  reason: string | null;
  review_id: string | null;
  error: string | null;
};

export type ActivityAttentionIssue = {
  job_id: string;
  session_id: string | null;
  kind: "waiting_approval" | "indeterminate" | "failed";
  status: string;
  execution_phase: string;
  title: string;
  created_at: string;
  finished_at: string | null;
  message: string;
};

export type ActivityAttentionNotification = {
  id: string;
  source: string;
  kind: string;
  severity: "info" | "warning" | "error" | "critical";
  title: string;
  body: string;
  task_id: string;
  session_id?: string | null;
  state: "unread" | "read" | "acknowledged" | "dismissed";
  occurrences: number;
  updated_at_ms: number;
};

export type ActivityAttention = {
  schema: 1;
  activity_id: string;
  activity_state: ActivityState;
  limit: number;
  counts: ActivityAttentionCounts;
  decisions: ActivityAttentionDecision[];
  issues: ActivityAttentionIssue[];
  notifications: ActivityAttentionNotification[];
  totals: { decisions: number; issues: number; notifications: number };
  has_more: { decisions: boolean; issues: boolean; notifications: boolean };
};

const countKeys = [
  "queued", "running", "waiting", "completed", "failed", "cancelled",
  "indeterminate", "pending_decisions", "unavailable_decisions", "unread_notifications",
] as const;

function natural(value: unknown): value is number {
  return typeof value === "number" && Number.isSafeInteger(value) && value >= 0;
}

function isCounts(value: unknown): value is ActivityAttentionCounts {
  return record(value) && countKeys.every((key) => natural(value[key]));
}

function isDecision(value: unknown): value is ActivityAttentionDecision {
  if (!record(value)
      || typeof value.id !== "string"
      || typeof value.job_id !== "string"
      || !nullableString(value.session_id)
      || !["pending", "approved", "denied", "unavailable"].includes(String(value.status))
      || !(value.requested_at === null || natural(value.requested_at))
      || !nullableString(value.verb)
      || !(value.scope === null || record(value.scope))
      || !(value.risk === null || ["low", "medium", "high", "critical"].includes(String(value.risk)))
      || !nullableString(value.reason)
      || !nullableString(value.review_id)
      || !nullableString(value.error)) {
    return false;
  }
  return value.status === "unavailable"
    ? value.requested_at === null && value.verb === null && value.scope === null
      && value.risk === null && value.reason === null && value.review_id === null
      && typeof value.error === "string"
    : natural(value.requested_at) && typeof value.verb === "string"
      && record(value.scope) && typeof value.risk === "string"
      && typeof value.reason === "string" && typeof value.review_id === "string"
      && value.error === null;
}

function isIssue(value: unknown): value is ActivityAttentionIssue {
  return record(value)
    && typeof value.job_id === "string"
    && nullableString(value.session_id)
    && ["waiting_approval", "indeterminate", "failed"].includes(String(value.kind))
    && typeof value.status === "string"
    && typeof value.execution_phase === "string"
    && typeof value.title === "string"
    && typeof value.created_at === "string"
    && nullableString(value.finished_at)
    && typeof value.message === "string";
}

function isNotification(value: unknown): value is ActivityAttentionNotification {
  return record(value)
    && typeof value.id === "string"
    && typeof value.source === "string"
    && typeof value.kind === "string"
    && ["info", "warning", "error", "critical"].includes(String(value.severity))
    && typeof value.title === "string"
    && typeof value.body === "string"
    && typeof value.task_id === "string"
    && (value.session_id === undefined || nullableString(value.session_id))
    && ["unread", "read", "acknowledged", "dismissed"].includes(String(value.state))
    && natural(value.occurrences)
    && value.occurrences > 0
    && natural(value.updated_at_ms);
}

function isTotals(value: unknown): value is ActivityAttention["totals"] {
  return record(value)
    && natural(value.decisions)
    && natural(value.issues)
    && natural(value.notifications);
}

function isMore(value: unknown): value is ActivityAttention["has_more"] {
  return record(value)
    && typeof value.decisions === "boolean"
    && typeof value.issues === "boolean"
    && typeof value.notifications === "boolean";
}

export function readActivityAttention(value: unknown, id: string): ActivityAttention {
  if (!record(value)
      || value.schema !== 1
      || value.activity_id !== id
      || !isActivityState(value.activity_state)
      || !natural(value.limit)
      || value.limit < 1
      || value.limit > 100
      || !isCounts(value.counts)
      || !Array.isArray(value.decisions)
      || !value.decisions.every(isDecision)
      || !Array.isArray(value.issues)
      || !value.issues.every(isIssue)
      || !Array.isArray(value.notifications)
      || !value.notifications.every(isNotification)
      || !isTotals(value.totals)
      || !isMore(value.has_more)
      || value.decisions.length > value.limit
      || value.issues.length > value.limit
      || value.notifications.length > value.limit
      || value.totals.decisions < value.decisions.length
      || value.totals.issues < value.issues.length
      || value.totals.notifications < value.notifications.length
      || value.has_more.decisions !== (value.totals.decisions > value.decisions.length)
      || value.has_more.issues !== (value.totals.issues > value.issues.length)
      || value.has_more.notifications !== (value.totals.notifications > value.notifications.length)) {
    throw new Error("Invalid Activity attention response from the server.");
  }
  return value as ActivityAttention;
}

const path = (id: string) => `/api/activities/${encodeURIComponent(id)}/attention?limit=50`;

export const activityAttentionApi = {
  get: async (id: string, signal?: AbortSignal) =>
    readActivityAttention(await api.get<unknown>(path(id), { signal }), id),
};
