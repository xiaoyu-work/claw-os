import { api } from "@/lib/api";
import { record } from "@/lib/api-shapes";
import { recordedTime } from "@/lib/activity-receipts";

export const MAX_MONETARY_MICROUSD = 1_000_000_000_000n;
export const MAX_OUTPUT_TOKENS_PER_TURN = 1_000_000;
const MAX_U64 = 18_446_744_073_709_551_615n;

export type MonetaryBudgetDraft = {
  currency: "USD";
  max_total_microusd: string;
  input_microusd_per_million_tokens: string;
  output_microusd_per_million_tokens: string;
  max_output_tokens_per_turn: number;
};

export type ActivityMonetaryBudget = {
  activity_id: string;
  owner_uid: number;
  revision: string;
  enabled: boolean;
  spent_microusd: string;
  reserved_microusd: string;
  budget: MonetaryBudgetDraft;
  created_at: string;
  updated_at: string;
};

export type MonetaryBudgetView = {
  schema: 1;
  activity_id: string;
  monetary_budget: ActivityMonetaryBudget | null;
};

const exact = (value: Record<string, unknown>, keys: string[]) =>
  Object.keys(value).length === keys.length
  && keys.every((key) => Object.prototype.hasOwnProperty.call(value, key));

export function decimalInteger(value: unknown, minimum: bigint, maximum: bigint): value is string {
  if (typeof value !== "string" || !/^[0-9]+$/.test(value)) return false;
  try {
    const parsed = BigInt(value);
    return parsed >= minimum && parsed <= maximum;
  } catch {
    return false;
  }
}

const outputLimit = (value: unknown): value is number =>
  typeof value === "number" && Number.isSafeInteger(value)
  && value >= 1 && value <= MAX_OUTPUT_TOKENS_PER_TURN;

export function readMonetaryBudget(value: unknown, id: string): ActivityMonetaryBudget {
  const fields = [
    "activity_id", "owner_uid", "revision", "enabled", "spent_microusd",
    "reserved_microusd", "budget", "created_at", "updated_at",
  ];
  const budgetFields = [
    "currency", "max_total_microusd", "input_microusd_per_million_tokens",
    "output_microusd_per_million_tokens", "max_output_tokens_per_turn",
  ];
  if (!record(value) || !exact(value, fields) || value.activity_id !== id
    || typeof value.owner_uid !== "number" || !Number.isSafeInteger(value.owner_uid)
    || value.owner_uid < 0 || value.owner_uid > 0xffff_ffff
    || !decimalInteger(value.revision, 1n, MAX_U64)
    || typeof value.enabled !== "boolean"
    || !decimalInteger(value.spent_microusd, 0n, MAX_U64)
    || !decimalInteger(value.reserved_microusd, 0n, MAX_U64)
    || !record(value.budget) || !exact(value.budget, budgetFields)
    || value.budget.currency !== "USD"
    || !decimalInteger(value.budget.max_total_microusd, 1n, MAX_MONETARY_MICROUSD)
    || !decimalInteger(value.budget.input_microusd_per_million_tokens, 1n, MAX_MONETARY_MICROUSD)
    || !decimalInteger(value.budget.output_microusd_per_million_tokens, 1n, MAX_MONETARY_MICROUSD)
    || !outputLimit(value.budget.max_output_tokens_per_turn)
    || !recordedTime(value.created_at) || !recordedTime(value.updated_at)) {
    throw new Error("Invalid Activity monetary budget from the server.");
  }
  return value as ActivityMonetaryBudget;
}

export function readMonetaryBudgetView(value: unknown, id: string): MonetaryBudgetView {
  if (!record(value) || !exact(value, ["schema", "activity_id", "monetary_budget"])
    || value.schema !== 1 || value.activity_id !== id
    || (value.monetary_budget !== null && !record(value.monetary_budget))) {
    throw new Error("Invalid Activity monetary-budget view from the server.");
  }
  return {
    schema: 1,
    activity_id: id,
    monetary_budget: value.monetary_budget === null
      ? null : readMonetaryBudget(value.monetary_budget, id),
  };
}

export function validateMonetaryDraft(draft: MonetaryBudgetDraft): boolean {
  return draft.currency === "USD"
    && decimalInteger(draft.max_total_microusd, 1n, MAX_MONETARY_MICROUSD)
    && decimalInteger(draft.input_microusd_per_million_tokens, 1n, MAX_MONETARY_MICROUSD)
    && decimalInteger(draft.output_microusd_per_million_tokens, 1n, MAX_MONETARY_MICROUSD)
    && outputLimit(draft.max_output_tokens_per_turn);
}

export function remainingMicrousd(policy: ActivityMonetaryBudget): bigint {
  const total = BigInt(policy.budget.max_total_microusd);
  const committed = BigInt(policy.spent_microusd) + BigInt(policy.reserved_microusd);
  return committed >= total ? 0n : total - committed;
}

export function formatMicrousd(value: string | bigint): string {
  const amount = typeof value === "bigint" ? value : BigInt(value);
  const whole = amount / 1_000_000n;
  const fraction = (amount % 1_000_000n).toString().padStart(6, "0");
  return `USD ${whole.toString()}.${fraction}`;
}

const endpoint = (id: string) => `/api/activities/${encodeURIComponent(id)}/monetary-budget`;

function nextRevision(revision: string | null): string {
  const next = BigInt(revision ?? "0") + 1n;
  if (next > MAX_U64) throw new Error("Monetary-budget revision cannot be incremented.");
  return next.toString();
}

export const monetaryBudgetApi = {
  get: async (id: string, signal?: AbortSignal) => readMonetaryBudgetView(
    await api.get<unknown>(endpoint(id), { signal }), id,
  ),
  set: async (
    id: string,
    ownerUid: number,
    revision: string | null,
    budget: MonetaryBudgetDraft,
    previous?: ActivityMonetaryBudget,
  ) => {
    if (!validateMonetaryDraft(budget)) throw new Error("Enter valid positive monetary-budget integers.");
    const saved = readMonetaryBudget(
      await api.post<unknown>(endpoint(id), { expected_revision: revision, budget }), id,
    );
    if (saved.owner_uid !== ownerUid || saved.revision !== nextRevision(revision)
      || JSON.stringify(saved.budget) !== JSON.stringify(budget)
      || (revision === null && !saved.enabled)
      || (previous && (saved.enabled !== previous.enabled
        || saved.created_at !== previous.created_at))) {
      throw new Error("Monetary-budget acknowledgement did not preserve the owner, accounting, enabled state, or exact revision. Refresh before retrying.");
    }
    return saved;
  },
  enable: async (id: string, ownerUid: number, current: ActivityMonetaryBudget, enabled: boolean) => {
    const saved = readMonetaryBudget(
      await api.post<unknown>(`${endpoint(id)}/enabled`, {
        expected_revision: current.revision, enabled,
      }), id,
    );
    if (saved.owner_uid !== ownerUid || saved.revision !== nextRevision(current.revision)
      || saved.enabled !== enabled || JSON.stringify(saved.budget) !== JSON.stringify(current.budget)
      || saved.created_at !== current.created_at) {
      throw new Error("Monetary-budget toggle did not preserve configured accounting or the exact revision. Refresh before retrying.");
    }
    return saved;
  },
};
