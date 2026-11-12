import { mock, test } from "node:test";
import assert from "node:assert/strict";

import * as gui from "./gui";
import * as transport from "./transport";

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

test("isGuiLaunch requires host context", () => {
  withEnv({ COS_APP_GUI: undefined }, () => {
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

test("context requires app identity", () => {
  withEnv({ COS_APP_ID: undefined, COS_ARGS_JSON: undefined }, () => {
    assert.throws(() => gui.context(), /COS_APP_ID/);
  });
});

test("context rejects malformed args json", () => {
  withEnv({ COS_APP_ID: "notes", COS_ARGS_JSON: "not json" }, () => {
    assert.throws(() => gui.context(), /valid JSON/);
  });
  withEnv({ COS_APP_ID: "notes", COS_ARGS_JSON: '["/a",7]' }, () => {
    assert.throws(() => gui.context(), /array of strings/);
  });
});

test("explicit files override the environment", () => {
  const ctx = withEnv({ COS_APP_ID: "notes", COS_ARGS_JSON: JSON.stringify(["/x"]) }, () =>
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

test("selected read transports native nodes and keeps pending distinct from selected", () => {
  const selection = `sr-${"a".repeat(32)}`;
  const calls: unknown[][] = [];
  const primitive = mock.method(transport, "cosCallJson", (...args: unknown[]) => {
    calls.push(args);
    return { version: 1, selection_id: selection, node: 7, access: "read",
      state: calls.length === 1 ? "pending" : "selected" };
  });
  try {
    assert.equal(gui.requestFileRead(7).state, "pending");
    assert.equal(gui.fileReadStatus(selection, 7).state, "selected");
    assert.deepEqual(calls[0][1], ["__gui-file-read", "request", "--request-stdin"]);
    assert.deepEqual(JSON.parse(calls[0][3] as string), { node: 7 });
    assert.deepEqual(JSON.parse(calls[1][3] as string), { selection_id: selection, node: 7 });
    assert.throws(() => gui.requestFileRead(0));
    assert.throws(() => gui.fileReadStatus("foreign", 7));
    assert.equal(calls.length, 2);
    primitive.mock.mockImplementation(() => ({
      version: 1, selection_id: selection, node: 7, access: "read",
      state: "selected", owner_uid: 0,
    }));
    assert.throws(() => gui.requestFileRead(7));
  } finally { primitive.mock.restore(); }
});

test("selected write and atomic save transmit only Root resource references", () => {
  const selection = `sw-${"b".repeat(32)}`;
  const save = { version: 1, reservation_id: `ss-${"c".repeat(32)}`,
    selection_id: selection, node: 9, parent_node: 1,
    name: `.cos-save-${"c".repeat(32)}`, expires_in_ms: 170000 };
  const calls: unknown[][] = [];
  const primitive = mock.method(transport, "cosCallJson", (...args: unknown[]) => {
    calls.push(args);
    if (calls.length === 3) return save;
    return { version: 1, selection_id: selection, node: 9, access: "write",
      state: calls.length === 1 ? "pending" : "selected" };
  });

  test("one-shot creation keeps literal parent/name, selection, reservation and commit distinct", () => {
    const selection = `sc-${"d".repeat(32)}`;
    const name = "报告 ; '$x'.txt";
    const pending = { version: 1, selection_id: selection, parent_node: 1, name, access: "create", state: "pending" };
    const slot = { version: 1, reservation_id: `ss-${"e".repeat(32)}`, selection_id: selection,
      parent_node: 1, name: `.cos-save-${"e".repeat(32)}`, target_name: name, expires_in_ms: 190000 };
    const replies = [pending, { ...pending, state: "selected" }, slot, { ...pending, state: "created" }];
    const calls: unknown[][] = [];
    const primitive = mock.method(transport, "cosCallJson", (...args: unknown[]) => {
      calls.push(args);
      return replies[calls.length - 1];
    });
    try {
      assert.equal(gui.requestFileCreate(1, name).state, "pending");
      assert.equal(gui.fileCreateStatus(selection, 1).state, "selected");
      assert.equal(gui.reserveFileCreate(selection, 1).target_name, name);
      assert.equal(gui.fileCreateStatus(selection, 1).state, "created");
      for (const [index, action] of ["request", "status", "reserve", "status"].entries()) {
        assert.deepEqual(calls[index][1], ["__gui-file-create", action, "--request-stdin"]);
      }
      assert.deepEqual(JSON.parse(calls[0][3] as string), { parent_node: 1, name });
      for (const leaf of ["", ".", "..", "../leaf", "a/b", "a\\b", "a\0b", "\ud800", "é".repeat(128)]) {
        assert.throws(() => gui.requestFileCreate(1, leaf));
      }
      assert.throws(() => gui.requestFileCreate(0, "leaf"));
      assert.throws(() => gui.reserveFileCreate(`sw-${"d".repeat(32)}`, 1));
      assert.equal(calls.length, 4);
      primitive.mock.mockImplementation(() => ({ ...pending, state: "created", owner_uid: 0 }));
      assert.throws(() => gui.requestFileCreate(1, name));
    } finally { primitive.mock.restore(); }
  });
  try {
    assert.equal(gui.requestFileWrite(9).state, "pending");
    assert.equal(gui.fileWriteStatus(selection, 9).access, "write");
    assert.equal(gui.reserveFileSave(selection, 9).name, save.name);
    for (const [index, action] of ["request", "status", "reserve"].entries()) {
      assert.deepEqual(calls[index][1], ["__gui-file-write", action, "--request-stdin"]);
    }
    assert.deepEqual(JSON.parse(calls[0][3] as string), { node: 9 });
    assert.deepEqual(JSON.parse(calls[2][3] as string), { selection_id: selection, node: 9 });
    assert.throws(() => gui.reserveFileSave(`sr-${"b".repeat(32)}`, 9));
    primitive.mock.mockImplementation(() => ({ ...save, name: "../sibling" }));
    assert.throws(() => gui.reserveFileSave(selection, 9));
  } finally { primitive.mock.restore(); }
});
