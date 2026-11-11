import { record, oneOf } from "./api-shapes";
import { api } from "./api";

export const CHOICES = ["deny", "restore", "ask", "allow_once", "allow_session", "allow_forever"] as const;
export type ApprovalChoice = typeof CHOICES[number];
const STATUSES = ["pending", "confirmed", "consumed", "completed", "cancelled", "stale", "expired", "failed"] as const;
export type ApprovalStatus = typeof STATUSES[number];
export type ApprovalView = {
  id: string; revision: string; status: ApprovalStatus; label: string; description: string;
  target: string; reason: string; risk: string; choices: ApprovalChoice[];
  verb: string; details: { label: string; value: string }[]; diagnostic: string | null;
};
export type SystemReviewAction = "cancel" | "confirm_global_app_stop";
export type SystemApprovalView = {
  id: string; revision: string; kind: "package_impact"; status: ApprovalStatus;
  status_message: string | null;
  subject: { app_id: null; name: string; version: null; publisher: null };
  initiator: string; context: { label: string; value: string }[];
  disclosures: { label: string; value: string }[];
  contract_digest: string; actions: SystemReviewAction[];
};
export type ApprovalBundle = {
  task_id?: string; reviews: ApprovalView[]; app_reviews: { id: string; label: string }[];
  system_reviews: SystemApprovalView[];
};

function details(value: unknown): { label: string; value: string }[] {
  if (!Array.isArray(value) || value.length > 4096) throw new Error("The OS review details are malformed.");
  return value.map((detail: unknown) => {
    if (!record(detail) || Object.keys(detail).some((key) => !["label", "value"].includes(key)) ||
        typeof detail.label !== "string" || typeof detail.value !== "string") {
      throw new Error("The OS review detail is malformed.");
    }
    return { label: detail.label, value: detail.value };
  });
}

export function readSystemApproval(value: unknown): SystemApprovalView {
  const keys = ["id", "revision", "kind", "status", "status_message", "subject", "initiator",
    "context", "disclosures", "contract_digest", "actions"];
  if (!record(value) || Object.keys(value).some((key) => !keys.includes(key)) ||
      typeof value.id !== "string" || !/^[A-Za-z0-9_-]{1,128}$/.test(value.id) ||
      typeof value.revision !== "string" || !/^[1-9][0-9]{0,19}$/.test(value.revision) ||
      BigInt(value.revision) > 18446744073709551615n ||
      value.kind !== "package_impact" || !oneOf(STATUSES, value.status) ||
      !(value.status_message === null || typeof value.status_message === "string") ||
      !record(value.subject) || Object.keys(value.subject).some((key) =>
        !["app_id", "name", "version", "publisher"].includes(key)) ||
      value.subject.app_id !== null || value.subject.version !== null || value.subject.publisher !== null ||
      typeof value.subject.name !== "string" || !value.subject.name.trim() ||
      typeof value.initiator !== "string" ||
      typeof value.contract_digest !== "string" || !/^[a-f0-9]{64}$/.test(value.contract_digest) ||
      !Array.isArray(value.actions) || !value.actions.every((action) =>
        oneOf(["cancel", "confirm_global_app_stop"] as const, action)) ||
      new Set(value.actions).size !== value.actions.length ||
      (value.status !== "pending" && value.actions.length !== 0)) {
    throw new Error("The separate OS package-impact review is malformed.");
  }
  return {
    id: value.id, revision: value.revision, kind: value.kind, status: value.status,
    status_message: value.status_message,
    subject: { app_id: null, name: value.subject.name, version: null, publisher: null },
    initiator: value.initiator, context: details(value.context), disclosures: details(value.disclosures),
    contract_digest: value.contract_digest, actions: value.actions as SystemReviewAction[],
  };
}

export function readApprovalBundle(value: unknown, taskId?: string): ApprovalBundle {
  if (!record(value) || !Array.isArray(value.reviews) || !Array.isArray(value.app_reviews) ||
      !Array.isArray(value.system_reviews) || value.system_reviews.length > 100 ||
      (taskId !== undefined && value.task_id !== taskId)) {
    throw new Error("The OS approval response has an invalid identity or shape.");
  }
  const reviews = value.reviews.map((item: unknown): ApprovalView => {
    if (!record(item) || typeof item.id !== "string" || !/^[A-Za-z0-9_-]{1,128}$/.test(item.id) ||
        typeof item.revision !== "string" || !/^[1-9][0-9]{0,19}$/.test(item.revision) ||
        BigInt(item.revision) > 18446744073709551615n ||
        !oneOf(STATUSES, item.status) ||
        typeof item.label !== "string" || typeof item.description !== "string" ||
        typeof item.target !== "string" || typeof item.reason !== "string" ||
        typeof item.verb !== "string" || typeof item.risk !== "string" ||
        !Array.isArray(item.choices) || !item.choices.every((choice) => oneOf(CHOICES, choice)) ||
        !Array.isArray(item.details) || !(item.diagnostic === null || typeof item.diagnostic === "string")) {
      throw new Error("The OS approval snapshot is malformed.");
    }
    const choices = item.choices.filter((choice): choice is ApprovalChoice => oneOf(CHOICES, choice));
    const details = item.details.map((detail: unknown) => {
      if (!record(detail) || typeof detail.label !== "string" || typeof detail.value !== "string") {
        throw new Error("The OS approval detail is malformed.");
      }
      return { label: detail.label, value: detail.value };
    });
    return {
      id: item.id, revision: item.revision, status: item.status, label: item.label,
      description: item.description, target: item.target, reason: item.reason,
      risk: item.risk, choices, verb: item.verb, details, diagnostic: item.diagnostic,
    };
  });
  const appReviews = value.app_reviews.map((item: unknown) => {
    if (!record(item) || typeof item.id !== "string" || !/^[A-Za-z0-9_-]{1,128}$/.test(item.id) ||
        typeof item.label !== "string") throw new Error("The OS App-review reference is malformed.");
    return { id: item.id, label: item.label };
  });
  const systemReviews = value.system_reviews.map(readSystemApproval);
  const identities = [...reviews, ...appReviews, ...systemReviews].map((review) => review.id);
  if (new Set(identities).size !== identities.length) throw new Error("The OS review inventory repeats a request identity.");
  return { ...(taskId ? { task_id: taskId } : {}), reviews, app_reviews: appReviews, system_reviews: systemReviews };
}

