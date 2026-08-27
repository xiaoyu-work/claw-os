import { test } from "node:test";
import assert from "node:assert/strict";

import {
  WIRE_ENUM,
  WIRE_MAXIMUM,
  WIRE_MINIMUM,
  WIRE_REQUIRED,
  WIRE_TYPE,
  WIRE_UNKNOWN_FIELD,
  WireDecodeError,
  validateAi,
  validateBudgetShow,
  validateTool,
  validateToolCatalog,
} from "./generated";

function validAi(): Record<string, unknown> {
  return {
    text: "hello",
    model: "m",
    provider: "p",
    verb: "ai.chat",
    usage: { input_tokens: 1, output_tokens: 2, units: 3 },
    budget: { period: "2026-08", units_used: 3, units_cap: 100 },
    review: { safety: "strict", prompt_redacted: false },
    tool_calls: [{ id: "c1", name: "echo", input: { value: "ok" } }],
  };
}

test("AI validator enforces the shared contract", () => {
  const cases: Array<[Record<string, unknown>, string, string]> = [];

  const missing = validAi();
  delete missing.text;
  cases.push([missing, WIRE_REQUIRED, "$.text"]);

  const wrongType = validAi();
  (wrongType.usage as Record<string, unknown>).input_tokens = "1";
  cases.push([wrongType, WIRE_TYPE, "$.usage.input_tokens"]);

  const belowMinimum = validAi();
  (belowMinimum.usage as Record<string, unknown>).units = -1;
  cases.push([belowMinimum, WIRE_MINIMUM, "$.usage.units"]);

  const invalidEnum = validAi();
  invalidEnum.verb = "ai.unknown";
  cases.push([invalidEnum, WIRE_ENUM, "$.verb"]);

  const unknownNested = validAi();
  (unknownNested.usage as Record<string, unknown>).extra = true;
  cases.push([unknownNested, WIRE_UNKNOWN_FIELD, "$.usage.extra"]);

  const malformedCall = validAi();
  delete ((malformedCall.tool_calls as Array<Record<string, unknown>>)[0]).name;
  cases.push([malformedCall, WIRE_REQUIRED, "$.tool_calls[0].name"]);

  for (const [payload, code, path] of cases) {
    assert.throws(
      () => validateAi(payload),
      (error: unknown) =>
        error instanceof WireDecodeError &&
        error.code === code &&
        error.path === path,
    );
  }
});

test("integer validation uses JSON Schema mathematical semantics", () => {
  for (const literal of ["1.0", "1e0", "9007199254740991"]) {
    const payload = JSON.parse(JSON.stringify(validAi()).replace('"units":3', `"units":${literal}`));
    assert.doesNotThrow(() => validateAi(payload));
  }

  const fractional = JSON.parse(JSON.stringify(validAi()).replace('"units":3', '"units":1.5'));
  assert.throws(
    () => validateAi(fractional),
    (error: unknown) => error instanceof WireDecodeError && error.code === WIRE_TYPE,
  );

  for (const literal of ["9007199254740992", "18446744073709551616"]) {
    const payload = JSON.parse(JSON.stringify(validAi()).replace('"units":3', `"units":${literal}`));
    assert.throws(
      () => validateAi(payload),
      (error: unknown) =>
        error instanceof WireDecodeError &&
        error.code === WIRE_MAXIMUM &&
        error.path === "$.usage.units",
    );
  }
});

test("v1 tool inputs remain unrestricted", () => {
  for (const input of ["scalar", [1, true], null]) {
    const payload = validAi();
    ((payload.tool_calls as Array<Record<string, unknown>>)[0]).input = input;
    assert.doesNotThrow(() => validateAi(payload));
  }
});

test("structured items are validated without skipping", () => {
  assert.doesNotThrow(() => validateAi(validAi()));
  assert.doesNotThrow(() =>
    validateTool({ tool: "echo", app_id: "app", status: "ok", result: null }),
  );
  assert.throws(
    () =>
      validateToolCatalog({
        tools: [
          {
            name: "echo",
            summary: "Echo",
            verb: "ipc.invoke",
            stability: "stable",
            args_schema: {},
            returns_schema: {},
          },
          7,
        ],
      }),
    (error: unknown) =>
      error instanceof WireDecodeError &&
      error.code === WIRE_TYPE &&
      error.path === "$.tools[1]",
  );
});

test("root types and budget show have stable contracts", () => {
  const validators: Array<(value: unknown) => void> = [
    validateAi,
    validateTool,
    validateToolCatalog,
  ];
  for (const validator of validators) {
    assert.throws(
      () => validator(null),
      (error: unknown) =>
        error instanceof WireDecodeError &&
        error.code === WIRE_TYPE &&
        error.path === "$",
    );
  }
  assert.doesNotThrow(() =>
    validateBudgetShow({ app: "notes", period: "2026-08", units_used: 7 }),
  );
});
