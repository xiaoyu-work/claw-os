import { record, oneOf } from "./api-shapes";

export const MEMORY_KINDS = ["notes", "apps", "conversations", "semantic"] as const;
export type MemoryKind = typeof MEMORY_KINDS[number];
export type MemoryReference =
  | { kind: "note"; name: string }
  | { kind: "app"; id: string }
  | { kind: "message"; id: string }
  | { kind: "semantic"; namespace: string; key: string };
export type MemoryEntry = {
  reference: MemoryReference; source: string; category: string;
  recorded_at_ms: number | null; revision: string; content: string;
  editable: boolean; deletable: boolean; provenance: string;
};
export type MemoryAccess = { source: string; revision: string; reference: MemoryReference };
export function readMemorySources(value: unknown): MemoryAccess[] {
  if (!Array.isArray(value) || value.length > 128) throw new Error("Memory source references exceed their presentation bound.");
  return value.map((source: unknown) => {
    if (!record(source) || typeof source.source !== "string" || source.source.length > 512 ||
        typeof source.revision !== "string" || !/^[a-fA-F0-9]{64}$/.test(source.revision)) {
      throw new Error("Memory source references are malformed.");
    }
    return { source: source.source, revision: source.revision, reference: memoryReference(source.reference) };
  });
}
export function memoryReference(value: unknown): MemoryReference {
  if (!record(value)) throw new Error("Invalid memory reference.");
  if (value.kind === "note" && typeof value.name === "string") return { kind: "note", name: value.name };
  if ((value.kind === "app" || value.kind === "message") && typeof value.id === "string" && /^[1-9][0-9]*$/.test(value.id)) {
    return { kind: value.kind, id: value.id };
  }
  if (value.kind === "semantic" && typeof value.namespace === "string" && typeof value.key === "string") {
    return { kind: "semantic", namespace: value.namespace, key: value.key };
  }
  throw new Error("Invalid memory source reference.");
}
export function readMemoryEntry(value: unknown): MemoryEntry {
  if (!record(value) || typeof value.source !== "string" || typeof value.category !== "string" ||
      typeof value.content !== "string" || typeof value.provenance !== "string" ||
      typeof value.revision !== "string" || !/^[a-f0-9]{64}$/.test(value.revision) ||
      typeof value.editable !== "boolean" || typeof value.deletable !== "boolean" ||
      !(value.recorded_at_ms === null || (typeof value.recorded_at_ms === "number" && Number.isSafeInteger(value.recorded_at_ms)))) {
    throw new Error("The owner memory entry is malformed.");
  }
  return {
    reference: memoryReference(value.reference), source: value.source, category: value.category,
    content: value.content, provenance: value.provenance, revision: value.revision,
    recorded_at_ms: value.recorded_at_ms, editable: value.editable, deletable: value.deletable,
  };
}
export function readMemoryList(value: unknown) {
  if (!record(value) || !Array.isArray(value.entries) || typeof value.has_more !== "boolean" || typeof value.coverage !== "string") {
    throw new Error("The owner memory inventory is malformed.");
  }
  return { entries: value.entries.map(readMemoryEntry), has_more: value.has_more, coverage: value.coverage };
}
