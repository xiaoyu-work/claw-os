import { afterEach, expect, mock, spyOn, test } from "bun:test";
import { api } from "../src/lib/api";
import {
  capabilityPolicyApi, readCapabilityPolicy, readCapabilityPolicyCatalog,
  readCapabilityPolicyDraft, readCapabilityPolicyView, type CapabilityPolicyDraft, type CapabilityScope,
} from "../src/lib/capability-policy";
import { capabilityPolicyFixture, policyCatalog } from "./capability-policy-fixtures";

afterEach(() => mock.restore());
const expected = { revision: 1, enabled: true, ownerUid: 1000 };

test("catalogue is bounded closed metadata, not App schemas or grants", () => {
  const catalog = policyCatalog();
  expect(readCapabilityPolicyCatalog(catalog)).toEqual(catalog);
  for (const value of [
    null, { ...catalog, capabilities: ["fs.read"] }, { ...catalog, schema: 2 },
    { ...catalog, verbs: [] },
    { ...catalog, verbs: [...catalog.verbs, catalog.verbs[0]] },
    { ...catalog, verbs: [{ ...catalog.verbs[0], scope_kind: "json-schema" }] },
    { ...catalog, verbs: [{ ...catalog.verbs[0], label: " " }] },
    { ...catalog, verbs: [{ ...catalog.verbs[0], fields: ["prompt"] }] },
  ]) expect(() => readCapabilityPolicyCatalog(value)).toThrow("catalogue");
});

test("policy responses require identity, safe revisions, explicit absence and complete rules", () => {
  const policy = capabilityPolicyFixture();
  const catalog = policyCatalog();
  expect(readCapabilityPolicy(policy, "activity-1", catalog)).toEqual(policy);
  expect(readCapabilityPolicyView({
    schema: 1, activity_id: "activity-1", capability_policy: null,
  }, "activity-1", catalog).capability_policy).toBeNull();
  for (const value of [
    { ...policy, owner_uid: -1 }, { ...policy, owner_uid: 0x1_0000_0000 },
    { ...policy, revision: 0 }, { ...policy, revision: Number.MAX_SAFE_INTEGER + 1 },
    { ...policy, enabled: "yes" }, { ...policy, activity_id: "other" },
    { ...policy, created_at: "yesterday" }, { ...policy, updated_at: "not-a-time" },
    { ...policy, priority: 100 }, { ...policy, rules: undefined },
  ]) expect(() => readCapabilityPolicy(value, "activity-1", catalog)).toThrow();
  for (const value of [
    { schema: 1, activity_id: "activity-1" },
    { schema: 1, activity_id: "another", capability_policy: null },
    { schema: 2, activity_id: "activity-1", capability_policy: null },
  ]) expect(() => readCapabilityPolicyView(value, "activity-1", catalog)).toThrow();
});

