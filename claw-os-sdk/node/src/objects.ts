/** Pure App object identifiers, without discovery, storage, or dispatch. */
import { type ObjectRef, WireDecodeError, validateObjectRef } from "./generated";

export type { ObjectRef } from "./generated";

const COMPONENT = /^[a-z][a-z0-9_-]*$/u;
const QUERY_VALUE = /^(?:[A-Za-z0-9._~-]|%[A-Fa-f0-9]{2})*$/u;
const CONTROL = /\p{Cc}/u;
const MAX_URI_BYTES = 4096;

export class ObjectRefError extends Error {
  constructor(message: string) {
    super(`invalid App object reference: ${message}`);
    this.name = "ObjectRefError";
  }
}

function utf8Length(value: string): number {
  for (const character of value) {
    const unit = character.charCodeAt(0);
    if (character.length === 1 && unit >= 0xd800 && unit <= 0xdfff) {
      throw new ObjectRefError("reference text must be valid UTF-8");
    }
  }
  return Buffer.byteLength(value, "utf8");
}

function validate(value: unknown): asserts value is ObjectRef {
  try {
    validateObjectRef(value);
  } catch (error) {
    if (error instanceof WireDecodeError) throw new ObjectRefError(error.message);
    throw error;
  }
  for (const [field, limit] of [["app_id", 128], ["object_type", 64]] as const) {
    const text = value[field];
    if (text.length > limit || COMPONENT.exec(text)?.[0] !== text) {
      throw new ObjectRefError(
        `${field} must be a lowercase ASCII component of at most ${limit} bytes`,
      );
    }
  }
  for (const [field, limit] of [["object_id", 1024], ["revision", 128]] as const) {
    const text = value[field];
    if (text === undefined) continue;
    if (!text || text.length > limit || utf8Length(text) > limit || CONTROL.test(text)) {
      throw new ObjectRefError(
        `${field} must contain 1..=${limit} UTF-8 bytes without control characters`,
      );
    }
  }
}

function encode(value: string): string {
  return encodeURIComponent(value).replace(
    /[!'()*]/g,
    (character) => `%${character.charCodeAt(0).toString(16).toUpperCase()}`,
  );
}

export function format_reference(value: ObjectRef): string {
  validate(value);
  let uri = `app://${value.app_id}/${value.object_type}?id=${encode(value.object_id)}`;
  if (value.revision !== undefined) uri += `&revision=${encode(value.revision)}`;
  if (uri.length > MAX_URI_BYTES) throw new ObjectRefError("URI exceeds 4096 UTF-8 bytes");
  return uri;
}

function decode(value: string): string {
  if (QUERY_VALUE.exec(value)?.[0] !== value) {
    throw new ObjectRefError("query values must use RFC3986 percent encoding");
  }
  try {
    return decodeURIComponent(value);
  } catch (error) {
    if (error instanceof URIError) throw new ObjectRefError("query value is not valid UTF-8");
    throw error;
  }
}

export function parse_reference(value: string): ObjectRef {
  if (typeof value !== "string") throw new ObjectRefError("URI must be a string");
  if (value.length > MAX_URI_BYTES || utf8Length(value) > MAX_URI_BYTES) {
    throw new ObjectRefError("URI exceeds 4096 UTF-8 bytes");
  }
  if (!value.startsWith("app://")) {
    throw new ObjectRefError("URI must use the exact app:// scheme");
  }
  const separator = value.indexOf("?", 6);
  if (separator === -1) throw new ObjectRefError("URI requires an id query parameter");
  const address = value.slice(6, separator);
  const slash = address.indexOf("/");
  if (slash === -1) throw new ObjectRefError("URI requires one App and one object type");
  let objectId: string | undefined;
  let revision: string | undefined;
  for (const parameter of value.slice(separator + 1).split("&")) {
    const equals = parameter.indexOf("=");
    if (equals === -1) throw new ObjectRefError("invalid query parameter");
    const key = parameter.slice(0, equals);
    const encoded = parameter.slice(equals + 1);
    if (key === "id" && objectId === undefined) objectId = decode(encoded);
    else if (key === "revision" && revision === undefined) revision = decode(encoded);
    else throw new ObjectRefError("unknown or repeated query parameter");
  }
  if (objectId === undefined) throw new ObjectRefError("URI requires an id query parameter");
  const reference: ObjectRef = {
    app_id: address.slice(0, slash),
    object_type: address.slice(slash + 1),
    object_id: objectId,
  };
  if (revision !== undefined) reference.revision = revision;
  if (format_reference(reference) !== value) {
    throw new ObjectRefError("URI is not canonically spelled");
  }
  return reference;
}

export { format_reference as formatReference, parse_reference as parseReference };
