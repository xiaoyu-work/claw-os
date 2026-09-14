import { api } from "@/lib/api";
import { record } from "@/lib/api-shapes";
import type { Activity, ActivityResource } from "@/lib/activities";

export const MAX_CONTINUITY_DOCUMENT_BYTES = 192 * 1024;
const MAX_JSON_DEPTH = 8;
const MAX_JSON_CONTAINERS = 128;
const MAX_RAW_JSON_STRING_BYTES = 128 * 1024;
const MAX_I64 = 9_223_372_036_854_775_807n;
const encoder = new TextEncoder();

export type ContinuityExecutionLimits = {
  enabled: boolean;
  max_attempts: number;
  max_turns_per_attempt: number;
  expires_at: string;
};
export type ContinuityDocument = {
  kind: "claw_os.activity_continuity";
  schema_version: 1;
  lineage: { id: string; revision: string };
  snapshot: string;
  intent: {
    title: string;
    goal: string;
    completion_criteria: string;
    boundaries: string;
  };
  references: ActivityResource[];
  rules: {
    execution_limits: ContinuityExecutionLimits | null;
    scheduling: { priority: "foreground" | "standard" | "background" } | null;
  };
};
export type ContinuityImportAcknowledgement = {
  activity: Activity;
  continuity_id: string;
  continuity_revision: string;
  placement: "local";
};

class RawNumber {
  constructor(readonly value: string) {}
}

type ParsedJson = null | boolean | string | RawNumber | ParsedJson[]
  | { [key: string]: ParsedJson };

class ExactJsonParser {
  private index = 0;
  private containers = 0;

  constructor(private readonly source: string) {}

  parse(): ParsedJson {
    this.space();
    const value = this.value(0);
    this.space();
    if (this.index !== this.source.length) this.fail("contains trailing data");
    return value;
  }

  private value(depth: number): ParsedJson {
    this.space();
    const character = this.source[this.index];
    if (character === "{") return this.object(depth + 1);
    if (character === "[") return this.array(depth + 1);
    if (character === '"') return this.string();
    if (character === "t") return this.literal("true", true);
    if (character === "f") return this.literal("false", false);
    if (character === "n") return this.literal("null", null);
    return this.number();
  }

  private container(depth: number) {
    this.containers += 1;
    if (depth > MAX_JSON_DEPTH) this.fail("is too deeply nested");
    if (this.containers > MAX_JSON_CONTAINERS) this.fail("has too many containers");
  }

  private object(depth: number): { [key: string]: ParsedJson } {
    this.container(depth);
    this.index += 1;
    const result: { [key: string]: ParsedJson } = Object.create(null);
    const keys = new Set<string>();
    this.space();
    if (this.source[this.index] === "}") {
      this.index += 1;
      return result;
    }
    while (true) {
      if (this.source[this.index] !== '"') this.fail("has an object key that is not a string");
      const key = this.string();
      if (keys.has(key)) throw new Error(`Duplicate Activity continuity JSON key: ${key}`);
      keys.add(key);
      this.space();
      if (this.source[this.index++] !== ":") this.fail("is missing ':' after an object key");
      result[key] = this.value(depth);
      this.space();
      const separator = this.source[this.index++];
      if (separator === "}") return result;
      if (separator !== ",") this.fail("is missing ',' between object fields");
      this.space();
    }
  }

  private array(depth: number): ParsedJson[] {
    this.container(depth);
    this.index += 1;
    const result: ParsedJson[] = [];
    this.space();
    if (this.source[this.index] === "]") {
      this.index += 1;
      return result;
    }
    while (true) {
      result.push(this.value(depth));
      this.space();
      const separator = this.source[this.index++];
      if (separator === "]") return result;
      if (separator !== ",") this.fail("is missing ',' between array items");
      this.space();
    }
  }

