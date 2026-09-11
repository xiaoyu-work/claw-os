import { nullableString, oneOf, record } from "@/lib/api-shapes";
import { EFFECT_KINDS, EFFECT_RECOVERIES, type PlannedEffect } from "@/lib/operation-preview";

export type ResultSummary = {
  kind: "json" | "text" | "empty";
  sha256: string;
  bytes: number;
  preview: string;
  preview_truncated: boolean;
};
export type ReceiptReport = {
  id: string;
  app_id: string;
  operation: string;
  package_digest: string;
  outcome: "returned" | "reported_error" | "indeterminate";
  result: ResultSummary | null;
  error: string | null;
};
export type ReceiptEffect = Pick<PlannedEffect, "kind" | "label" | "recovery" | "target_arg">;
export type ReceiptDeclaration = {
  app_version: string;
  operation_label: string;
  effects: ReceiptEffect[];
};
export type ActivityReceipt = {
  id: string;
  activity_id: string;
  owner_uid: number;
  received_at: string;
  source: "caller_reported";
  report: ReceiptReport;
} & (
  | { declaration: ReceiptDeclaration; declaration_error: null }
  | { declaration: null; declaration_error: string }
);
export type ActivityReceipts = {
  schema: 1;
  activity_id: string;
  receipts: ActivityReceipt[];
};

function identifier(value: unknown): value is string {
  return typeof value === "string" && value.length > 0;
}

function unsigned(value: unknown, max = Number.MAX_SAFE_INTEGER): value is number {
  return typeof value === "number" && Number.isSafeInteger(value) && value >= 0 && value <= max;
}

function recordedTime(value: unknown): value is string {
  return typeof value === "string"
    && /^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(?:\.\d+)?(?:Z|[+-]\d{2}:\d{2})$/i.test(value)
    && Number.isFinite(Date.parse(value));
}

function isResultSummary(value: unknown): value is ResultSummary {
  return record(value) && oneOf(["json", "text", "empty"] as const, value.kind)
    && identifier(value.sha256) && unsigned(value.bytes)
    && typeof value.preview === "string" && typeof value.preview_truncated === "boolean";
}

function isReport(value: unknown): value is ReceiptReport {
  return record(value) && identifier(value.id) && identifier(value.app_id)
    && identifier(value.operation) && identifier(value.package_digest)
    && oneOf(["returned", "reported_error", "indeterminate"] as const, value.outcome)
    && (value.result === null || isResultSummary(value.result))
    && nullableString(value.error);
}

function isDeclaration(value: unknown): value is ReceiptDeclaration {
  return record(value) && typeof value.app_version === "string"
    && typeof value.operation_label === "string" && Array.isArray(value.effects)
    && value.effects.every((effect) => record(effect)
      && oneOf(EFFECT_KINDS, effect.kind) && typeof effect.label === "string"
      && oneOf(EFFECT_RECOVERIES, effect.recovery) && nullableString(effect.target_arg));
}

function isReceipt(value: unknown): value is ActivityReceipt {
  return record(value) && identifier(value.id) && identifier(value.activity_id)
    && unsigned(value.owner_uid, 0xffff_ffff) && recordedTime(value.received_at)
    && value.source === "caller_reported" && isReport(value.report)
    && ((isDeclaration(value.declaration) && value.declaration_error === null)
      || (value.declaration === null && typeof value.declaration_error === "string"));
}

export function readActivityReceipts(value: unknown, id: string): ActivityReceipts {
  if (!record(value) || value.schema !== 1 || value.activity_id !== id
      || !Array.isArray(value.receipts) || !value.receipts.every(isReceipt)
      || value.receipts.some((receipt) => receipt.activity_id !== id)
      || new Set(value.receipts.map((receipt) => receipt.id)).size !== value.receipts.length) {
    throw new Error("Invalid Activity receipts from the server.");
  }
  return { schema: 1, activity_id: id, receipts: value.receipts };
}
