import type {
  ActivityCapabilityPolicy, CapabilityPolicyCatalog, CapabilityPolicyData,
} from "../src/lib/capability-policy";

export const policyCatalog = (): CapabilityPolicyCatalog => ({
  schema: 1,
  verbs: [
    { verb: "fs.read", scope_kind: "path", label: "Read files", description: "Read file data." },
    { verb: "fs.write", scope_kind: "path", label: "Write files", description: "Change file data." },
    { verb: "fs.delete", scope_kind: "path", label: "Delete files", description: "Remove files." },
    { verb: "net.dial", scope_kind: "host", label: "Connect", description: "Connect to a host." },
    { verb: "secret.read", scope_kind: "name", label: "Read secrets", description: "Read a named secret." },
    { verb: "proc.spawn", scope_kind: "self-ref", label: "Spawn processes", description: "Start a process." },
    { verb: "ui.notify", scope_kind: "none", label: "Notify", description: "Show a notification." },
  ],
});

export const capabilityPolicyFixture = (id = "activity-1"): ActivityCapabilityPolicy => ({
  activity_id: id, owner_uid: 1000, revision: 1, enabled: true,
  rules: [
    { verb: "fs.delete", mode: "deny", scopes: [] },
    { verb: "fs.read", mode: "normal", scopes: [{ kind: "path", value: "/home/user/project/**" }] },
  ],
  created_at: "2026-09-11T00:00:00Z", updated_at: "2026-09-11T00:00:00Z",
});

export const capabilityPolicyData = (
  policy: ActivityCapabilityPolicy | null = capabilityPolicyFixture(), id = "activity-1",
): CapabilityPolicyData => ({
  schema: 1, activity_id: id, capability_policy: policy, catalog: policyCatalog(),
});
