import { record, oneOf } from "./api-shapes";

export const CHOICES = ["deny", "restore", "ask", "allow_once", "allow_session", "allow_forever"] as const;
export type ApprovalChoice = typeof CHOICES[number];
const STATUSES = ["pending", "confirmed", "consumed", "completed", "cancelled", "stale", "expired", "failed"] as const;
export type ApprovalStatus = typeof STATUSES[number];
export type ApprovalView = {
  id: string; revision: string; status: ApprovalStatus; label: string; description: string;
  target: string; reason: string; risk: string; choices: ApprovalChoice[];
  verb: string; details: { label: string; value: string }[]; diagnostic: string | null;
};
export type ApprovalBundle = {
  task_id?: string; reviews: ApprovalView[]; app_reviews: { id: string; label: string }[];
};

export function readApprovalBundle(value: unknown, taskId?: string): ApprovalBundle {
  if (!record(value) || !Array.isArray(value.reviews) || !Array.isArray(value.app_reviews) ||
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
  return { ...(taskId ? { task_id: taskId } : {}), reviews, app_reviews: appReviews };
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

const AUTHORIZATION_PHASES = ["starting", "prompt", "verifying", "finished", "failed", "cancelled", "expired"] as const;
export type AuthorizationPhase = typeof AUTHORIZATION_PHASES[number];
export type AuthorizationView = {
  id: string; review_id: string; revision: string; label: string; target: string;
  choice: ApprovalChoice | null; phase: AuthorizationPhase;
  action?: "confirm_install" | "confirm_activation" | "confirm_update";
  prompt: { id: string; message: string } | null; message: string | null;
};

export function readAuthorization(value: unknown, expected: {
  review_id: string; revision: string; id?: string;
}): AuthorizationView {
  if (!record(value) || typeof value.id !== "string" || !/^[a-f0-9]{64}$/.test(value.id) ||
      value.review_id !== expected.review_id || value.revision !== expected.revision ||
      (expected.id !== undefined && value.id !== expected.id) ||
      typeof value.label !== "string" || typeof value.target !== "string" ||
      !(oneOf(CHOICES, value.choice) && value.choice !== "deny" && value.action === undefined ||
        value.choice === null && oneOf(["confirm_install", "confirm_activation", "confirm_update"] as const, value.action)) ||
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