test("drafts distinguish normal, ask and whole-verb deny without providing coverage authority", () => {
  const catalog = policyCatalog();
  for (const rules of [
    [],
    capabilityPolicyFixture().rules,
    [{ verb: "net.dial", mode: "require_approval", scopes: [{ kind: "host", value: "EXAMPLE.COM:443" }] }],
    [{ verb: "secret.read", mode: "normal", scopes: [{ kind: "name", value: "service/token" }] }],
    [{ verb: "proc.spawn", mode: "normal", scopes: [{ kind: "self-ref", value: "self" }] }],
    [{ verb: "ui.notify", mode: "require_approval", scopes: [{ kind: "wild" }] }],
  ]) expect(readCapabilityPolicyDraft({ rules }, catalog).rules).toEqual(rules);

  for (const rule of [
    { verb: "fs.read", mode: "allow", scopes: [] },
    { verb: "fs.read", mode: "normal", scopes: [] },
    { verb: "fs.read", mode: "require_approval", scopes: [] },
    { verb: "fs.read", mode: "deny", scopes: [{ kind: "path", value: "/data/**" }] },
    { verb: "fs.read", mode: "normal", scopes: [{ kind: "wild" }] },
    { verb: "net.dial", mode: "normal", scopes: [{ kind: "wild" }] },
    { verb: "secret.read", mode: "normal", scopes: [{ kind: "wild" }] },
    { verb: "net.dial", mode: "normal", scopes: [{ kind: "path", value: "/data" }] },
    { verb: "fs.read", mode: "normal", scopes: [{ kind: "path", value: "" }] },
    { verb: "fs.read", mode: "normal", scopes: [{ kind: "path", value: "/data/\nfile" }] },
    { verb: "fs.read", mode: "normal", scopes: [{ kind: "path", value: "/data", grant: true }] },
    { verb: "ui.notify", mode: "normal", scopes: [{ kind: "wild", value: "*" }] },
    { verb: "invented.permission", mode: "deny", scopes: [] },
    { verb: "fs.delete", mode: "deny", scopes: [], approval: "always" },
  ]) expect(() => readCapabilityPolicyDraft({ rules: [rule] }, catalog)).toThrow();
  const rule = capabilityPolicyFixture().rules[1];
  expect(() => readCapabilityPolicyDraft({ rules: [rule, rule] }, catalog)).toThrow("one rule");
  expect(() => readCapabilityPolicyDraft({ rules: [rule], enabled: true }, catalog)).toThrow();
  expect(() => readCapabilityPolicyDraft({
    rules: [{ ...rule, scopes: Array.from({ length: 33 }, () => rule.scopes[0]) }],
  }, catalog)).toThrow("1–32");
  expect(() => readCapabilityPolicyDraft({
    rules: [{ ...rule, scopes: [{ kind: "path", value: "/data/" + "界".repeat(6000) }] }],
  }, catalog)).toThrow("16 KiB");
  const manyVerbs = Array.from({ length: 65 }, (_, index) => ({ ...catalog.verbs[0], verb: `test.cap-${index}` }));
  expect(() => readCapabilityPolicyDraft({
    rules: manyVerbs.map((verb) => ({ verb: verb.verb, mode: "deny", scopes: [] })),
  }, { schema: 1, verbs: manyVerbs })).toThrow("64");
});

test("read requests are abortable and use only the selected Activity plus static catalogue", async () => {
  const policy = capabilityPolicyFixture("with space");
  const get = spyOn(api, "get").mockImplementation(async (path) => path.endsWith("-catalog")
    ? policyCatalog() : { schema: 1, activity_id: "with space", capability_policy: policy });
  const signal = new AbortController().signal;
  const data = await capabilityPolicyApi.get("with space", signal);
  expect(data.capability_policy).toEqual(policy);
  expect(get).toHaveBeenCalledWith("/api/activities/capability-policy-catalog", { signal });
  expect(get).toHaveBeenCalledWith("/api/activities/with%20space/capability-policy", { signal });
});

test("literal scope values survive reads and writes without trimming or invented name restrictions", async () => {
  const draft: CapabilityPolicyDraft = {
    rules: [{ verb: "secret.read", mode: "normal", scopes: [
      { kind: "name", value: " " }, { kind: "name", value: " service/token " },
    ] }],
  };
  const catalog = policyCatalog();
  const saved = { ...capabilityPolicyFixture(), revision: 2, rules: draft.rules };
  expect(readCapabilityPolicyDraft(draft, catalog)).toEqual(draft);
  expect(readCapabilityPolicy(saved, "activity-1", catalog).rules).toEqual(draft.rules);
  const post = spyOn(api, "post").mockResolvedValue(saved);
  expect(await capabilityPolicyApi.set("activity-1", expected, draft, catalog)).toEqual(saved);
  expect(post).toHaveBeenCalledWith("/api/activities/activity-1/capability-policy", {
    expected_revision: 1, policy: draft,
  });
});

