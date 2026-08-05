import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";

import * as ai from "./ai";
import { installFakeCos, withCos } from "./testutil";

const OK_CHAT = JSON.stringify({
  verb: "ai.chat",
  text: "hello there",
  model: "m",
  provider: "p",
  usage: { input_tokens: 3, output_tokens: 5, units: 8 },
  budget: { period: "2026-05", units_used: 8, units_cap: 1000 },
  review: { safety: "strict", prompt_redacted: false },
  tool_calls: [{ id: "c1", name: "fs.read_text", input: { path: "/x" } }],
});

test("chat builds the right argv and parses the envelope", () => {
  const fake = installFakeCos(OK_CHAT);
  const res = withCos(fake, { COS_APP_ID: "notes" }, () =>
    ai.chat("summarise", { origin: "external-content", maxUnits: 100, tools: ["fs.read_text"] }),
  );
  assert.equal(res.text, "hello there");
  assert.equal(res.usage.units, 8);
  assert.equal(res.budget.unitsCap, 1000);
  assert.equal(res.toolCalls.length, 1);
  assert.equal(res.toolCalls[0].name, "fs.read_text");

  const argv = readFileSync(fake.argvOut, "utf8").split("\n");
  assert.deepEqual(argv.slice(0, 6), [
    "ai",
    "chat",
    "--app",
    "notes",
    "--origin",
    "external-content",
  ]);
  assert.ok(argv.includes("--prompt"));
  assert.ok(argv.includes("--max-units"));
  assert.ok(argv.includes("--tools"));
});

test("chat rejects an empty prompt", () => {
  assert.throws(() => ai.chat("  "), ai.AiError);
});

test("chat requires an app id", () => {
  const fake = installFakeCos(OK_CHAT);
  assert.throws(
    () => withCos(fake, { COS_APP_ID: undefined }, () => ai.chat("hi")),
    ai.AiError,
  );
});

test("budget-exceeded envelope maps to AiBudgetExceeded", () => {
  const fake = installFakeCos(JSON.stringify({ error: "monthly budget exceeded" }), 1);
  assert.throws(
    () => withCos(fake, { COS_APP_ID: "notes" }, () => ai.chat("hi")),
    ai.AiBudgetExceeded,
  );
});

test("safety envelope maps to AiSafetyViolation", () => {
  const fake = installFakeCos(JSON.stringify({ error: "prompt injection detected" }), 1);
  assert.throws(
    () => withCos(fake, { COS_APP_ID: "notes" }, () => ai.chat("hi")),
    ai.AiSafetyViolation,
  );
});

test("generic error envelope maps to AiDenied", () => {
  const fake = installFakeCos(JSON.stringify({ error: "capability denied" }), 1);
  assert.throws(
    () => withCos(fake, { COS_APP_ID: "notes" }, () => ai.chat("hi")),
    ai.AiDenied,
  );
});

test("embed is an unsupported compatibility shim", () => {
  assert.throws(
    () => ai.embed(""),
    (err: unknown) =>
      err instanceof ai.AiUnsupported &&
      err.modality === "embed" &&
      err.message.includes("only chat/chat-untrusted are stable"),
  );
});