  private string(): string {
    const start = this.index++;
    let escaped = false;
    while (this.index < this.source.length) {
      const character = this.source[this.index++];
      if (!escaped && character === '"') {
        const token = this.source.slice(start, this.index);
        if (encoder.encode(token.slice(1, -1)).byteLength > MAX_RAW_JSON_STRING_BYTES) {
          throw new Error("Activity continuity JSON string exceeds its bound.");
        }
        try {
          return JSON.parse(token) as string;
        } catch {
          this.fail("contains an invalid JSON string");
        }
      }
      if (!escaped && character === "\\") escaped = true;
      else escaped = false;
      if (!escaped && character.charCodeAt(0) < 0x20) this.fail("contains a raw control character");
    }
    this.fail("contains an incomplete JSON string");
  }

  private literal<T extends boolean | null>(text: string, value: T): T {
    if (this.source.slice(this.index, this.index + text.length) !== text) {
      this.fail("contains an invalid JSON literal");
    }
    this.index += text.length;
    return value;
  }

  private number(): RawNumber {
    const rest = this.source.slice(this.index);
    const match = /^-?(?:0|[1-9][0-9]*)(?:\.[0-9]+)?(?:[eE][+-]?[0-9]+)?/.exec(rest);
    if (!match) this.fail("contains an invalid JSON value");
    this.index += match[0].length;
    return new RawNumber(match[0]);
  }

  private space() {
    while (/[\t\n\r ]/.test(this.source[this.index] ?? "")) this.index += 1;
  }

  private fail(message: string): never {
    throw new Error(`Activity continuity JSON ${message}.`);
  }
}

const exact = (value: Record<string, unknown>, keys: string[]) =>
  Object.keys(value).length === keys.length
  && keys.every((key) => Object.prototype.hasOwnProperty.call(value, key));

function utf8(value: string): number {
  return encoder.encode(value).byteLength;
}

function text(
  value: unknown,
  field: string,
  maximum: number,
  allowEmpty: boolean,
  multiline: boolean,
): string {
  if (typeof value !== "string"
    || (!allowEmpty && value.trim().length === 0)
    || utf8(value.trim()) > maximum
    || value.trim() !== value
    || [...value].some((character) => {
      const code = character.codePointAt(0)!;
      const control = code <= 0x1f || (code >= 0x7f && code <= 0x9f);
      return control && !(multiline && ["\n", "\r", "\t"].includes(character));
    })) {
    throw new Error(`Activity continuity ${field} violates its canonical string bound.`);
  }
  return value;
}

function integer(value: unknown, field: string, minimum: number, maximum: number): number {
  const raw = value instanceof RawNumber ? value.value
    : typeof value === "number" && Number.isSafeInteger(value) ? String(value) : "";
  if (!/^(?:0|[1-9][0-9]*)$/.test(raw)) {
    throw new Error(`Activity continuity ${field} must be an exact integer.`);
  }
  const parsed = Number(raw);
  if (!Number.isSafeInteger(parsed) || parsed < minimum || parsed > maximum) {
    throw new Error(`Activity continuity ${field} is outside its bound.`);
  }
  return parsed;
}

function revision(value: unknown): string {
  const raw = value instanceof RawNumber ? value.value : typeof value === "string" ? value : "";
  if (!/^[1-9][0-9]*$/.test(raw) || BigInt(raw) > MAX_I64) {
    throw new Error("Activity continuity lineage revision is outside its bound.");
  }
  return raw;
}

function object(value: unknown, fields: string[], label: string): Record<string, unknown> {
  if (!record(value) || !exact(value, fields)) {
    throw new Error(`Activity continuity ${label} has missing or unknown fields.`);
  }
  return value;
}

function canonicalUuid(value: unknown, label: string): string {
  if (typeof value !== "string"
    || !/^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/.test(value)) {
    throw new Error(`Activity continuity ${label} must be a canonical UUID.`);
  }
  return value;
}