test("writes forward CAS without identity or enabled overrides and preserve root normalization", async () => {
  const policy = capabilityPolicyFixture();
  const catalog = policyCatalog();
  const post = spyOn(api, "post").mockResolvedValue({ ...policy, revision: 2 });
  expect((await capabilityPolicyApi.set("activity-1", expected, { rules: policy.rules }, catalog)).revision).toBe(2);
  expect(post).toHaveBeenLastCalledWith("/api/activities/activity-1/capability-policy", {
    expected_revision: 1, policy: { rules: policy.rules },
  });
  post.mockResolvedValue(policy);
  await capabilityPolicyApi.set("activity-1", { ...expected, revision: null }, { rules: policy.rules }, catalog);
  expect(post).toHaveBeenLastCalledWith("/api/activities/activity-1/capability-policy", {
    expected_revision: null, policy: { rules: policy.rules },
  });
  post.mockResolvedValue({ ...policy, revision: 2 });
  await capabilityPolicyApi.set("activity-1", expected, { rules: [...policy.rules].reverse() }, catalog);
  post.mockResolvedValue({
    ...policy, revision: 2,
    rules: [{ verb: "net.dial", mode: "require_approval", scopes: [{ kind: "host", value: "example.com:443" }] }],
  });
  await capabilityPolicyApi.set("activity-1", expected, {
    rules: [{ verb: "net.dial", mode: "require_approval", scopes: [{ kind: "host", value: "EXAMPLE.COM:443" }] }],
  }, catalog);
});

test("unmatched or ambiguous write acknowledgements fail instead of silently retrying", async () => {
  const policy = capabilityPolicyFixture();
  const post = spyOn(api, "post");
  for (const value of [
    policy, { ...policy, revision: 2, owner_uid: 2000 },
    { ...policy, revision: 2, enabled: false },
    { ...policy, revision: 2, activity_id: "another" },
    { ...policy, revision: 2, rules: [] },
  ]) {
    post.mockResolvedValue(value);
    await expect(capabilityPolicyApi.set("activity-1", expected, { rules: policy.rules }, policyCatalog())).rejects.toThrow();
  }
  expect(post).toHaveBeenCalledTimes(5);
  post.mockRejectedValue(new Error("Activity capability policy revision conflict"));
  await expect(capabilityPolicyApi.set("activity-1", expected, { rules: policy.rules }, policyCatalog())).rejects.toThrow("revision conflict");
  expect(post).toHaveBeenCalledTimes(6);
  await expect(capabilityPolicyApi.set("activity-1", { ...expected, revision: Number.MAX_SAFE_INTEGER }, { rules: [] }, policyCatalog())).rejects.toThrow("safe");
  expect(post).toHaveBeenCalledTimes(6);
});

test("set acknowledgements allow only ASCII host folding, rule/scope ordering and deduplication", async () => {
  const draft: CapabilityPolicyDraft = { rules: [
    { verb: "ui.notify", mode: "normal", scopes: [{ kind: "wild" }, { kind: "wild" }] },
    { verb: "net.dial", mode: "require_approval", scopes: [
      { kind: "host", value: "B.EXAMPLE.test" }, { kind: "host", value: "A.EXAMPLE.test" },
      { kind: "host", value: "a.example.test" },
    ] },
    { verb: "proc.spawn", mode: "normal", scopes: [{ kind: "self-ref", value: "self.children" }, { kind: "wild" }] },
    { verb: "fs.read", mode: "normal", scopes: [
      { kind: "path", value: "/workspace/b/**" }, { kind: "path", value: "/workspace/a/**" },
      { kind: "path", value: "/workspace/a/**" },
    ] },
  ] };
  const normalized: CapabilityPolicyDraft = { rules: [
    { verb: "fs.read", mode: "normal", scopes: [
      { kind: "path", value: "/workspace/a/**" }, { kind: "path", value: "/workspace/b/**" },
    ] },
    { verb: "net.dial", mode: "require_approval", scopes: [
      { kind: "host", value: "a.example.test" }, { kind: "host", value: "b.example.test" },
    ] },
    { verb: "proc.spawn", mode: "normal", scopes: [{ kind: "wild" }, { kind: "self-ref", value: "self.children" }] },
    { verb: "ui.notify", mode: "normal", scopes: [{ kind: "wild" }] },
  ] };
  const saved = { ...capabilityPolicyFixture(), revision: 2, ...normalized };
  const post = spyOn(api, "post").mockResolvedValue(saved);
  expect(await capabilityPolicyApi.set("activity-1", expected, draft, policyCatalog())).toEqual(saved);
  expect(post).toHaveBeenCalledWith("/api/activities/activity-1/capability-policy", {
    expected_revision: 1, policy: draft,
  });
});

