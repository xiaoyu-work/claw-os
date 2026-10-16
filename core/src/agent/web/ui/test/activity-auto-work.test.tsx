import { afterEach, expect, spyOn, test, mock } from "bun:test";
import { api } from "../src/lib/api";
import { autoWorkApi, readAutoWork, readAutoWorkView } from "../src/lib/activity-auto-work";

const id = "aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee";
function state() {
  return {
    activity_id: id, owner_uid: 1000, revision: "9007199254740993",
    config: { task: "Prepare draft", check_interval_secs: 3600, wake_event: null },
    enabled: false, generation: null, phase: "ready", next_task: "Prepare draft",
    next_wake_at: null, event_after: "0", current: null, last_checkpoint: null,
    stagnant_segments: 0, message: null,
    created_at: "2026-10-15T00:00:00Z", updated_at: "2026-10-15T00:00:00Z",
  };
}

afterEach(() => mock.restore());

test("automatic-work views are closed and preserve exact large revisions", () => {
  expect(readAutoWork(state(), id).revision).toBe("9007199254740993");
  expect(readAutoWorkView({ schema: 1, activity_id: id, work: null, history: [] }, id).work).toBeNull();
  for (const value of [
    { ...state(), approved: true }, { ...state(), revision: 9007199254740992 },
    { ...state(), phase: "completed" }, { ...state(), enabled: true },
    { ...state(), owner_uid: -1 }, { ...state(), phase: "running" },
  ]) expect(() => readAutoWork(value, id)).toThrow();
});

test("automatic-work controls never send owner, progress, Job or authority selectors", async () => {
  const saved = { ...state(), revision: "9007199254740994", enabled: true, generation: id };
  const post = spyOn(api, "post").mockResolvedValue(saved);
  expect((await autoWorkApi.enabled(id, 1000, "9007199254740993", true)).enabled).toBe(true);
  expect(post).toHaveBeenCalledWith(`/api/activities/${id}/work/enabled`, {
    expected_revision: "9007199254740993", enabled: true, next_task: null, review_note: null,
  });
  post.mockResolvedValue({ ...saved, revision: "9007199254740995" });
  await expect(autoWorkApi.enabled(id, 1000, "9007199254740993", true)).rejects.toThrow();
  post.mockResolvedValue({ ...saved, owner_uid: 2000 });
  await expect(autoWorkApi.enabled(id, 1000, "9007199254740993", true)).rejects.toThrow();
});
