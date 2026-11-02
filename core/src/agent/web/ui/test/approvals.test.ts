import { expect, test } from "bun:test";
import { APPROVAL_LABELS, APPROVAL_STATUS_LABELS, readApprovalBundle, readAuthorization } from "../src/lib/approvals";

const review = () => ({
  id:"approval-1", revision:"9007199254740993", status:"pending",
  label:"Read files", description:"Read the selected file.", target:"/tmp/report.txt",
  reason:"Requested report", risk:"medium", choices:["deny","allow_once"],
  verb:"fs.read", details:[], diagnostic:null,
});

test("OS authentication is bound to the displayed request and a current prompt", () => {
  const expected = { review_id: "approval-1", revision: "9007199254740993" };
  const state = {
    id: "a".repeat(64), ...expected, label: "Read files", target: "/tmp/report.txt",
    choice: "allow_once", phase: "prompt",
    prompt: { id: "b".repeat(64), message: "Password:" }, message: null,
  };
  expect(readAuthorization(state, expected).prompt?.id).toBe("b".repeat(64));
  expect(() => readAuthorization({ ...state, review_id: "other" }, expected)).toThrow();
  expect(() => readAuthorization({ ...state, revision: "2" }, expected)).toThrow();
  expect(() => readAuthorization({ ...state, choice: "deny" }, expected)).toThrow();
  expect(() => readAuthorization({ ...state, prompt: null }, expected)).toThrow();
  expect(() => readAuthorization({ ...state, phase: "finished" }, expected)).toThrow();
  expect(() => readAuthorization(state, { ...expected, id: "c".repeat(64) })).toThrow();
});

test("approval views preserve exact revisions and only OS-supported choices", () => {
  const bundle = readApprovalBundle({task_id:"job-1",reviews:[review()],app_reviews:[]}, "job-1");
  expect(bundle.reviews[0].revision).toBe("9007199254740993");
  expect(bundle.reviews[0].choices).toEqual(["deny","allow_once"]);
  expect(APPROVAL_LABELS.allow_once).toBe("Authorize once");
  expect(APPROVAL_STATUS_LABELS.completed).toBe("Authorized");
  expect(APPROVAL_STATUS_LABELS.cancelled).toBe("Denied");
  expect(() => readApprovalBundle({task_id:"job-other",reviews:[review()],app_reviews:[]}, "job-1")).toThrow();
  expect(() => readApprovalBundle({reviews:[{...review(),choices:["force_grant"]}],app_reviews:[]})).toThrow();
});
