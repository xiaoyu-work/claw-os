import { afterEach, describe, expect, mock, spyOn, test } from "bun:test";

import { activityApi } from "../src/lib/activities";
import { api } from "../src/lib/api";
import {
  EFFECT_KINDS, EFFECT_RECOVERIES, readOperationPreview,
  type OperationInvocation, type OperationPreview,
} from "../src/lib/operation-preview";

const invocation: OperationInvocation = {
  app_id: "archive", operation: "get",
  args: ["--message=ARGUMENT_ONLY_PAYLOAD; $(printf inert)", "--", "../requested/draft.txt"],
};
const preview: OperationPreview = {
  schema: 1, app_id: "archive", app_name: "Archive", app_version: "1.0",
  package_digest: "fixture-digest", operation: "get", operation_label: "Inspect entry",
  effects_declared: true,
  effects: [{
    kind: "delete", label: "App-declared removal", recovery: "irreversible",
    target_arg: "path", target_kind: "path", requested_targets: ["../requested/draft.txt"],
    target_state: "requested",
  }],
  unresolved_arguments: ["provider"],
  authorization_checked: false, executed: false, effects_confirmed: false,
  notes: ["Metadata only; no runtime selectors evaluated."],
};

afterEach(() => mock.restore());

describe("operation preview contract", () => {
  test("preserves declared kinds, recovery modes and requested paths without inference", () => {
    for (const kind of EFFECT_KINDS) {
      for (const recovery of EFFECT_RECOVERIES) {
        const value = { ...preview, effects: [{ ...preview.effects[0], kind, recovery }] };
        expect(readOperationPreview(value, invocation)).toEqual(value);
      }
    }
    const result = readOperationPreview(preview, invocation);
    expect(result.effects[0].requested_targets).toEqual(["../requested/draft.txt"]);
    expect(JSON.stringify(result)).not.toContain("ARGUMENT_ONLY_PAYLOAD");
    expect(readOperationPreview({ ...preview, effects_declared: false, effects: [] }, invocation).effects).toEqual([]);
  });

  test("rejects grants, receipts, mismatched operations and malformed metadata", () => {
    for (const invalid of [
      { ...preview, authorization_checked: true },
      { ...preview, executed: true },
      { ...preview, effects_confirmed: true },
      { ...preview, executed: undefined },
      { ...preview, schema: 2 },
      { ...preview, app_id: "another" },
      { ...preview, operation: "another" },
      { ...preview, effects_declared: false },
      { ...preview, effects: undefined },
      { ...preview, effects: [{ ...preview.effects[0], kind: "safe" }] },
      { ...preview, effects: [{ ...preview.effects[0], recovery: "guaranteed" }] },
      { ...preview, effects: [{ ...preview.effects[0], target_kind: "text" }] },
      { ...preview, effects: [{ ...preview.effects[0], target_state: "canonical" }] },
      { ...preview, effects: [{ ...preview.effects[0], requested_targets: "arbitrary content" }] },
      { ...preview, unresolved_arguments: [42] },
      { ...preview, notes: "all good" },
    ]) {
      expect(() => readOperationPreview(invalid, invocation)).toThrow("Invalid operation preview");
    }
  });

  test("posts only invocation fields with opaque argv and an abort signal", async () => {
    const controller = new AbortController();
    const post = spyOn(api, "post").mockResolvedValue(preview);
    const get = spyOn(api, "get");
    const input = { ...invocation, owner_uid: 0, id: "override", executed: true };
    expect(await activityApi.previewOperation("activity 1", input, controller.signal)).toEqual(preview);
    expect(post).toHaveBeenCalledTimes(1);
    expect(post).toHaveBeenLastCalledWith("/api/activities/activity%201/operation-preview", invocation, {
      signal: controller.signal,
    });
    expect(get).not.toHaveBeenCalled();
  });

  test("broker errors never become a fabricated read-only or empty preview", async () => {
    spyOn(api, "post").mockRejectedValue(new Error("App package is quarantined"));
    await expect(activityApi.previewOperation("activity-1", invocation)).rejects.toThrow("App package is quarantined");
  });
});
