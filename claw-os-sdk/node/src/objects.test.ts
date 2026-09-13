import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { resolve } from "node:path";

import { objects } from "./index";
import { type Manifest, type ObjectRef, WireDecodeError, validateObjectRef } from "./generated";

interface Vectors {
  valid: Array<{ reference: ObjectRef; uri: string }>;
  invalid_uris: string[];
  invalid_refs: ObjectRef[];
  limits: Array<{
    field: "app_id" | "object_type" | "object_id" | "revision";
    unit: string;
    count: number;
    bytes: number;
  }>;
  wire_cases: Array<{ value: unknown; code: string | null; path: string | null }>;
}

const vectors: Vectors = JSON.parse(
  readFileSync(resolve(__dirname, "../../wire/v1/object_ref.vectors.json"), "utf8"),
);

function reference(): ObjectRef {
  return { app_id: "notes", object_type: "note", object_id: "x" };
}

test("shared canonical object vectors round trip without identity changes", () => {
  for (const entry of vectors.valid) {
    const value = Object.freeze({ ...entry.reference });
    assert.equal(objects.format_reference(value), entry.uri);
    assert.deepEqual(objects.parse_reference(entry.uri), entry.reference);
    assert.equal(objects.formatReference(value), entry.uri);
    assert.deepEqual(objects.parseReference(entry.uri), entry.reference);
  }
});

test("shared invalid object vectors reject permissive URI normalization", () => {
  for (const uri of vectors.invalid_uris) {
    assert.throws(() => objects.parse_reference(uri), objects.ObjectRefError, uri);
  }
  for (const value of vectors.invalid_refs) {
    assert.throws(() => objects.format_reference(value), objects.ObjectRefError);
  }
});

test("shared byte limits accept the exact boundary and reject one more byte", () => {
  for (const limit of vectors.limits) {
    const value = reference();
    const text = limit.unit.repeat(limit.count);
    assert.equal(Buffer.byteLength(text, "utf8"), limit.bytes);
    value[limit.field] = text;
    assert.deepEqual(objects.parse_reference(objects.format_reference(value)), value);
    value[limit.field] = `${text}x`;
    assert.throws(() => objects.format_reference(value), objects.ObjectRefError, limit.field);
  }
});

test("maximum object components fit the URI ceiling", () => {
  const value: ObjectRef = {
    app_id: "a".repeat(128),
    object_type: "a".repeat(64),
    object_id: "\u00e9".repeat(512),
    revision: "\u00e9".repeat(64),
  };
  const uri = objects.format_reference(value);
  assert.equal(Buffer.byteLength(uri, "utf8"), 3669);
  assert.deepEqual(objects.parse_reference(uri), value);
  assert.throws(
    () => objects.parse_reference(`app://notes/note?id=${"x".repeat(4096)}`),
    objects.ObjectRefError,
  );
});

test("unpaired UTF-16 surrogates are never replacement encoded", () => {
  for (const text of ["\ud800", "\udfff", "x\ud800", "\ud800x"]) {
    for (const field of ["object_id", "revision"] as const) {
      const value = reference();
      value[field] = text;
      assert.throws(() => objects.format_reference(value), objects.ObjectRefError);
    }
    assert.throws(() => objects.parse_reference(`app://notes/note?id=${text}`), objects.ObjectRefError);
  }
});

test("generated ObjectRef decoder follows the shared closed contract", () => {
  for (const entry of vectors.wire_cases) {
    if (entry.code === null) {
      assert.doesNotThrow(() => validateObjectRef(entry.value));
    } else {
      assert.throws(
        () => validateObjectRef(entry.value),
        (error: unknown) =>
          error instanceof WireDecodeError && error.code === entry.code && error.path === entry.path,
      );
      assert.throws(
        () => Reflect.apply(objects.format_reference, undefined, [entry.value]),
        objects.ObjectRefError,
      );
    }
  }
});

test("legacy manifests and unrevisioned references remain compatible", () => {
  const manifest: Manifest = { id: "notes", version: "1.0.0", name: { en: "Notes" } };
  assert.equal(Object.hasOwnProperty.call(manifest, "objects"), false);
  const value = objects.parse_reference("app://notes/note?id=x");
  assert.equal(Object.hasOwnProperty.call(value, "revision"), false);
  assert.equal(Object.hasOwnProperty.call(value, "wire_version"), false);
});