test("matching modes cannot conceal scope substitution, trimming, case changes or fuzzy coverage", async () => {
  const cases: Array<{ verb: string; requested: CapabilityScope[]; reported: CapabilityScope[] }> = [
    { verb: "fs.read", requested: [{ kind: "path", value: "/workspace/project/**" }], reported: [{ kind: "path", value: "/**" }] },
    { verb: "fs.read", requested: [{ kind: "path", value: "/workspace/project/**" }], reported: [{ kind: "path", value: "/workspace/./project/**" }] },
    { verb: "fs.read", requested: [{ kind: "path", value: "/workspace/FILE" }], reported: [{ kind: "path", value: "/workspace/file" }] },
    { verb: "secret.read", requested: [{ kind: "name", value: " token " }], reported: [{ kind: "name", value: "token" }] },
    { verb: "secret.read", requested: [{ kind: "name", value: "service/*" }], reported: [{ kind: "name", value: "**" }] },
    { verb: "proc.spawn", requested: [{ kind: "self-ref", value: "self.children" }], reported: [{ kind: "self-ref", value: "self" }] },
    { verb: "proc.spawn", requested: [{ kind: "self-ref", value: "self.children" }], reported: [{ kind: "wild" }] },
    { verb: "net.dial", requested: [{ kind: "host", value: "*.EXAMPLE.test" }], reported: [{ kind: "host", value: "**.example.test" }] },
    { verb: "net.dial", requested: [{ kind: "host", value: " EXAMPLE.test " }], reported: [{ kind: "host", value: "example.test" }] },
    { verb: "net.dial", requested: [{ kind: "host", value: "\u00dc.EXAMPLE.test" }], reported: [{ kind: "host", value: "\u00fc.example.test" }] },
    { verb: "net.dial", requested: [{ kind: "host", value: "EXAMPLE.test" }], reported: [{ kind: "host", value: "EXAMPLE.test" }] },
    { verb: "fs.read", requested: [{ kind: "path", value: "/workspace/a/**" }], reported: [
      { kind: "path", value: "/workspace/a/**" }, { kind: "path", value: "/workspace/b/**" },
    ] },
    { verb: "fs.read", requested: [{ kind: "path", value: "/workspace/a/**" }], reported: [
      { kind: "path", value: "/workspace/a/**" }, { kind: "path", value: "/workspace/a/**" },
    ] },
    { verb: "fs.read", requested: [
      { kind: "path", value: "/workspace/a/**" }, { kind: "path", value: "/workspace/b/**" },
    ], reported: [{ kind: "path", value: "/workspace/a/**" }] },
  ];
  const post = spyOn(api, "post");
  for (const { verb, requested, reported } of cases) {
    const draft: CapabilityPolicyDraft = { rules: [{ verb, mode: "normal", scopes: requested }] };
    post.mockResolvedValue({
      ...capabilityPolicyFixture(), revision: 2, rules: [{ verb, mode: "normal", scopes: reported }],
    });
    await expect(capabilityPolicyApi.set("activity-1", expected, draft, policyCatalog())).rejects.toThrow("requested rules");
  }
  expect(post).toHaveBeenCalledTimes(cases.length);
});

test("disable and re-enable use the same revision contract and cannot change rules", async () => {
  const policy = capabilityPolicyFixture();
  const post = spyOn(api, "post").mockResolvedValue({ ...policy, revision: 2, enabled: false });
  const disabled = await capabilityPolicyApi.enable("activity-1", policy, false, policyCatalog());
  expect(disabled.rules).toEqual(policy.rules);
  expect(post).toHaveBeenLastCalledWith("/api/activities/activity-1/capability-policy/enabled", {
    expected_revision: 1, enabled: false,
  });
  post.mockResolvedValue({ ...disabled, revision: 3, enabled: true });
  await capabilityPolicyApi.enable("activity-1", disabled, true, policyCatalog());
  post.mockResolvedValue({ ...disabled, revision: 3, enabled: true, rules: [] });
  await expect(capabilityPolicyApi.enable("activity-1", disabled, true, policyCatalog())).rejects.toThrow("changed rules");
  await expect(capabilityPolicyApi.enable("another", disabled, true, policyCatalog())).rejects.toThrow("selected Activity");
  expect(post).toHaveBeenCalledTimes(3);
});