export const APPROVAL_LABELS: Record<ApprovalChoice, string> = {
  deny: "Deny", restore: "Restore App policy", ask: "Ask each time",
  allow_once: "Authorize once", allow_session: "Authorize for this session",
  allow_forever: "Authorize reusable access",
};

export const APPROVAL_STATUS_LABELS: Record<ApprovalStatus, string> = {
  pending: "Pending", confirmed: "Authorized", consumed: "Authorization used",
  completed: "Authorized", cancelled: "Denied", stale: "Changed",
  expired: "Expired", failed: "Failed",
};
export const SYSTEM_REVIEW_STATUS_LABELS: Record<ApprovalStatus, string> = {
  pending: "Separate OS confirmation required", confirmed: "Stop confirmation recorded",
  consumed: "Stop confirmation used — package outcome separate",
  completed: "OS review completed — package outcome separate",
  cancelled: "Stop review cancelled", stale: "Changed — refresh",
  expired: "Expired", failed: "Review failed",
};

const AUTHORIZATION_PHASES = ["starting", "prompt", "verifying", "finished", "failed", "cancelled", "expired"] as const;
const AUTHORIZATION_ACTIONS = ["confirm_install", "confirm_activation", "confirm_update", "confirm_global_app_stop"] as const;
export type AuthorizationAction = typeof AUTHORIZATION_ACTIONS[number];
export type AuthorizationPhase = typeof AUTHORIZATION_PHASES[number];
export type AuthorizationView = {
  id: string; review_id: string; revision: string; label: string; target: string;
  choice: ApprovalChoice | null; phase: AuthorizationPhase;
  action?: AuthorizationAction;
  prompt: { id: string; message: string } | null; message: string | null;
};

export function readAuthorization(value: unknown, expected: {
  review_id: string; revision: string; id?: string;
  choice?: ApprovalChoice | null; action?: AuthorizationAction;
}): AuthorizationView {
  if (!record(value) || typeof value.id !== "string" || !/^[a-f0-9]{64}$/.test(value.id) ||
      value.review_id !== expected.review_id || value.revision !== expected.revision ||
      (expected.id !== undefined && value.id !== expected.id) ||
      (expected.choice !== undefined && value.choice !== expected.choice) ||
      (expected.action !== undefined && value.action !== expected.action) ||
      typeof value.label !== "string" || typeof value.target !== "string" ||
      !(oneOf(CHOICES, value.choice) && value.choice !== "deny" && value.action === undefined ||
        value.choice === null && oneOf(AUTHORIZATION_ACTIONS, value.action)) ||
      !oneOf(AUTHORIZATION_PHASES, value.phase) ||
      !(value.message === null || typeof value.message === "string")) {
    throw new Error("The OS authentication response has an invalid identity or shape.");
  }
  let prompt: AuthorizationView["prompt"] = null;
  if (value.prompt !== null) {
    if (!record(value.prompt) || typeof value.prompt.id !== "string" ||
        !/^[a-f0-9]{64}$/.test(value.prompt.id) || typeof value.prompt.message !== "string") {
      throw new Error("The OS authentication prompt is invalid.");
    }
    prompt = { id: value.prompt.id, message: value.prompt.message };
  }
  if ((value.phase === "prompt") !== (prompt !== null)) {
    throw new Error("The OS authentication prompt does not match its state.");
  }
  return {
    id: value.id, review_id: expected.review_id, revision: expected.revision,
    label: value.label, target: value.target, choice: value.choice,
    ...(value.choice === null ? { action: value.action } : {}),
    phase: value.phase, prompt, message: value.message,
  };
}

export const systemReviewApi = {
  async authorize(review: SystemApprovalView): Promise<AuthorizationView> {
    if (review.status !== "pending" || !review.actions.includes("confirm_global_app_stop")) {
      throw new Error("The current OS review does not offer all-App stop confirmation.");
    }
    const value = await api.post(
      `/api/approvals/system-reviews/${encodeURIComponent(review.id)}/authorization`,
      { revision: review.revision }, { signal: AbortSignal.timeout(10_000) },
    );
    return readAuthorization(value, {
      review_id: review.id, revision: review.revision, choice: null, action: "confirm_global_app_stop",
    });
  },
  async cancel(review: SystemApprovalView): Promise<SystemApprovalView> {
    if (review.status !== "pending" || !review.actions.includes("cancel")) {
      throw new Error("The current OS review does not offer cancellation.");
    }
    const next = readSystemApproval(await api.post(
      `/api/approvals/system-reviews/${encodeURIComponent(review.id)}/cancel`,
      { revision: review.revision },
    ));
    if (next.id !== review.id || next.status === "pending") {
      throw new Error("The OS cancellation outcome is not confirmed; refresh instead of resubmitting.");
    }
    return next;
  },
};
