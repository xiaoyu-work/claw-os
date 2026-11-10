import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import test from "node:test";
import { resolve_owner_path, validate_owner_path, validate_owner_path_bindings } from "./manifest";
import { validateOwnerPathNeed } from "./generated";

test("owner-path shared vectors remain bounded lexical previews", () => {
  const vectors = JSON.parse(readFileSync(resolve(__dirname, "..", "..", "wire", "v1", "owner_path.vectors.json"), "utf8")) as {
    cases: Array<{ name: string; verb: string; path: string; home: string; expected?: string }>;
  };
  for (const entry of vectors.cases) {
    const preview = () => resolve_owner_path(entry.verb, entry.path, entry.home);
    if (entry.expected !== undefined) assert.equal(preview(), entry.expected, entry.name);
    else assert.throws(preview, entry.name);
  }
  assert.throws(() => validate_owner_path("fs.read", "é".repeat(128)));
  for (const field of ["owner_uid", "home", "owner_home"]) {
    assert.throws(() => validateOwnerPathNeed({ verb: "fs.read", scope: { kind: "owner-path", path: "Documents/**", [field]: "/home/bob" } }));
  }
});

test("owner-path validation covers operations MCP and each GUI declaration", () => {
  const bad = { needs: [{ verb: "fs.delete", scope: { kind: "owner-path", path: "../bob/**" } }] };
  for (const value of [
    { operations: { remove: bad } }, { mcp: { tools: [bad] } },
    { desktop: bad }, { desktop: { entries: { applet: bad } } },
  ]) assert.throws(() => validate_owner_path_bindings(value));
});

test("optional GUI lists validate owner paths without fallback on both surfaces", () => {
  for (const invalid of [null, "not-an-array", {}, [{
    verb: "fs.read", scope: { kind: "owner-path", path: "../other/**" },
  }]]) {
    for (const value of [
      { desktop: { optional_needs: invalid } },
      { desktop: { entries: { applet: { optional_needs: invalid } } } },
    ]) assert.throws(() => validate_owner_path_bindings(value));
  }
  validate_owner_path_bindings({ desktop: { optional_needs: [] } });
  validate_owner_path_bindings({ desktop: { entries: { applet: { optional_needs: [{
    verb: "fs.read", scope: { kind: "owner-path", path: "Documents/**" },
  }] } } } });
});
