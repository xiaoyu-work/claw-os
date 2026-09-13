import { api } from "@/lib/api";
import { oneOf, record } from "@/lib/api-shapes";
import { recordedTime } from "@/lib/activity-receipts";

export const CAPABILITY_POLICY_MODES = ["normal", "require_approval", "deny"] as const;
export const CAPABILITY_SCOPE_KINDS = ["path", "host", "name", "self-ref", "wild"] as const;
const CATALOG_SCOPE_KINDS = [...CAPABILITY_SCOPE_KINDS, "none"] as const;
export const MAX_POLICY_RULES = 64;
export const MAX_POLICY_SCOPES = 32;
export const MAX_POLICY_BYTES = 16 * 1024;

export type CapabilityPolicyMode = typeof CAPABILITY_POLICY_MODES[number];
export type CapabilityScope = { kind: "wild" }
  | { kind: "path" | "host" | "name" | "self-ref"; value: string };
export type CapabilityPolicyRule = {
  verb: string;
  mode: CapabilityPolicyMode;
  scopes: CapabilityScope[];
};
export type CapabilityPolicyDraft = { rules: CapabilityPolicyRule[] };
export type CapabilityPolicyVerb = {
  verb: string;
  scope_kind: typeof CATALOG_SCOPE_KINDS[number];
  label: string;
  description: string;
};
export type CapabilityPolicyCatalog = { schema: 1; verbs: CapabilityPolicyVerb[] };
export type ActivityCapabilityPolicy = CapabilityPolicyDraft & {
  activity_id: string;
  owner_uid: number;
  revision: number;
  enabled: boolean;
  created_at: string;
  updated_at: string;
};
export type CapabilityPolicyView = {
  schema: 1;
  activity_id: string;
  capability_policy: ActivityCapabilityPolicy | null;
};
export type CapabilityPolicyData = CapabilityPolicyView & { catalog: CapabilityPolicyCatalog };
export type CapabilityPolicyExpectation = { revision: number | null; enabled: boolean; ownerUid: number };

export const capabilityPolicyModeLabels: Record<CapabilityPolicyMode, string> = {
  normal: "Normal — existing permission checks",
  require_approval: "Ask — exact single-use approval",
  deny: "Deny — entire capability verb",
};

const integer = (value: unknown, minimum: number, maximum = Number.MAX_SAFE_INTEGER): value is number =>
  typeof value === "number" && Number.isSafeInteger(value) && value >= minimum && value <= maximum;
const only = (value: Record<string, unknown>, keys: string[]) =>
  Object.keys(value).length === keys.length && keys.every((key) => Object.prototype.hasOwnProperty.call(value, key));
const literalText = (value: unknown, maximum: number): value is string =>
  typeof value === "string" && value.length > 0 && value.length <= maximum
  && !/[\u0000-\u001f\u007f-\u009f]/.test(value);
const text = (value: unknown, maximum: number): value is string =>
  literalText(value, maximum) && !!value.trim();

export function readCapabilityPolicyCatalog(value: unknown): CapabilityPolicyCatalog {
  if (!record(value) || !only(value, ["schema", "verbs"]) || value.schema !== 1
    || !Array.isArray(value.verbs) || !value.verbs.length || value.verbs.length > 256) {
    throw new Error("Invalid capability-policy catalogue from the server.");
  }
  const seen = new Set<string>();
  const verbs = value.verbs.map((entry): CapabilityPolicyVerb => {
    if (!record(entry) || !only(entry, ["verb", "scope_kind", "label", "description"])
      || !text(entry.verb, 128) || !/^[a-z][a-z0-9]*(?:[.-][a-z0-9]+)*$/.test(entry.verb)
      || seen.has(entry.verb) || !oneOf(CATALOG_SCOPE_KINDS, entry.scope_kind)
      || !text(entry.label, 512) || !text(entry.description, 4096)) {
      throw new Error("Invalid capability-policy catalogue entry from the server.");
    }
    seen.add(entry.verb);
    return { verb: entry.verb, scope_kind: entry.scope_kind, label: entry.label, description: entry.description };
  });
  return { schema: 1, verbs };
}

