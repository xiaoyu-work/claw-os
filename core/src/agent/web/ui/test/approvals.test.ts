import { afterEach, expect, mock, spyOn, test } from "bun:test";
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { api } from "../src/lib/api";
import { SystemApprovalCard } from "../src/components/chat-approvals";
import { APPROVAL_LABELS, APPROVAL_STATUS_LABELS, SYSTEM_REVIEW_STATUS_LABELS, readApprovalBundle, readAuthorization, readSystemApproval, systemReviewApi } from "../src/lib/approvals";

afterEach(() => mock.restore());

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
  const bundle = readApprovalBundle({task_id:"job-1",reviews:[review()],app_reviews:[],system_reviews:[]}, "job-1");
  expect(bundle.reviews[0].revision).toBe("9007199254740993");
  expect(bundle.reviews[0].choices).toEqual(["deny","allow_once"]);
  expect(APPROVAL_LABELS.allow_once).toBe("Authorize once");
  expect(APPROVAL_STATUS_LABELS.completed).toBe("Authorized");
  expect(APPROVAL_STATUS_LABELS.cancelled).toBe("Denied");
  expect(() => readApprovalBundle({task_id:"job-other",reviews:[review()],app_reviews:[],system_reviews:[]}, "job-1")).toThrow();
  expect(() => readApprovalBundle({reviews:[{...review(),choices:["force_grant"]}],app_reviews:[],system_reviews:[]})).toThrow();
});

function packageImpact() {
  return {
    id: "pg-synthetic", revision: "9007199254740993", kind: "package_impact", status: "pending",
    status_message: "Confirmation is not package success.",
    subject: { app_id: null, name: "All-App stop", version: null, publisher: null },
    initiator: "Root package transaction", context: [{ label: "Plan", value: "<script>not executable</script>" }],
    disclosures: [{ label: "Scope", value: "All users; unsaved work may be lost." }],
    contract_digest: "d".repeat(64), actions: ["cancel", "confirm_global_app_stop"],
  };
}

test("package-impact reviews are separate, closed, revision-bound OS views", () => {
  const bundle = readApprovalBundle({
    reviews: [], app_reviews: [], system_reviews: [packageImpact()],
  });
  expect(bundle.system_reviews[0].revision).toBe("9007199254740993");
  expect(bundle.app_reviews).toEqual([]);
  for (const changed of [
    { ...packageImpact(), kind: "install" },
    { ...packageImpact(), confirmed: true },
    { ...packageImpact(), owner_uid: 0 },
    { ...packageImpact(), actions: ["confirm_install"] },
    { ...packageImpact(), status: "consumed" },
    { ...packageImpact(), revision: 9007199254740992 },
    { ...packageImpact(), subject: { ...packageImpact().subject, app_id: "pkg" } },
  ]) expect(() => readSystemApproval(changed)).toThrow();
  expect(SYSTEM_REVIEW_STATUS_LABELS.consumed).toContain("package outcome separate");
});

test("all-App stop is an OS authentication action, not an App install or capability choice", () => {
  const review = readSystemApproval(packageImpact());
  const expected = {
    review_id: review.id, revision: review.revision, choice: null, action: "confirm_global_app_stop" as const,
  };
  const state = {
    id: "a".repeat(64), ...expected, label: review.subject.name, target: "All users",
    phase: "starting", prompt: null, message: null,
  };
  expect(readAuthorization(state, expected).phase).toBe("starting");
  expect(() => readAuthorization({ ...state, action: "confirm_install" }, expected)).toThrow();
  expect(() => readAuthorization({ ...state, choice: "allow_once" }, expected)).toThrow();
});

test("global-stop buttons use only the password start or owner cancellation route", async () => {
  const review = readSystemApproval(packageImpact());
  const post = spyOn(api, "post").mockResolvedValue({
    id: "a".repeat(64), review_id: review.id, revision: review.revision,
    label: review.subject.name, target: "All users", choice: null,
    action: "confirm_global_app_stop", phase: "starting", prompt: null, message: null,
  });
  expect((await systemReviewApi.authorize(review)).phase).toBe("starting");
  expect(post).toHaveBeenLastCalledWith(
    `/api/approvals/system-reviews/${review.id}/authorization`,
    { revision: review.revision }, expect.objectContaining({ signal: expect.any(AbortSignal) }),
  );
  await expect(systemReviewApi.authorize({ ...review, actions: ["cancel"] })).rejects.toThrow();
  expect(post).toHaveBeenCalledTimes(1);
  post.mockResolvedValue({ ...packageImpact(), revision: "9007199254740994", status: "cancelled", actions: [] });
  expect((await systemReviewApi.cancel(review)).status).toBe("cancelled");
  expect(post).toHaveBeenLastCalledWith(
    `/api/approvals/system-reviews/${review.id}/cancel`, { revision: review.revision },
  );
});

test("top-level OS stop presentation shows inert Root evidence and never labels package success", () => {
  const value = readSystemApproval(packageImpact());
  const html = renderToStaticMarkup(createElement(SystemApprovalCard, {
    review: value, onChanged: () => {},
  }));
  expect(html).toContain("Unsaved work may be lost");
  expect(html).toContain("Authenticate to confirm this all-App stop");
  expect(html).toContain("&lt;script&gt;not executable&lt;/script&gt;");
  expect(html).not.toContain("<script>");
  expect(html).toContain("does not mean the package transaction succeeded");
  const used = renderToStaticMarkup(createElement(SystemApprovalCard, {
    review: { ...value, status: "consumed", actions: [] }, onChanged: () => {},
  }));
  expect(used).toContain("package outcome separate");
  expect(used).not.toContain("Authenticate to confirm");
});
