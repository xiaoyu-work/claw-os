import { nullableString, record, strings } from "@/lib/api-shapes";

export type OperationInvocation = { app_id: string; operation: string; args: string[] };

export const EFFECT_KINDS = ["read", "create", "update", "delete", "external", "execute"] as const;
export const EFFECT_RECOVERIES = ["not_applicable", "reversible", "compensatable", "irreversible", "unknown"] as const;
const TARGET_KINDS = ["path", "host", "name"] as const;
const TARGET_STATES = ["requested", "unspecified", "unresolved"] as const;

export type PlannedEffect = {
  kind: (typeof EFFECT_KINDS)[number];
  label: string;
  recovery: (typeof EFFECT_RECOVERIES)[number];
  target_arg: string | null;
  target_kind: (typeof TARGET_KINDS)[number] | null;
  requested_targets: string[];
  target_state: (typeof TARGET_STATES)[number];
};
export type OperationPreview = {
  schema: 1;
  app_id: string;
  app_name: string;
  app_version: string;
  package_digest: string;
  operation: string;
  operation_label: string;
  effects_declared: boolean;
  effects: PlannedEffect[];
  unresolved_arguments: string[];
  authorization_checked: false;
  executed: false;
  effects_confirmed: false;
  notes: string[];
};

function oneOf<T extends readonly string[]>(choices: T, value: unknown): value is T[number] {
  return choices.some((choice) => choice === value);
}

function isPlannedEffect(value: unknown): value is PlannedEffect {
  return record(value)
    && oneOf(EFFECT_KINDS, value.kind)
    && typeof value.label === "string"
    && oneOf(EFFECT_RECOVERIES, value.recovery)
    && nullableString(value.target_arg)
    && (value.target_kind === null || oneOf(TARGET_KINDS, value.target_kind))
    && strings(value.requested_targets)
    && oneOf(TARGET_STATES, value.target_state);
}

function isOperationPreview(value: unknown): value is OperationPreview {
  return record(value) && value.schema === 1
    && typeof value.app_id === "string"
    && typeof value.app_name === "string"
    && typeof value.app_version === "string"
    && typeof value.package_digest === "string"
    && typeof value.operation === "string"
    && typeof value.operation_label === "string"
    && typeof value.effects_declared === "boolean"
    && Array.isArray(value.effects) && value.effects.every(isPlannedEffect)
    && (value.effects_declared || value.effects.length === 0)
    && strings(value.unresolved_arguments)
    && value.authorization_checked === false
    && value.executed === false
    && value.effects_confirmed === false
    && strings(value.notes);
}

export function readOperationPreview(value: unknown, invocation: OperationInvocation): OperationPreview {
  if (!isOperationPreview(value) || value.app_id !== invocation.app_id
      || value.operation !== invocation.operation) {
    throw new Error("Invalid operation preview from the server.");
  }
  return value;
}
