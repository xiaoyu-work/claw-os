import { expect, test } from "bun:test";
import { APPROVAL_LABELS, readApprovalBundle } from "../src/lib/approvals";

const review = () => ({
  id:"approval-1", revision:"9007199254740993", status:"pending",
  label:"Read files", description:"Read the selected file.", target:"/tmp/report.txt",
  reason:"Requested report", risk:"medium", choices:["deny","allow_once"],
  verb:"fs.read", details:[], diagnostic:null,
});

test("approval views preserve exact revisions and only OS-supported choices", () => {
  const bundle = readApprovalBundle({task_id:"job-1",reviews:[review()],app_reviews:[]}, "job-1");
  expect(bundle.reviews[0].revision).toBe("9007199254740993");
  expect(bundle.reviews[0].choices).toEqual(["deny","allow_once"]);
  expect(APPROVAL_LABELS.allow_once).toBe("Authorize once");
  expect(() => readApprovalBundle({task_id:"job-other",reviews:[review()],app_reviews:[]}, "job-1")).toThrow();
  expect(() => readApprovalBundle({reviews:[{...review(),choices:["force_grant"]}],app_reviews:[]})).toThrow();
});