export function capabilityScopeKinds(verb: CapabilityPolicyVerb): CapabilityScope["kind"][] {
  if (verb.scope_kind === "none" || verb.scope_kind === "wild") return ["wild"];
  if (verb.scope_kind === "self-ref") return ["self-ref", "wild"];
  return [verb.scope_kind];
}

export function emptyCapabilityScope(verb: CapabilityPolicyVerb): CapabilityScope {
  const kind = capabilityScopeKinds(verb)[0];
  return kind === "wild" ? { kind } : { kind, value: "" };
}

function readScope(value: unknown, verb: CapabilityPolicyVerb): CapabilityScope {
  if (!record(value) || !oneOf(CAPABILITY_SCOPE_KINDS, value.kind)
    || !capabilityScopeKinds(verb).includes(value.kind)) {
    throw new Error(`Use a compatible ${verb.scope_kind} scope for ${verb.verb}; raw wildcard is not a substitute.`);
  }
  if (value.kind === "wild" && only(value, ["kind"])) return { kind: "wild" };
  if (value.kind !== "wild" && only(value, ["kind", "value"]) && literalText(value.value, MAX_POLICY_BYTES)) {
    return { kind: value.kind, value: value.value };
  }
  throw new Error(`A ${verb.verb} scope must have a nonempty value without control characters.`);
}

export function readCapabilityPolicyDraft(value: unknown, catalog: CapabilityPolicyCatalog): CapabilityPolicyDraft {
  if (!record(value) || !only(value, ["rules"]) || !Array.isArray(value.rules)
    || value.rules.length > MAX_POLICY_RULES) {
    throw new Error("A capability-policy draft must contain at most 64 rules and no other fields.");
  }
  if (new TextEncoder().encode(JSON.stringify(value)).byteLength > MAX_POLICY_BYTES) {
    throw new Error("The capability-policy draft exceeds 16 KiB.");
  }
  const seen = new Set<string>();
  const rules = value.rules.map((rule, index): CapabilityPolicyRule => {
    const verb = record(rule) ? catalog.verbs.find((entry) => entry.verb === rule.verb) : undefined;
    if (!record(rule) || !only(rule, ["verb", "mode", "scopes"]) || !verb
      || !oneOf(CAPABILITY_POLICY_MODES, rule.mode) || !Array.isArray(rule.scopes)) {
      throw new Error(`Choose a known capability and mode for rule ${index + 1}.`);
    }
    if (seen.has(verb.verb)) throw new Error(`Only one rule per capability is allowed: ${verb.verb}.`);
    seen.add(verb.verb);
    if (rule.mode === "deny") {
      if (rule.scopes.length) throw new Error("Deny applies to the whole capability verb and must have no scopes.");
      return { verb: verb.verb, mode: rule.mode, scopes: [] };
    }
    if (!rule.scopes.length || rule.scopes.length > MAX_POLICY_SCOPES) {
      throw new Error(`Normal and Ask require 1–32 compatible scopes for ${verb.verb}.`);
    }
    return { verb: verb.verb, mode: rule.mode, scopes: rule.scopes.map((scope) => readScope(scope, verb)) };
  });
  return { rules };
}

export function readCapabilityPolicy(
  value: unknown, id: string, catalog: CapabilityPolicyCatalog,
): ActivityCapabilityPolicy {
  if (!record(value) || !only(value, [
    "activity_id", "owner_uid", "revision", "enabled", "rules", "created_at", "updated_at",
  ]) || value.activity_id !== id || !integer(value.owner_uid, 0, 0xffff_ffff)
    || !integer(value.revision, 1) || typeof value.enabled !== "boolean"
    || !recordedTime(value.created_at) || !recordedTime(value.updated_at)) {
    throw new Error("Invalid Activity capability policy from the server.");
  }
  const draft = readCapabilityPolicyDraft({ rules: value.rules }, catalog);
  return {
    ...draft, activity_id: id, owner_uid: value.owner_uid, revision: value.revision,
    enabled: value.enabled, created_at: value.created_at, updated_at: value.updated_at,
  };
}

export function readCapabilityPolicyView(
  value: unknown, id: string, catalog: CapabilityPolicyCatalog,
): CapabilityPolicyView {
  if (!record(value) || !only(value, ["schema", "activity_id", "capability_policy"])
    || value.schema !== 1 || value.activity_id !== id) {
    throw new Error("Invalid Activity capability-policy view from the server.");
  }
  return {
    schema: 1, activity_id: id,
    capability_policy: value.capability_policy === null ? null : readCapabilityPolicy(value.capability_policy, id, catalog),
  };
}

