import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { resolve } from "node:path";

import {
  type Operation,
  type Operationeffect,
  type FileChangePlan,
  WIRE_ENUM,
  WIRE_MAXIMUM,
  WIRE_MINIMUM,
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
  validateFileChangePlan,
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

interface FilePlanVectors {
  base: Record<string, unknown>;
  cases: Array<{
    name: string;
    root?: unknown;
    set?: Record<string, unknown>;
    code: string | null;
    path: string | null;
  }>;
}

function filePlanVectors(): FilePlanVectors {
  return JSON.parse(
    readFileSync(resolve(__dirname, "../../wire/v1/file_change_plan.vectors.json"), "utf8"),
  );
}

test("file change plan shared vectors and required fields", () => {
  const vectors = filePlanVectors();
  for (const entry of vectors.cases) {
    const value = Object.hasOwnProperty.call(entry, "root")
      ? entry.root
      : { ...vectors.base, ...entry.set };
    if (entry.code === null) {
      assert.doesNotThrow(() => validateFileChangePlan(value), entry.name);
    } else {
      assert.throws(
        () => validateFileChangePlan(value),
        (error: unknown) =>
          error instanceof WireDecodeError && error.code === entry.code && error.path === entry.path,
        entry.name,
      );
    }
  }
  for (const field of Object.keys(vectors.base)) {
    const value = { ...vectors.base };
    delete value[field];
    assert.throws(
      () => validateFileChangePlan(value),
      (error: unknown) =>
        error instanceof WireDecodeError && error.code === WIRE_REQUIRED && error.path === `$.${field}`,
    );
  }
});

test("file change plan generated type supports required and optional nullability", () => {
  const base = filePlanVectors().base;
  validateFileChangePlan(base);
  const nullable: FileChangePlan = {
    ...base, before_sha256: null, snapshot: null, applied_at: null, changed: null, diagnostic: null,
  };
  assert.doesNotThrow(() => validateFileChangePlan(nullable));
  assert.deepEqual(JSON.parse(JSON.stringify(nullable)), nullable);
  const applied: FileChangePlan = {
    ...base,
    state: "applied",
    before_exists: true,
    before_sha256: `sha256:${"c".repeat(64)}`,
    snapshot: "snapshot-1",
    applied_at: "2026-09-10T12:05:00Z",
    changed: false,
    diagnostic: "App-reported result",
  };
  assert.doesNotThrow(() => validateFileChangePlan(applied));
  assert.equal(JSON.parse(JSON.stringify(applied)).changed, false);
});

test("operation effect bindings preserve omission and explicit declarations", () => {
  const minimal: Operationeffect = {
    kind: "read",
    label: { en: "Read requested paths" },
  };
  const declared: Operationeffect = {
    kind: "update",
    label: { en: "Update requested paths" },
    target_arg: "paths",
    recovery: "compensatable",
  };
  const legacy: Operation = { label: { en: "Inspect" } };
  assert.equal(Object.hasOwnProperty.call(legacy, "effects"), false);
  assert.equal(Object.hasOwnProperty.call(minimal, "recovery"), false);
  for (const operation of [
    legacy,
    { label: { en: "Inspect" }, effects: [] },
    { label: { en: "Inspect" }, effects: [minimal] },
    { label: { en: "Update" }, effects: [declared] },
  ] satisfies Operation[]) {
    assert.deepEqual(JSON.parse(JSON.stringify(operation)), operation);
  }
});
