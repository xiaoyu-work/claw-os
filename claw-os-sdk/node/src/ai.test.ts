import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";

import * as ai from "./ai";
import * as tools from "./tools";
import { WireDecimal, stringifyWireJson } from "./generated";
import {
  installFakeCos,
  wireError,
  wireSuccessJson,
  withCos,
} from "./testutil";

const OK_CHAT_DATA = JSON.stringify({
  verb: "ai.chat",
  text: "hello there",
  model: "m",
  provider: "p",
  usage: { input_tokens: 3, output_tokens: 5, units: 8 },
  budget: { period: "2026-05", units_used: 8, units_cap: 1000 },
  review: { safety: "strict", prompt_redacted: false },
  tool_calls: [{ id: "c1", name: "fs.read_text", input: { path: "/x" } }],
});
const OK_CHAT = wireSuccessJson(OK_CHAT_DATA);

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
  assert.deepEqual(argv.slice(0, 7), [
    "--wire=1",
    "ai",
    "chat",
    "--app",
    "notes",
    "--origin",
    "external-content",
  ]);
  assert.ok(argv.includes("--prompt-file"));
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

test("budget-exceeded code maps to AiBudgetExceeded", () => {
  const fake = installFakeCos(wireError("opaque", "BUDGET_EXCEEDED"), 1);
  assert.throws(
    () => withCos(fake, { COS_APP_ID: "notes" }, () => ai.chat("hi")),
    ai.AiBudgetExceeded,
  );
});

test("safety code maps to AiSafetyViolation", () => {
  const fake = installFakeCos(wireError("opaque", "SAFETY_VIOLATION"), 1);
  assert.throws(
    () => withCos(fake, { COS_APP_ID: "notes" }, () => ai.chat("hi")),
    ai.AiSafetyViolation,
  );
});

test("generic error envelope maps to AiDenied", () => {
  const fake = installFakeCos(wireError("capability denied", "PERMISSION_DENIED"), 1);
  assert.throws(
    () => withCos(fake, { COS_APP_ID: "notes" }, () => ai.chat("hi")),
    ai.AiDenied,
  );
});

test("stable error code determines the typed error", () => {
  const budget = installFakeCos(wireError("safety words", "BUDGET_EXCEEDED"), 1);
  assert.throws(
    () => withCos(budget, { COS_APP_ID: "notes" }, () => ai.chat("hi")),
    ai.AiBudgetExceeded,
  );

  const safety = installFakeCos(wireError("budget words", "SAFETY_VIOLATION"), 1);
  assert.throws(
    () => withCos(safety, { COS_APP_ID: "notes" }, () => ai.chat("hi")),
    ai.AiSafetyViolation,
  );
});

test("malformed tool call rejects the entire response", () => {
  const payload = JSON.parse(OK_CHAT_DATA) as Record<string, unknown>;
  payload.tool_calls = [{ id: "c1", input: {} }];
  const fake = installFakeCos(wireSuccessJson(JSON.stringify(payload)));
  assert.throws(
    () => withCos(fake, { COS_APP_ID: "notes" }, () => ai.chat("hi")),
    (error: unknown) =>
      error instanceof ai.AiUnavailable &&
      error.message.includes("WIRE_REQUIRED") &&
      error.message.includes("$.tool_calls[0].name"),
  );
});

test("chat preserves unrestricted tool input and mathematical integers", () => {
  const payload = wireSuccessJson(OK_CHAT_DATA
    .replace('"input_tokens":3', '"input_tokens":1.0')
    .replace('"output_tokens":5', '"output_tokens":1e0')
    .replace('"units":8', '"units":18446744073709551615')
    .replace('"input":{"path":"/x"}', '"input":["a",1]'));
  const fake = installFakeCos(payload);
  const response = withCos(fake, { COS_APP_ID: "notes" }, () => ai.chat("hi"));
  assert.equal(response.usage.inputTokens, 1);
  assert.equal(response.usage.outputTokens, 1);
  assert.equal(response.usage.units, 18446744073709551615n);
  assert.deepEqual(response.toolCalls[0].input, ["a", 1]);
});

test("chat rejects scalar root with stable wire error", () => {
  const fake = installFakeCos("null");
  assert.throws(
    () => withCos(fake, { COS_APP_ID: "notes" }, () => ai.chat("hi")),
    (error: unknown) =>
      error instanceof ai.AiUnavailable &&
      error.message.includes("WIRE_ONE_OF") &&
      error.message.includes(" at $:"),
  );
});

test("budget accepts the producer budget-show shape", () => {
  const fake = installFakeCos(
    wireSuccessJson(JSON.stringify({ app: "notes", period: "2026-08", units_used: 7 })),
  );
  const result = withCos(fake, {}, () => ai.budget("notes"));
  assert.deepEqual(result, { period: "2026-08", unitsUsed: 7, unitsCap: 0 });
});

test("proposed lossless input round-trips directly through tools.call", () => {
  const lexeme = "0.12345678901234567890";
  const chatFake = installFakeCos(
    wireSuccessJson(OK_CHAT_DATA.replace('"input":{"path":"/x"}', `"input":${lexeme}`)),
  );
  const response = withCos(chatFake, { COS_APP_ID: "notes" }, () => ai.chat("hi"));
  assert.ok(response.toolCalls[0].input instanceof WireDecimal);
  assert.equal(stringifyWireJson(response.toolCalls[0].input), lexeme);

  const toolFake = installFakeCos(
    wireSuccessJson(
      JSON.stringify({ tool: "echo", app_id: "notes", status: "ok", result: null }),
    ),
  );
  withCos(toolFake, {}, () =>
    tools.call("echo", response.toolCalls[0].input, { appId: "notes" }),
  );
  const argv = readFileSync(toolFake.argvOut, "utf8").split("\n");
  assert.equal(argv[argv.indexOf("--args") + 1], lexeme);
});