function checkExpectation(expected: CapabilityPolicyExpectation) {
  if (!integer(expected.ownerUid, 0, 0xffff_ffff) || typeof expected.enabled !== "boolean"
    || (expected.revision !== null && !integer(expected.revision, 1, Number.MAX_SAFE_INTEGER - 1))) {
    throw new Error("A safe current policy revision and owner are required. Refresh before making changes.");
  }
}

function checkAcknowledgement(saved: ActivityCapabilityPolicy, expected: CapabilityPolicyExpectation, enabled: boolean) {
  if (saved.owner_uid !== expected.ownerUid || saved.revision !== (expected.revision ?? 0) + 1
    || saved.enabled !== enabled) {
    throw new Error("Capability-policy acknowledgement does not match the owner, revision or enabled state. Refresh before retrying.");
  }
}

function matchesCanonicalRules(requested: CapabilityPolicyRule[], saved: CapabilityPolicyRule[]): boolean {
  if (requested.length !== saved.length) return false;
  return requested.every((rule) => {
    const actual = saved.find((entry) => entry.verb === rule.verb);
    if (!actual || actual.mode !== rule.mode) return false;
    const expectedScopes = new Set(rule.scopes.map((scope) => JSON.stringify(scope.kind === "host"
      ? { kind: scope.kind, value: scope.value.replace(/[A-Z]/g, (letter) => letter.toLowerCase()) }
      : scope)));
    const actualScopes = new Set(actual.scopes.map((scope) => JSON.stringify(scope)));
    return actualScopes.size === actual.scopes.length && actualScopes.size === expectedScopes.size
      && [...actualScopes].every((scope) => expectedScopes.has(scope));
  });
}

const endpoint = (id: string) => `/api/activities/${encodeURIComponent(id)}/capability-policy`;
export const capabilityPolicyApi = {
  get: async (id: string, signal?: AbortSignal): Promise<CapabilityPolicyData> => {
    const [metadata, raw] = await Promise.all([
      api.get<unknown>("/api/activities/capability-policy-catalog", { signal }),
      api.get<unknown>(endpoint(id), { signal }),
    ]);
    const catalog = readCapabilityPolicyCatalog(metadata);
    return { ...readCapabilityPolicyView(raw, id, catalog), catalog };
  },
  set: async (
    id: string, expected: CapabilityPolicyExpectation, draft: CapabilityPolicyDraft, catalog: CapabilityPolicyCatalog,
  ) => {
    checkExpectation(expected);
    const policy = readCapabilityPolicyDraft(draft, catalog);
    const saved = readCapabilityPolicy(await api.post<unknown>(endpoint(id), {
      expected_revision: expected.revision, policy,
    }), id, catalog);
    checkAcknowledgement(saved, expected, expected.revision === null ? true : expected.enabled);
    // Acknowledgement equality allows only root's case/order/dedup transforms, never scope coverage.
    if (!matchesCanonicalRules(policy.rules, saved.rules)) {
      throw new Error("Capability-policy acknowledgement changed the requested rules. Refresh before retrying.");
    }
    return saved;
  },
  enable: async (
    id: string, current: ActivityCapabilityPolicy, enabled: boolean, catalog: CapabilityPolicyCatalog,
  ) => {
    if (current.activity_id !== id || typeof enabled !== "boolean") {
      throw new Error("A policy for the selected Activity and an explicit enabled state are required.");
    }
    readCapabilityPolicy(current, id, catalog);
    const expected = { revision: current.revision, enabled: current.enabled, ownerUid: current.owner_uid };
    checkExpectation(expected);
    const saved = readCapabilityPolicy(await api.post<unknown>(`${endpoint(id)}/enabled`, {
      expected_revision: current.revision, enabled,
    }), id, catalog);
    checkAcknowledgement(saved, expected, enabled);
    if (JSON.stringify(saved.rules) !== JSON.stringify(current.rules)) {
      throw new Error("Capability-policy acknowledgement changed rules during enable/disable. Refresh before retrying.");
    }
    return saved;
  },
};
