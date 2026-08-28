import { test } from "node:test";
import assert from "node:assert/strict";

import * as gui from "./gui";

function withEnv<T>(env: Record<string, string | undefined>, fn: () => T): T {
  const saved: Record<string, string | undefined> = {};
  for (const k of Object.keys(env)) saved[k] = process.env[k];
  for (const [k, v] of Object.entries(env)) {
    if (v === undefined) delete process.env[k];
    else process.env[k] = v;
  }
  try {
    return fn();
  } finally {
    for (const [k, v] of Object.entries(saved)) {
      if (v === undefined) delete process.env[k];
      else process.env[k] = v;
    }
  }
}

test("isGuiLaunch detects COS_APP_GUI", () => {
  withEnv({ COS_APP_GUI: "1" }, () => {
    assert.equal(gui.isGuiLaunch(), true);
  });
});

test("isGuiLaunch falls back to the --gui command", () => {
  withEnv({ COS_APP_GUI: undefined }, () => {
    assert.equal(gui.isGuiLaunch("--gui"), true);
    assert.equal(gui.isGuiLaunch("open"), false);
    assert.equal(gui.isGuiLaunch(), false);
  });
});

test("context decodes files from COS_ARGS_JSON", () => {
  const ctx = withEnv(
    { COS_APP_ID: "notes", COS_ARGS_JSON: JSON.stringify(["/a.md", "/b.md"]) },
    () => gui.context(),
  );
  assert.equal(ctx.appId, "notes");
  assert.deepEqual(ctx.files, ["/a.md", "/b.md"]);
});

test("context defaults app id and ignores malformed args json", () => {
  const ctx = withEnv({ COS_APP_ID: undefined, COS_ARGS_JSON: "not json" }, () =>
    gui.context(),
  );
  assert.equal(ctx.appId, "unknown");
  assert.deepEqual(ctx.files, []);
});

test("explicit files override the environment", () => {
  const ctx = withEnv({ COS_ARGS_JSON: JSON.stringify(["/x"]) }, () =>
    gui.context(["/explicit"]),
  );
  assert.deepEqual(ctx.files, ["/explicit"]);
});

test("openAgentOverlay rejects when the binary is missing", async () => {
  const ctx = new gui.GuiContext("notes", []);
  await withEnv({ COS_AGENT_UI_BIN: "/nonexistent/attacker" }, async () => {
    let result!: Promise<void>;
    assert.doesNotThrow(() => {
      result = ctx.openAgentOverlay();
    });
    await assert.rejects(result);
  });
});
