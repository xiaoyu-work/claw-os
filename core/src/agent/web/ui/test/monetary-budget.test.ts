import { afterEach, expect, mock, spyOn, test } from "bun:test";
import { api } from "../src/lib/api";
import {
  formatMicrousd,
  monetaryBudgetApi,
  readMonetaryBudget,
  readMonetaryBudgetView,
  remainingMicrousd,
  type ActivityMonetaryBudget,
} from "../src/lib/monetary-budget";

afterEach(() => mock.restore());

const policy = (): ActivityMonetaryBudget => ({
  activity_id: "activity-1",
  owner_uid: 1000,
  revision: "9007199254740993",
  enabled: true,
  spent_microusd: "9007199254740993",
  reserved_microusd: "7",
  budget: {
    currency: "USD",
    max_total_microusd: "1000000000000",
    input_microusd_per_million_tokens: "250000",
    output_microusd_per_million_tokens: "1000000",
    max_output_tokens_per_turn: 4096,
  },
  created_at: "2026-09-13T00:00:00Z",
  updated_at: "2026-09-13T01:00:00Z",
});

test("monetary budget uses closed string integers and explicit absence without precision loss", () => {
  expect(readMonetaryBudgetView({
    schema: 1, activity_id: "activity-1", monetary_budget: null,
  }, "activity-1").monetary_budget).toBeNull();
  const value = readMonetaryBudget(policy(), "activity-1");
  expect(value.spent_microusd).toBe("9007199254740993");
  expect(remainingMicrousd(value)).toBe(0n);
  expect(formatMicrousd("5000001")).toBe("USD 5.000001");
  for (const invalid of [
    { ...policy(), currency: "USD" },
    { ...policy(), revision: 1 },
    { ...policy(), spent_microusd: 1 },
    { ...policy(), reserved_microusd: "-1" },
    { ...policy(), extra: true },
    { ...policy(), budget: { ...policy().budget, currency: "EUR" } },
    { ...policy(), budget: { ...policy().budget, max_total_microusd: "1000000000001" } },
    { ...policy(), budget: { ...policy().budget, max_output_tokens_per_turn: 0 } },
  ]) expect(() => readMonetaryBudget(invalid, "activity-1")).toThrow();
  expect(() => readMonetaryBudgetView({
    schema: 1, activity_id: "activity-1",
  }, "activity-1")).toThrow();
});

test("monetary budget writes send no owner or accounting selectors and require exact string CAS", async () => {
  const current = policy();
  current.revision = "7";
  current.spent_microusd = "2000000";
  current.reserved_microusd = "1000000";
  const saved = { ...current, revision: "8" };
  const toggled = { ...saved, enabled: false };
  const post = spyOn(api, "post").mockResolvedValue(saved);
  expect(await monetaryBudgetApi.set(
    "activity-1", 1000, "7", current.budget, current,
  )).toEqual(saved);
  expect(post).toHaveBeenLastCalledWith("/api/activities/activity-1/monetary-budget", {
    expected_revision: "7", budget: current.budget,
  });
  post.mockResolvedValue(toggled);
  expect(await monetaryBudgetApi.enable("activity-1", 1000, current, false)).toEqual(toggled);
  expect(post).toHaveBeenLastCalledWith("/api/activities/activity-1/monetary-budget/enabled", {
    expected_revision: "7", enabled: false,
  });
  expect(JSON.stringify(post.mock.calls)).not.toContain("owner_uid");
  expect(JSON.stringify(post.mock.calls)).not.toContain("spent_microusd");
  post.mockResolvedValue({ ...toggled, revision: "9" });
  await expect(monetaryBudgetApi.enable("activity-1", 1000, current, false))
    .rejects.toThrow("exact revision");
});

test("initial monetary policy is enabled and configured rates are exact", async () => {
  const created = {
    ...policy(),
    revision: "1",
    spent_microusd: "0",
    reserved_microusd: "0",
  };
  const post = spyOn(api, "post").mockResolvedValue(created);
  expect(await monetaryBudgetApi.set(
    "activity-1", 1000, null, created.budget,
  )).toEqual(created);
  expect(post).toHaveBeenCalledWith("/api/activities/activity-1/monetary-budget", {
    expected_revision: null, budget: created.budget,
  });
  post.mockResolvedValue({ ...created, enabled: false });
  await expect(monetaryBudgetApi.set("activity-1", 1000, null, created.budget))
    .rejects.toThrow("enabled state");
});
