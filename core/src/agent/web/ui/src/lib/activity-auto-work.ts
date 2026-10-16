import { api } from "@/lib/api";
import { oneOf, record } from "@/lib/api-shapes";
import { recordedTime } from "@/lib/activity-receipts";
import { decimalInteger } from "@/lib/monetary-budget";

export type WorkDraft = {
  task: string;
  check_interval_secs: number;
  wake_event: { source: string; event_type: string } | null;
};
export type WorkNext =
  | { kind: "continue"; task: string }
  | { kind: "wait"; task: string; reason: string }
  | { kind: "review" | "blocked"; reason: string };
export type WorkCheckpoint = {
  summary: string; made_progress: boolean; completed_steps: string[]; artifacts: string[]; next: WorkNext;
};
export type WorkRecord = {
  job_id: string; session_id: string; recorded_at: string; checkpoint: WorkCheckpoint | null;
  outcome: "reported" | "failed" | "indeterminate" | "cancelled"; message: string | null;
};
export type AutoWork = {
  activity_id: string; owner_uid: number; revision: string; config: WorkDraft;
  enabled: boolean; generation: string | null; phase: "ready" | "running" | "waiting" | "review" | "held";
  next_task: string; next_wake_at: string | null; event_after: string;
  current: { job_id: string; session_id: string; generation: string; prepared_at: string; checkpoint: WorkCheckpoint | null } | null;
  last_checkpoint: WorkRecord | null; stagnant_segments: number; message: string | null;
  created_at: string; updated_at: string;
};
export type AutoWorkView = { schema: 1; activity_id: string; work: AutoWork | null; history: WorkRecord[] };

