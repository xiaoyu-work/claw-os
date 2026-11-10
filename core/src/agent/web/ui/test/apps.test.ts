import { expect, test } from "bun:test";
import { appMessage, readAppReview, readAppView, readStore } from "../src/lib/apps";
import { readAuthorization } from "../src/lib/approvals";

const nonce = "a".repeat(64);
const app = { id: "kv", name: "Key-value", state: "installed" as const, web_ready: true,
  tools: [{ name: "kv.get", command: "get" }] };

test("isolated App messages cannot invoke another App or a system API", () => {
  const message = { protocol: "claw.web-app/v1", type: "call", nonce, id: "web-1", tool: "kv.get", arguments: { key: "one" } };
  expect(appMessage(message, nonce, app)?.tool).toBe("kv.get");
  expect(appMessage({ ...message, tool: "email.send" }, nonce, app)).toBeNull();
  expect(appMessage({ ...message, type: "task.submit" }, nonce, app)).toBeNull();
  expect(appMessage({ ...message, nonce: "b".repeat(64) }, nonce, app)).toBeNull();
  expect(appMessage({ ...message, arguments: [] }, nonce, app)).toBeNull();
  expect(appMessage({ ...message, arguments: { value: "x".repeat(1024 * 1024) } }, nonce, app)).toBeNull();
});

test("App views bind the exact local resource route and owning identity", () => {
  const view = { status: "ready", view_id: nonce, nonce, src: `/app-ui/${nonce}/index.html`,
    app: { ...app, protocol: "claw.web-app/v1" } };
  expect(readAppView(view, "kv").app.id).toBe("kv");
  expect(() => readAppView(view, "email")).toThrow();
  expect(() => readAppView({ ...view, src: "https://external.example/index.html" }, "kv")).toThrow();
});

test("App installation confirmation is distinct from runtime capability authorization", () => {
  const review = {
    schema_version: 1, id: "rv-fixture", revision: "18446744073709551615", kind: "install",
    status: "pending", subject: { app_id: "kv", name: "Key-value", version: "1.0.0", publisher: "fixture" },
    permissions: [], disclosures: [],
  };
  expect(readAppReview(review).revision).toBe("18446744073709551615");
  expect(() => readAppReview({ ...review, revision: 9007199254740993 })).toThrow();
  const auth = {
    id: nonce, review_id: review.id, revision: review.revision, label: "Key-value", target: "kv",
    choice: null, action: "confirm_install", phase: "starting", prompt: null, message: null,
  };
  expect(readAuthorization(auth, { review_id: review.id, revision: review.revision }).choice).toBeNull();
  expect(() => readAuthorization({ ...auth, choice: "allow_forever" }, auth)).toThrow();
  expect(() => readAuthorization({ ...auth, action: "apply_choices" }, auth)).toThrow();
});

test("official Store Apps bind the exact APT package rather than a snapshot installer", () => {
  const checksum = "b".repeat(64);
  const delivery = "c".repeat(64);
  const value = { architecture: "amd64", installed: [], catalog: {
    schema: "claw.app-store/v2", repository: "xiaoyu-work/clawos-app", runtime_abi: 1,
    generation: "1", expires_at: new Date(Date.now() + 86_400_000).toISOString(),
    packages: [{ package: "claw-cap-storage-sdk", version: "0.2.1", architecture: "all",
      size: 8192, sha256: delivery, apps: ["db", "kv"] }],
    apps: [{ app_id: "kv", app_version: "1.0.0", release_version: "0.2.1", architecture: "all",
      artifact: { file: `claw-app-snapshot-${checksum}.tar.gz`, size: 4096,
        sha256: checksum, media_type: "application/gzip" },
      provenance: { content_digest: `sha256:${checksum}` },
      display: { name: "Key-value", description: "", source_name: "storage-sdk", source_kind: "capability" },
      delivery: { kind: "apt", package_sha256: delivery } }],
  } };
  expect(readStore(value).apps[0].package.size).toBe(8192);
  const changed = structuredClone(value);
  changed.catalog.apps[0].delivery.package_sha256 = "d".repeat(64);
  expect(() => readStore(changed)).toThrow();
  const wrongOwner = structuredClone(value);
  wrongOwner.catalog.packages[0].apps = ["db"];
  expect(() => readStore(wrongOwner)).toThrow();
  expect(() => readStore({ ...value, catalog: { ...value.catalog, schema: "claw.app-store/v1" } })).toThrow();
});
