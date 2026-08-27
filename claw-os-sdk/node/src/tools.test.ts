import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";

import * as tools from "./tools";
import {
  WireDecimal,
  WireJsonSerializationError,
  stringifyWireJson,
} from "./generated";
import { installFakeCos, withCos } from "./testutil";

test("call builds argv and parses the result", () => {
  const fake = installFakeCos(
    JSON.stringify({ tool: "fs.read_text", app_id: "notes", status: "ok", result: { text: "hi" } }),
  );
  const res = withCos(fake, { COS_APP_ID: "notes" }, () =>
    tools.call("fs.read_text", { path: "/etc/hostname" }),
  );
  assert.equal(res.name, "fs.read_text");
  assert.equal(res.status, "ok");
  assert.deepEqual(res.value, { text: "hi" });

  const argv = readFileSync(fake.argvOut, "utf8").split("\n");
  assert.deepEqual(argv.slice(0, 5), ["ai", "tool", "fs.read_text", "--app", "notes"]);
  const argsIdx = argv.indexOf("--args");
  assert.ok(argsIdx >= 0);
  assert.deepEqual(JSON.parse(argv[argsIdx + 1]), { path: "/etc/hostname" });
});

test("call maps an error envelope to ToolDenied", () => {
  const fake = installFakeCos(JSON.stringify({ error: "unknown tool" }), 1);
  assert.throws(
    () => withCos(fake, { COS_APP_ID: "notes" }, () => tools.call("nope")),
    tools.ToolDenied,
  );
});

test("call requires an app id", () => {
  assert.throws(
    () => withCos(installFakeCos("{}"), { COS_APP_ID: undefined }, () => tools.call("x")),
    tools.ToolError,
  );
});

test("catalog parses tool rows", () => {
  const lexeme = "0.12345678901234567890";
  const fake = installFakeCos(`{"tools":[{
    "name":"fs.read_text","summary":"read a file","verb":"fs.read",
    "stability":"stable","args_schema":{"type":"number","minimum":${lexeme}},
    "returns_schema":{"type":"number","maximum":${lexeme}}
  }]}`);
  const rows = withCos(fake, {}, () => tools.catalog());
  assert.equal(rows.length, 1);
  assert.equal(rows[0].verb, "fs.read");
  assert.ok(rows[0].argsSchema?.minimum instanceof WireDecimal);
  assert.throws(() => JSON.stringify(rows[0].argsSchema), /stringifyWireJson/);
  assert.equal(stringifyWireJson(rows[0].argsSchema!), `{"type":"number","minimum":${lexeme}}`);
  assert.equal(stringifyWireJson(rows[0].returnsSchema!), `{"type":"number","maximum":${lexeme}}`);
  // catalog takes no --app
  const argv = readFileSync(fake.argvOut, "utf8").split("\n").filter(Boolean);
  assert.deepEqual(argv, ["ai", "tools"]);
});

test("call preserves explicit null, scalar, and array arguments", () => {
  for (const args of [null, "scalar", [1, true]]) {
    const fake = installFakeCos(
      JSON.stringify({ tool: "echo", app_id: "notes", status: "ok", result: null }),
    );
    withCos(fake, {}, () => tools.call("echo", args, { appId: "notes" }));
    const argv = readFileSync(fake.argvOut, "utf8").split("\n");
    assert.equal(argv[argv.indexOf("--args") + 1], stringifyWireJson(args));
  }
});

test("tool result preserves unsafe fractional lexemes", () => {
  const lexeme = "0.12345678901234567890";
  const fake = installFakeCos(
    `{"tool":"echo","app_id":"notes","status":"ok","result":${lexeme}}`,
  );
  const result = withCos(fake, {}, () => tools.call("echo", {}, { appId: "notes" }));
  assert.ok(result.value instanceof WireDecimal);
  assert.equal(stringifyWireJson(result.value), lexeme);
});

test("tool invocation rejects unsafe native numeric arguments", () => {
  for (const args of [Number.NaN, Number.POSITIVE_INFINITY, Number.MAX_SAFE_INTEGER + 1]) {
    assert.throws(
      () => tools.call("echo", args, { appId: "notes" }),
      WireJsonSerializationError,
    );
  }
});

test("forChat trims and drops empties", () => {
  assert.deepEqual(tools.forChat("fs.read_text", " kv.get ", ""), ["fs.read_text", "kv.get"]);
});
