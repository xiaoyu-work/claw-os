import { isReceiptReport, recordedTime, type ReceiptReport } from "@/lib/activity-receipts";
import { oneOf, record } from "@/lib/api-shapes";

export const RELATION_KINDS = ["related_to", "depends_on", "derived_from"] as const;
export type ObjectRelationKind = (typeof RELATION_KINDS)[number];
export type ObjectStateContent =
  | { kind: "user_statement"; text: string }
  | { kind: "agent_inference"; text: string }
  | { kind: "app_report"; receipt_id: string }
  | { kind: "relation"; relation: ObjectRelationKind; target: string; note: string }
  | { kind: "retracted"; reason: string };
export type ObjectStateDraft = {
  id: string;
  reference: string;
  content: ObjectStateContent;
  observed_at: string | null;
  valid_until: string | null;
  supersedes: string | null;
};
export const OBJECT_VALIDITIES = ["unknown", "not_yet_applicable", "within_reported_window", "expired"] as const;
export type ObjectStateEntry = {
  id: string;
  activity_id: string;
  owner_uid: number;
  recorded_at: string;
  source: "caller_reported";
  draft: ObjectStateDraft;
  receipt: ReceiptReport | null;
  superseded_by: string | null;
  validity: (typeof OBJECT_VALIDITIES)[number];
};
export type ObjectStateView = { schema: 1; activity_id: string; entries: ObjectStateEntry[] };
export const objectKindLabels: Record<ObjectStateContent["kind"], string> = {
  user_statement: "User statement (reported)",
  agent_inference: "Agent inference (reported)",
  app_report: "Linked App report",
  relation: "Planning relationship",
  retracted: "Retracted",
};
export const validityLabels: Record<ObjectStateEntry["validity"], string> = {
  unknown: "Validity unknown",
  not_yet_applicable: "Reported window has not started",
  within_reported_window: "Within reported window (not verified)",
  expired: "Reported window expired",
};

const uuid = (value: unknown): value is string =>
  typeof value === "string" && /^[0-9a-f]{8}(?:-[0-9a-f]{4}){3}-[0-9a-f]{12}$/.test(value);
const optionalId = (value: unknown): value is string | null => value === null || uuid(value);
const text = (value: unknown, max: number, empty = false): value is string =>
  typeof value === "string" && (empty || value.trim().length > 0)
  && new TextEncoder().encode(value).length <= max
  && !/[\u0000-\u0008\u000b\u000c\u000e-\u001f\u007f-\u009f]/.test(value);
const keys = (value: Record<string, unknown>, allowed: string[]) =>
  Object.keys(value).every((key) => allowed.includes(key));

function isContent(value: unknown): value is ObjectStateContent {
  if (!record(value)) return false;
  switch (value.kind) {
    case "user_statement":
    case "agent_inference":
      return keys(value, ["kind", "text"]) && text(value.text, 4096);
    case "app_report":
      return keys(value, ["kind", "receipt_id"]) && uuid(value.receipt_id);
    case "relation":
      return keys(value, ["kind", "relation", "target", "note"])
        && oneOf(RELATION_KINDS, value.relation) && text(value.target, 4096)
        && text(value.note, 2048, true);
    case "retracted":
      return keys(value, ["kind", "reason"]) && text(value.reason, 4096);
    default:
      return false;
  }
}

function isDraft(value: unknown): value is ObjectStateDraft {
  if (!record(value) || !keys(value, [
    "id", "reference", "content", "observed_at", "valid_until", "supersedes",
  ]) || !uuid(value.id) || !text(value.reference, 4096) || !isContent(value.content)
    || !optionalId(value.supersedes) || value.supersedes === value.id) return false;
  const window = (value.observed_at === null && value.valid_until === null)
    || (recordedTime(value.observed_at) && recordedTime(value.valid_until)
      && Date.parse(value.valid_until) > Date.parse(value.observed_at));
  if (!window) return false;
  if (value.content.kind === "relation" || value.content.kind === "retracted") {
    if (value.observed_at !== null || value.valid_until !== null) return false;
  }
  return (value.content.kind !== "retracted" || value.supersedes !== null)
    && (value.content.kind !== "relation" || value.content.target !== value.reference);
}

function isEntry(value: unknown): value is ObjectStateEntry {
  if (!record(value) || !keys(value, [
    "id", "activity_id", "owner_uid", "recorded_at", "source", "draft", "receipt", "superseded_by", "validity",
  ]) || !uuid(value.id) || !text(value.activity_id, 128)
    || !Number.isInteger(value.owner_uid) || typeof value.owner_uid !== "number"
    || value.owner_uid < 0 || value.owner_uid > 0xffff_ffff
    || !recordedTime(value.recorded_at) || value.source !== "caller_reported"
    || !isDraft(value.draft) || value.id !== value.draft.id
    || !optionalId(value.superseded_by) || value.superseded_by === value.id
    || !oneOf(OBJECT_VALIDITIES, value.validity)) return false;
  if ((value.draft.observed_at === null) !== (value.validity === "unknown")) return false;
  return value.draft.content.kind === "app_report"
    ? isReceiptReport(value.receipt) && value.receipt.id === value.draft.content.receipt_id
    : value.receipt === null;
}

export function readObjectStateEntry(value: unknown, activityId: string): ObjectStateEntry {
  if (!isEntry(value) || value.activity_id !== activityId) {
    throw new Error("Invalid object-state entry from the server.");
  }
  return value;
}

export function readObjectState(value: unknown, activityId: string): ObjectStateView {
  if (!record(value) || value.schema !== 1 || value.activity_id !== activityId
    || !Array.isArray(value.entries) || value.entries.length > 100
    || !value.entries.every(isEntry) || value.entries.some((entry) => entry.activity_id !== activityId)
    || new Set(value.entries.map((entry) => entry.id)).size !== value.entries.length) {
    throw new Error("Invalid object-state history from the server.");
  }
  return { schema: 1, activity_id: activityId, entries: value.entries };
}