function canonicalReference(value: unknown): string {
  if (typeof value !== "string" || utf8(value) > 4096 || !value.startsWith("app://")) {
    throw new Error("Activity continuity reference must be a bounded canonical app:// URI.");
  }
  const match = /^app:\/\/([a-z][a-z0-9_-]*)\/([a-z][a-z0-9_-]*)\?(.+)$/.exec(value);
  if (!match || match[1].length > 128 || match[2].length > 64) {
    throw new Error("Activity continuity reference has an invalid App or object type.");
  }
  const parameters = match[3].split("&");
  if (parameters.length < 1 || parameters.length > 2
    || !parameters[0].startsWith("id=")
    || (parameters.length === 2 && !parameters[1].startsWith("revision="))) {
    throw new Error("Activity continuity reference query is not canonical.");
  }
  const decode = (encoded: string, field: string, maximum: number) => {
    if (!/^(?:[A-Za-z0-9._~-]|%[A-F0-9]{2})*$/.test(encoded)) {
      throw new Error(`Activity continuity reference ${field} is not canonically encoded.`);
    }
    let decoded: string;
    try {
      decoded = decodeURIComponent(encoded);
    } catch {
      throw new Error(`Activity continuity reference ${field} is not valid UTF-8.`);
    }
    if (!decoded || utf8(decoded) > maximum
      || [...decoded].some((character) => {
        const code = character.codePointAt(0)!;
        return code <= 0x1f || (code >= 0x7f && code <= 0x9f);
      })) {
      throw new Error(`Activity continuity reference ${field} violates its bound.`);
    }
    return decoded;
  };
  const encode = (decoded: string) => encodeURIComponent(decoded)
    .replace(/[!'()*]/g, (character) =>
      `%${character.charCodeAt(0).toString(16).toUpperCase()}`);
  const id = decode(parameters[0].slice(3), "id", 1024);
  const revisionValue = parameters.length === 2
    ? decode(parameters[1].slice("revision=".length), "revision", 128) : null;
  const canonical = `app://${match[1]}/${match[2]}?id=${encode(id)}${
    revisionValue === null ? "" : `&revision=${encode(revisionValue)}`}`;
  if (canonical !== value) {
    throw new Error("Activity continuity reference is not canonically spelled.");
  }
  return value;
}

function readShape(value: unknown): ContinuityDocument {
  const root = object(value, [
    "kind", "schema_version", "lineage", "snapshot", "intent", "references", "rules",
  ], "document");
  if (root.kind !== "claw_os.activity_continuity") {
    throw new Error("Activity continuity kind must be claw_os.activity_continuity.");
  }
  if (integer(root.schema_version, "schema version", 0, 0xffff_ffff) !== 1) {
    throw new Error("Unsupported Activity continuity schema version; supported version is 1.");
  }
  const lineage = object(root.lineage, ["id", "revision"], "lineage");
  const intent = object(root.intent, [
    "title", "goal", "completion_criteria", "boundaries",
  ], "intent");
  const rules = object(root.rules, ["execution_limits", "scheduling"], "rules");
  if (!Array.isArray(root.references) || root.references.length > 32) {
    throw new Error("Activity continuity references exceed the maximum of 32.");
  }
  const references = root.references.map((entry) => {
    const reference = object(entry, ["label", "reference"], "reference");
    return {
      label: text(reference.label, "reference label", 240, false, false),
      reference: canonicalReference(reference.reference),
    };
  });
  let executionLimits: ContinuityExecutionLimits | null = null;
  if (rules.execution_limits !== null) {
    const limits = object(rules.execution_limits, [
      "enabled", "max_attempts", "max_turns_per_attempt", "expires_at",
    ], "execution limits");
    if (typeof limits.enabled !== "boolean"
      || typeof limits.expires_at !== "string"
      || !/^[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}\.[0-9]{9}Z$/
        .test(limits.expires_at)
      || Number.isNaN(Date.parse(limits.expires_at))) {
      throw new Error("Activity continuity execution limits are not canonical.");
    }
    executionLimits = {
      enabled: limits.enabled,
      max_attempts: integer(limits.max_attempts, "max attempts", 1, 1000),
      max_turns_per_attempt: integer(limits.max_turns_per_attempt, "turn limit", 1, 100),
      expires_at: limits.expires_at,
    };
  }
  let scheduling: ContinuityDocument["rules"]["scheduling"] = null;
  if (rules.scheduling !== null) {
    const preference = object(rules.scheduling, ["priority"], "scheduling preference");
    if (preference.priority !== "foreground"
      && preference.priority !== "standard"
      && preference.priority !== "background") {
      throw new Error("Activity continuity scheduling priority is invalid.");
    }
    scheduling = { priority: preference.priority };
  }
  if (typeof root.snapshot !== "string" || !/^sha256:[0-9a-f]{64}$/.test(root.snapshot)) {
    throw new Error("Activity continuity snapshot digest is invalid.");
  }
  return {
    kind: "claw_os.activity_continuity",
    schema_version: 1,
    lineage: {
      id: canonicalUuid(lineage.id, "lineage id"),
      revision: revision(lineage.revision),
    },
    snapshot: root.snapshot,
    intent: {
      title: text(intent.title, "title", 240, false, false),
      goal: text(intent.goal, "goal", 16 * 1024, false, true),
      completion_criteria: text(
        intent.completion_criteria, "completion criteria", 8 * 1024, true, true,
      ),
      boundaries: text(intent.boundaries, "boundaries", 8 * 1024, true, true),
    },
    references,
    rules: { execution_limits: executionLimits, scheduling },
  };
}

function jsonString(value: string): string {
  return JSON.stringify(value);
}

function rulesJson(document: ContinuityDocument): string {
  const limits = document.rules.execution_limits;
  const scheduling = document.rules.scheduling;
  return `{"execution_limits":${limits === null ? "null" : `{"enabled":${limits.enabled},`
    + `"max_attempts":${limits.max_attempts},"max_turns_per_attempt":${limits.max_turns_per_attempt},`
    + `"expires_at":${jsonString(limits.expires_at)}}`},`
    + `"scheduling":${scheduling === null ? "null"
      : `{"priority":${jsonString(scheduling.priority)}}`}}`;
}

function snapshotMaterial(document: ContinuityDocument): string {
  return `{"kind":${jsonString(document.kind)},"schema_version":1,`
    + `"lineage":{"id":${jsonString(document.lineage.id)},"revision":${document.lineage.revision}},`
    + `"intent":{"title":${jsonString(document.intent.title)},"goal":${jsonString(document.intent.goal)},`
    + `"completion_criteria":${jsonString(document.intent.completion_criteria)},`
    + `"boundaries":${jsonString(document.intent.boundaries)}},`
    + `"references":[${document.references.map((reference) =>
      `{"label":${jsonString(reference.label)},"reference":${jsonString(reference.reference)}}`,
    ).join(",")}],"rules":${rulesJson(document)}}`;
}

async function digest(value: string): Promise<string> {
  const bytes = await globalThis.crypto.subtle.digest("SHA-256", encoder.encode(value));
  return `sha256:${Array.from(new Uint8Array(bytes))
    .map((byte) => byte.toString(16).padStart(2, "0")).join("")}`;
}

export async function validateContinuityDocument(value: unknown): Promise<ContinuityDocument> {
  const document = readShape(value);
  if (await digest(snapshotMaterial(document)) !== document.snapshot) {
    throw new Error("Activity continuity snapshot digest does not match the document.");
  }
  if (encoder.encode(continuityJson(document)).byteLength > MAX_CONTINUITY_DOCUMENT_BYTES) {
    throw new Error(`Activity continuity document exceeds ${MAX_CONTINUITY_DOCUMENT_BYTES} bytes.`);
  }
  return document;
}

export async function parseContinuityFile(source: string): Promise<ContinuityDocument> {
  const bytes = encoder.encode(source).byteLength;
  if (bytes === 0) throw new Error("Activity continuity document is empty.");
  if (bytes > MAX_CONTINUITY_DOCUMENT_BYTES) {
    throw new Error(`Activity continuity document exceeds ${MAX_CONTINUITY_DOCUMENT_BYTES} bytes.`);
  }
  return validateContinuityDocument(new ExactJsonParser(source).parse());
}

export function continuityJson(document: ContinuityDocument): string {
  return `{"kind":${jsonString(document.kind)},"schema_version":1,`
    + `"lineage":{"id":${jsonString(document.lineage.id)},"revision":${document.lineage.revision}},`
    + `"snapshot":${jsonString(document.snapshot)},"intent":{"title":${jsonString(document.intent.title)},`
    + `"goal":${jsonString(document.intent.goal)},"completion_criteria":${jsonString(document.intent.completion_criteria)},`
    + `"boundaries":${jsonString(document.intent.boundaries)}},"references":[${document.references.map((reference) =>
      `{"label":${jsonString(reference.label)},"reference":${jsonString(reference.reference)}}`,
    ).join(",")}],"rules":${rulesJson(document)}}`;
}

export function continuityFilename(document: ContinuityDocument): string {
  return `claw-os-activity-${document.lineage.id}.json`;
}

function importedActivity(value: unknown, document: ContinuityDocument): Activity {
  const fields = [
    "id", "owner_uid", "title", "goal", "completion_criteria", "boundaries", "resources",
    "state", "completion_note", "created_at", "updated_at",
  ];
  const activity = object(value, fields, "imported Activity");
  if (!Number.isSafeInteger(activity.owner_uid) || (activity.owner_uid as number) < 0
    || (activity.owner_uid as number) > 0xffff_ffff
    || activity.state !== "paused" || activity.completion_note !== null
    || activity.title !== document.intent.title || activity.goal !== document.intent.goal
    || activity.completion_criteria !== document.intent.completion_criteria
    || activity.boundaries !== document.intent.boundaries
    || !Array.isArray(activity.resources)
    || JSON.stringify(activity.resources) !== JSON.stringify(document.references)
    || typeof activity.created_at !== "string" || Number.isNaN(Date.parse(activity.created_at))
    || typeof activity.updated_at !== "string" || Number.isNaN(Date.parse(activity.updated_at))) {
    throw new Error("Activity continuity import did not create the exact paused Activity.");
  }
  canonicalUuid(activity.id, "imported Activity id");
  return activity as Activity;
}

export async function readContinuityImport(
  value: unknown,
  document: ContinuityDocument,
  existingIds: ReadonlySet<string>,
): Promise<ContinuityImportAcknowledgement> {
  const acknowledgement = object(value, [
    "activity", "continuity_id", "continuity_revision", "placement",
  ], "import acknowledgement");
  if (acknowledgement.continuity_id !== document.lineage.id
    || acknowledgement.continuity_revision !== document.lineage.revision
    || acknowledgement.placement !== "local") {
    throw new Error("Activity continuity import acknowledgement did not match the lineage or local placement.");
  }
  const activity = importedActivity(acknowledgement.activity, document);
  if (existingIds.has(activity.id)) {
    throw new Error("Activity continuity import acknowledgement did not identify a new Activity.");
  }
  return {
    activity,
    continuity_id: document.lineage.id,
    continuity_revision: document.lineage.revision,
    placement: "local",
  };
}

export const continuityApi = {
  export: async (id: string, signal?: AbortSignal) =>
    validateContinuityDocument(await api.get<unknown>(
      `/api/activities/${encodeURIComponent(id)}/continuity/export`,
      { signal },
    )),
  import: async (
    document: ContinuityDocument,
    existingIds: ReadonlySet<string>,
    signal?: AbortSignal,
  ) => readContinuityImport(await api.post<unknown>(
    "/api/activities/continuity/import",
    { placement: "local", document },
    { signal },
  ), document, existingIds),
};
