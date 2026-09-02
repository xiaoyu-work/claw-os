import { test } from "node:test";
import assert from "node:assert/strict";

import {
  WIRE_ENUM,
  WIRE_MAX_LENGTH,
  WIRE_MAXIMUM,
  WIRE_MIN_LENGTH,
  WIRE_MINIMUM,
  WIRE_PATTERN,
  WIRE_REQUIRED,
  WIRE_TYPE,
  WIRE_UNKNOWN_FIELD,
  WireDecodeError,
  WireDecimal,
  WireJsonSerializationError,
  decodeWireJson,
  stringifyWireJson,
  validateAi,
  validateBudgetShow,
  validateMcpCallContext,
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
  const accepted = new Map<string, number | bigint>([
    ["1.0", 1],
    ["1e0", 1],
    ["1.5e1", 15],
    ["9007199254740992", 9007199254740992n],
    ["18446744073709551615", 18446744073709551615n],
  ]);
  for (const [literal, expected] of accepted) {
    const payload = decodeWireJson(
      JSON.stringify(validAi()).replace('"units":3', `"units":${literal}`),
    ) as Record<string, unknown>;
    assert.doesNotThrow(() => validateAi(payload));
    assert.equal((payload.usage as Record<string, unknown>).units, expected);
  }

  for (const literal of ["1.5", "15e-1", "1e-400", "9007199254740990.5"]) {
    const payload = decodeWireJson(
      JSON.stringify(validAi()).replace('"units":3', `"units":${literal}`),
    );
    assert.throws(
      () => validateAi(payload),
      (error: unknown) =>
        error instanceof WireDecodeError &&
        error.code === WIRE_TYPE &&
        error.path === "$.usage.units",
    );
  }

  const oversized = decodeWireJson(
    JSON.stringify(validAi()).replace('"units":3', '"units":18446744073709551616'),
  );
  assert.throws(
    () => validateAi(oversized),
    (error: unknown) => error instanceof WireDecodeError && error.code === WIRE_MAXIMUM,
  );

  const fractionalAbove = decodeWireJson(
    JSON.stringify(validAi()).replace('"units":3', '"units":18446744073709551615.5'),
  );
  assert.throws(
    () => validateAi(fractionalAbove),
    (error: unknown) => error instanceof WireDecodeError && error.code === WIRE_TYPE,
  );
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
    validateMcpCallContext,
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

test("MCP call context is closed and depth bounded", () => {
  const context = {
    wire_version: 1,
    call_id: "call-1",
    trace_id: "trace-1",
    depth: 0,
    caller: {
      kind: "system-agent",
      id: "session-1",
      owner_uid: 1000,
    },
  };
  assert.doesNotThrow(() => validateMcpCallContext(context));
  assert.throws(
    () =>
      validateMcpCallContext({
        ...context,
        caller: { ...context.caller, token: "forged" },
      }),
    (error: unknown) =>
      error instanceof WireDecodeError &&
      error.code === WIRE_UNKNOWN_FIELD &&
      error.path === "$.caller.token",
  );
  assert.throws(
    () => validateMcpCallContext({ ...context, depth: 17 }),
    (error: unknown) =>
      error instanceof WireDecodeError &&
      error.code === WIRE_MAXIMUM &&
      error.path === "$.depth",
  );
  for (const [callId, code] of [
    ["", WIRE_MIN_LENGTH],
    ["x".repeat(129), WIRE_MAX_LENGTH],
    ["call id", WIRE_PATTERN],
    ["call-1\n", WIRE_PATTERN],
  ]) {
    assert.throws(
      () => validateMcpCallContext({ ...context, call_id: callId }),
      (error: unknown) =>
        error instanceof WireDecodeError &&
        error.code === code &&
        error.path === "$.call_id",
    );
  }
});

test("compact huge exponents remain compact public wrappers", () => {
  const lexeme = "1e1000000000";
  const value = decodeWireJson(lexeme);
  assert.ok(value instanceof WireDecimal);
  assert.equal(value.lexeme, lexeme);
  assert.equal(stringifyWireJson(value), lexeme);
});

test("serializer rejects non-finite and unsafe native numbers recursively", () => {
  for (const [value, code] of [
    [Number.NaN, "WIRE_JSON_NON_FINITE"],
    [Number.POSITIVE_INFINITY, "WIRE_JSON_NON_FINITE"],
    [Number.NEGATIVE_INFINITY, "WIRE_JSON_NON_FINITE"],
    [Number.MAX_SAFE_INTEGER + 1, "WIRE_JSON_UNSAFE_INTEGER"],
    [{ nested: [Number.MAX_SAFE_INTEGER + 1] }, "WIRE_JSON_UNSAFE_INTEGER"],
  ] as Array<[unknown, string]>) {
    assert.throws(
      () => stringifyWireJson(value as never),
      (error: unknown) =>
        error instanceof WireJsonSerializationError && error.code === code,
    );
  }
});