function invalid(): never { throw new Error("Invalid Activity automatic-work response; refresh before making changes."); }
function object(value: unknown, keys: string[]): Record<string, unknown> {
  if (!record(value) || Object.keys(value).length !== keys.length
    || !keys.every((key) => Object.prototype.hasOwnProperty.call(value, key))) invalid();
  return value;
}
function text(value: unknown, maximum: number): string {
  if (typeof value !== "string" || !value.trim() || new TextEncoder().encode(value).length > maximum
    || /[\u0000-\u0008\u000b\u000c\u000e-\u001f\u007f]/.test(value)) invalid();
  return value;
}
function nullableText(value: unknown, maximum: number): string | null {
  return value === null ? null : text(value, maximum);
}
function timestamp(value: unknown): string {
  if (!recordedTime(value)) invalid();
  return value;
}
function uuid(value: unknown): string {
  const result = text(value, 36);
  if (!/^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/.test(result)) invalid();
  return result;
}
function integer(value: unknown, minimum: number, maximum: number): number {
  if (typeof value !== "number" || !Number.isSafeInteger(value) || value < minimum || value > maximum) invalid();
  return value;
}
function boolean(value: unknown): boolean {
  if (typeof value !== "boolean") invalid();
  return value;
}
function decimal(value: unknown, minimum = 0n): string {
  if (!decimalInteger(value, minimum, 9_223_372_036_854_775_807n)) invalid();
  return value;
}
function list(value: unknown, maximum: number, bytes: number): string[] {
  if (!Array.isArray(value) || value.length > maximum) invalid();
  return value.map((item: unknown) => text(item, bytes));
}
export function readWorkDraft(value: unknown): WorkDraft {
  const draft = object(value, ["task", "check_interval_secs", "wake_event"]);
  let wake: WorkDraft["wake_event"] = null;
  if (draft.wake_event !== null) {
    const event = object(draft.wake_event, ["source", "event_type"]);
    wake = { source: text(event.source, 128), event_type: text(event.event_type, 128) };
  }
  return { task: text(draft.task, 8192), check_interval_secs: integer(draft.check_interval_secs, 60, 86400), wake_event: wake };
}
function readCheckpoint(value: unknown): WorkCheckpoint {
  const checkpoint = object(value, ["summary", "made_progress", "completed_steps", "artifacts", "next"]);
  if (!record(checkpoint.next)) invalid();
  const kind = checkpoint.next.kind;
  let next: WorkNext;
  if (kind === "continue") {
    const item = object(checkpoint.next, ["kind", "task"]);
    next = { kind, task: text(item.task, 8192) };
  } else if (kind === "wait") {
    const item = object(checkpoint.next, ["kind", "task", "reason"]);
    next = { kind, task: text(item.task, 8192), reason: text(item.reason, 2048) };
  } else if (kind === "review" || kind === "blocked") {
    const item = object(checkpoint.next, ["kind", "reason"]);
    next = { kind, reason: text(item.reason, 2048) };
  } else invalid();
  return {
    summary: text(checkpoint.summary, 8192), made_progress: boolean(checkpoint.made_progress),
    completed_steps: list(checkpoint.completed_steps, 32, 512), artifacts: list(checkpoint.artifacts, 32, 4096), next,
  };
}
function readRecord(value: unknown): WorkRecord {
  const item = object(value, ["job_id", "session_id", "recorded_at", "checkpoint", "outcome", "message"]);
  if (!oneOf(["reported", "failed", "indeterminate", "cancelled"] as const, item.outcome)) invalid();
  return {
    job_id: text(item.job_id, 128), session_id: text(item.session_id, 128),
    recorded_at: timestamp(item.recorded_at), checkpoint: item.checkpoint === null ? null : readCheckpoint(item.checkpoint),
    outcome: item.outcome, message: nullableText(item.message, 2048),
  };
}
export function readAutoWork(value: unknown, id: string): AutoWork {
  const item = object(value, [
    "activity_id", "owner_uid", "revision", "config", "enabled", "generation", "phase",
    "next_task", "next_wake_at", "event_after", "current", "last_checkpoint", "stagnant_segments",
    "message", "created_at", "updated_at",
  ]);
  if (item.activity_id !== id || !oneOf(["ready", "running", "waiting", "review", "held"] as const, item.phase)) invalid();
  const generation = item.generation === null ? null : uuid(item.generation);
  let current: AutoWork["current"] = null;
  if (item.current !== null) {
    const segment = object(item.current, ["job_id", "session_id", "generation", "prepared_at", "checkpoint"]);
    current = {
      job_id: text(segment.job_id, 128), session_id: text(segment.session_id, 128),
      generation: uuid(segment.generation), prepared_at: timestamp(segment.prepared_at),
      checkpoint: segment.checkpoint === null ? null : readCheckpoint(segment.checkpoint),
    };
    if (current.generation !== generation) invalid();
  }
  if ((item.phase === "running") !== (current !== null) || (item.enabled === true && generation === null)) invalid();
  return {
    activity_id: uuid(item.activity_id), owner_uid: integer(item.owner_uid, 0, 0xffff_ffff),
    revision: decimal(item.revision, 1n), config: readWorkDraft(item.config), enabled: boolean(item.enabled),
    generation, phase: item.phase, next_task: text(item.next_task, 8192),
    next_wake_at: item.next_wake_at === null ? null : timestamp(item.next_wake_at),
    event_after: decimal(item.event_after), current,
    last_checkpoint: item.last_checkpoint === null ? null : readRecord(item.last_checkpoint),
    stagnant_segments: integer(item.stagnant_segments, 0, 3), message: nullableText(item.message, 2048),
    created_at: timestamp(item.created_at), updated_at: timestamp(item.updated_at),
  };
}
export function readAutoWorkView(value: unknown, id: string): AutoWorkView {
  const item = object(value, ["schema", "activity_id", "work", "history"]);
  if (item.schema !== 1 || item.activity_id !== id || !Array.isArray(item.history) || item.history.length > 20) invalid();
  return {
    schema: 1, activity_id: id, work: item.work === null ? null : readAutoWork(item.work, id),
    history: item.history.map((record: unknown) => readRecord(record)),
  };
}
const endpoint = (id: string) => `/api/activities/${encodeURIComponent(id)}/work`;
function acknowledgement(value: unknown, id: string, owner: number, revision: string | null): AutoWork {
  const work = readAutoWork(value, id);
  if (work.owner_uid !== owner || BigInt(work.revision) !== BigInt(revision ?? "0") + 1n) invalid();
  return work;
}
export const autoWorkApi = {
  get: async (id: string, signal?: AbortSignal) =>
    readAutoWorkView(await api.get<unknown>(endpoint(id), { signal }), id),
  set: async (id: string, owner: number, revision: string | null, draft: WorkDraft) => {
    const submitted = readWorkDraft(draft);
    const saved = acknowledgement(await api.post<unknown>(endpoint(id), {
      expected_revision: revision, work: submitted,
    }), id, owner, revision);
    if (saved.enabled || JSON.stringify(saved.config) !== JSON.stringify(submitted)) invalid();
    return saved;
  },
  enabled: async (id: string, owner: number, revision: string, enabled: boolean, nextTask?: string, note?: string) => {
    const saved = acknowledgement(await api.post<unknown>(`${endpoint(id)}/enabled`, {
      expected_revision: revision, enabled, next_task: nextTask ?? null, review_note: note ?? null,
    }), id, owner, revision);
    if (saved.enabled !== enabled) invalid();
    return saved;
  },
};
