import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";

import * as tools from "./tools";
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
  const fake = installFakeCos(
    JSON.stringify({
      tools: [
        {
          name: "fs.read_text",
          summary: "read a file",
          verb: "fs.read",
          stability: "stable",
          args_schema: { type: "object" },
          returns_schema: { type: "string" },
        },
      ],
    }),
  );
  const rows = withCos(fake, {}, () => tools.catalog());
  assert.equal(rows.length, 1);
  assert.equal(rows[0].verb, "fs.read");
  assert.deepEqual(rows[0].argsSchema, { type: "object" });
  // catalog takes no --app
  const argv = readFileSync(fake.argvOut, "utf8").split("\n").filter(Boolean);
  assert.deepEqual(argv, ["ai", "tools"]);
});

test("forChat trims and drops empties", () => {
  assert.deepEqual(tools.forChat("fs.read_text", " kv.get ", ""), ["fs.read_text", "kv.get"]);
});
