import { useSyncExternalStore } from "react";
import { api } from "@/lib/api";
import { record } from "@/lib/api-shapes";

export type WorkspaceState = "active" | "archived" | "deleted";
export type Draft = { name: string; description: string; context: string };
export type SelectedSource = { tag_id: string; path: string };
export type ReadTarget = { kind: "job" | "conversation"; id: string };
export type ReadLink = { id: string; source_workspace_id: string; target: ReadTarget; note_ids: string[];
  sources: SelectedSource[]; expires_at: string; created_at: string };
export type Workspace = Draft & { id: string; state: WorkspaceState; revision: string; cleanup_pending: boolean;
  access_revision: string;
  read_links: ReadLink[]; created_at: string; updated_at: string };
export type Note = { id: string; workspace_id: string; title: string; content: string; source: "owner_note" | "agent_report";
  source_job_id: string | null; revision: string; created_at: string; updated_at: string };
export type Tag = { id: string; workspace_id: string; path: string; kind: "file" | "directory"; created_at: string };
export type WorkspaceView = { schema: 1; workspace: Workspace; notes: Note[]; tags: Tag[]; activity_ids: string[] };
export type WorkspaceAction =
  | { action: "create"; draft: Draft }
  | { action: "update"; id: string; expected_revision: string; draft: Draft }
  | { action: "archive"; id: string; expected_revision: string; archived: boolean }
  | { action: "delete"; id: string; expected_revision: string; confirm: true }
  | { action: "tag"; id: string; expected_revision: string; path: string }
  | { action: "untag"; id: string; expected_revision: string; tag_id: string }
  | { action: "put_note"; id: string; expected_revision: string; note_id?: string; note: { title: string; content: string } }
  | { action: "forget_note"; id: string; expected_revision: string; note_id: string }
  | { action: "bind_activity" | "unbind_activity"; id: string; expected_revision: string; activity_id: string }
  | { action: "read_link"; id: string; expected_revision: string; source_workspace_id: string; target: ReadTarget;
      note_ids: string[]; sources: SelectedSource[]; expires_at: string }
  | { action: "revoke_link"; id: string; expected_revision: string; link_id: string };
export type SourcePage = { path: string; source_workspace_id: string; imported: boolean; source: string; revision: string;
  content: string; offset: number; next_offset: number | null; total_chars: number; source_complete: boolean };
export type NotePreview = { note: Note; imported: boolean; source_workspace_id: string; evidence: string };
export type SearchHit = { path: string; key: string; revision: string; snippet: string; score: number; model: string;
  indexed_at_ms: number; source_workspace_id: string; imported: boolean; source: string; read: { command: "source"; path: string; revision: string; offset: number } };
export type SearchResult = { query: string; hits: SearchHit[]; limit: number; model: string; score_kind: string;
  scope: string; coverage: string; source_errors: string };
export type IndexStatus = { schema: 1; embedding: { configured: boolean; model: string; dim?: number; detail?: string | null };
  corpus: { indexed_sources: number; indexed_chunks: number; state: string; freshness?: string };
  index_source_registration: string; store: string; formats: string };

function invalid(): never { throw new Error("Invalid Workspace response. Refresh before changing context or sharing."); }
function obj(value: unknown, required: string[], optional: string[] = []): Record<string, unknown> {
  if (!record(value) || !required.every((key) => Object.prototype.hasOwnProperty.call(value, key))
    || Object.keys(value).some((key) => !required.includes(key) && !optional.includes(key))) invalid();
  return value;
}
function text(value: unknown, max = 8192, empty = true): string {
  if (typeof value !== "string" || !empty && !value.trim() || new TextEncoder().encode(value).length > max
    || /[\u0000-\u0008\u000b\u000c\u000e-\u001f\u007f-\u009f]/.test(value)) invalid();
  return value;
}
export function workspaceId(value: unknown): string {
  const id = text(value, 36, false);
  if (!/^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/.test(id)) invalid();
  return id;
}
function revision(value: unknown): string {
  if (typeof value !== "string" || !/^[1-9][0-9]{0,18}$/.test(value) || BigInt(value) > 9223372036854775807n) invalid();
  return value;
}
function digest(value: unknown): string { const result = text(value, 64, false); if (!/^[a-f0-9]{64}$/.test(result)) invalid(); return result; }
function time(value: unknown): string { const result = text(value, 64, false); if (!Number.isFinite(Date.parse(result))) invalid(); return result; }
function bool(value: unknown): boolean { if (typeof value !== "boolean") invalid(); return value; }
function number(value: unknown, max = Number.MAX_SAFE_INTEGER): number {
  if (typeof value !== "number" || !Number.isSafeInteger(value) || value < 0 || value > max) invalid(); return value;
}
function choice<const T extends readonly string[]>(value: unknown, choices: T): T[number] {
  const selected = choices.find((choice) => choice === value); if (selected === undefined) invalid(); return selected;
}
function list<T>(value: unknown, max: number, read: (value: unknown) => T): T[] {
  if (!Array.isArray(value) || value.length > max) invalid(); return value.map(read);
}
function unique<T extends { id: string }>(values: T[]): T[] {
  if (new Set(values.map((value) => value.id)).size !== values.length) invalid(); return values;
}
function path(value: unknown): string {
  const result = text(value, 4096, false);
  if (!result.startsWith("/") || result === "/" || result.split("/").some((part) => part === "." || part === "..")) invalid();
  return result;
}
function readLink(value: unknown): ReadLink {
  const row = obj(value, ["id", "source_workspace_id", "target", "note_ids", "sources", "expires_at", "created_at"]);
  const target = obj(row.target, ["kind", "id"]);
  const kind = choice(target.kind, ["job", "conversation"] as const);
  const note_ids = list(row.note_ids, 16, workspaceId);
  const sources = list(row.sources, 16, (value) => { const source = obj(value, ["tag_id", "path"]);
    return { tag_id: workspaceId(source.tag_id), path: path(source.path) }; });
  if (!note_ids.length && !sources.length || note_ids.length + sources.length > 16
    || new Set(note_ids).size !== note_ids.length || new Set(sources.map((source) => source.path)).size !== sources.length) invalid();
  const targetId = text(target.id, 128, false);
  if (kind === "job") workspaceId(targetId);
  else if (!/^ses_[A-Za-z0-9_-]+$/.test(targetId)) invalid();
  return { id: workspaceId(row.id), source_workspace_id: workspaceId(row.source_workspace_id),
    target: { kind, id: targetId }, note_ids, sources, expires_at: time(row.expires_at), created_at: time(row.created_at) };
}
function readWorkspace(value: unknown): Workspace {
  const row = obj(value, ["id", "name", "description", "context", "state", "revision", "cleanup_pending", "created_at", "updated_at"], ["read_links", "access_revision"]);
  const result: Workspace = { id: workspaceId(row.id), name: text(row.name, 240, false),
    description: text(row.description), context: text(row.context), state: choice(row.state, ["active", "archived", "deleted"] as const),
    revision: revision(row.revision), access_revision: row.access_revision === undefined ? "1" : revision(row.access_revision), cleanup_pending: bool(row.cleanup_pending), created_at: time(row.created_at),
    updated_at: time(row.updated_at), read_links: row.read_links === undefined ? [] : unique(list(row.read_links, 16, readLink)) };
  if (result.cleanup_pending && result.state !== "deleted" || result.read_links.some((link) => link.source_workspace_id === result.id)) invalid();
  return result;
}
function readNote(value: unknown): Note {
  const row = obj(value, ["id", "workspace_id", "title", "content", "source", "source_job_id", "revision", "created_at", "updated_at"]);
  const result: Note = { id: workspaceId(row.id), workspace_id: workspaceId(row.workspace_id), title: text(row.title, 240, false),
    content: text(row.content), source: choice(row.source, ["owner_note", "agent_report"] as const),
    source_job_id: row.source_job_id === null ? null : workspaceId(row.source_job_id), revision: revision(row.revision),
    created_at: time(row.created_at), updated_at: time(row.updated_at) };
  if ((result.source === "owner_note") !== (result.source_job_id === null)) invalid();
  return result;
}
function readTag(value: unknown): Tag {
  const row = obj(value, ["id", "workspace_id", "path", "kind", "created_at"]);
  return { id: workspaceId(row.id), workspace_id: workspaceId(row.workspace_id), path: path(row.path),
    kind: choice(row.kind, ["file", "directory"] as const), created_at: time(row.created_at) };
}
export function readWorkspaceView(value: unknown): WorkspaceView {
  if (new TextEncoder().encode(JSON.stringify(value)).length > 2 * 1024 * 1024) invalid();
  const row = obj(value, ["schema", "workspace", "notes", "tags", "activity_ids"]);
  if (row.schema !== 1) invalid();
  const result: WorkspaceView = { schema: 1, workspace: readWorkspace(row.workspace),
    notes: unique(list(row.notes, 64, readNote)), tags: unique(list(row.tags, 128, readTag)),
    activity_ids: list(row.activity_ids, 1000, workspaceId) };
  if (result.notes.some((note) => note.workspace_id !== result.workspace.id) || result.tags.some((tag) => tag.workspace_id !== result.workspace.id)
    || new Set(result.activity_ids).size !== result.activity_ids.length) invalid();
  return result;
}
export function readWorkspaceList(value: unknown): Workspace[] {
  const row = obj(value, ["schema", "workspaces"]); if (row.schema !== 1) invalid();
  return unique(list(row.workspaces, 64, readWorkspace));
}
export function readSource(value: unknown): SourcePage {
  const row = obj(value, ["path", "source_workspace_id", "imported", "source", "revision", "content", "offset", "next_offset", "total_chars", "source_complete"]);
  const result: SourcePage = { path: path(row.path), source_workspace_id: workspaceId(row.source_workspace_id), imported: bool(row.imported),
    source: text(row.source, 128, false), revision: digest(row.revision), content: text(row.content, 65536),
    offset: number(row.offset, 50 * 1024 * 1024), next_offset: row.next_offset === null ? null : number(row.next_offset, 50 * 1024 * 1024),
    total_chars: number(row.total_chars, 50 * 1024 * 1024), source_complete: bool(row.source_complete) };
  if (result.offset > result.total_chars || result.next_offset !== null && (result.next_offset <= result.offset || result.next_offset >= result.total_chars)
    || result.source_complete && (result.offset !== 0 || result.next_offset !== null)) invalid();
  return result;
}

export function readNotePreview(value: unknown): NotePreview {
  const row = obj(value, ["note", "imported", "source_workspace_id", "evidence"]);
  const result = { note: readNote(row.note), imported: bool(row.imported),
    source_workspace_id: workspaceId(row.source_workspace_id), evidence: text(row.evidence, 128, false) };
  if (result.note.workspace_id !== result.source_workspace_id
    || result.evidence !== "owner_note_or_caller_report_not_current_fact") invalid();
  return result;
}
export function readSearch(value: unknown): SearchResult {
  const row = obj(value, ["query", "hits", "limit", "model", "score_kind", "scope", "coverage", "source_errors"]);
  const hits = list(row.hits, 50, (value) => {
    const hit = obj(value, ["path", "key", "revision", "snippet", "score", "model", "indexed_at_ms", "source_workspace_id", "imported", "source", "read"]);
    const read = obj(hit.read, ["command", "path", "revision", "offset"]);
    if (typeof hit.score !== "number" || !Number.isFinite(hit.score) || read.command !== "source") invalid();
    const result: SearchHit = { path: path(hit.path), key: text(hit.key, 128, false), revision: digest(hit.revision),
      snippet: text(hit.snippet, 2048), score: hit.score, model: text(hit.model, 128, false), indexed_at_ms: number(hit.indexed_at_ms),
      source_workspace_id: workspaceId(hit.source_workspace_id), imported: bool(hit.imported), source: text(hit.source, 128, false),
      read: { command: "source", path: path(read.path), revision: digest(read.revision), offset: number(read.offset) } };
    if (result.model !== "Qwen3-Embedding-0.6B" || result.read.path !== result.path || result.read.revision !== result.revision
      || !result.key.startsWith(`sha256:${result.revision}:`)) invalid();
    return result;
  });
  return { query: text(row.query, 2048, false), hits, limit: number(row.limit, 50), model: text(row.model, 128, false),
    score_kind: text(row.score_kind, 128), scope: text(row.scope, 128), coverage: text(row.coverage), source_errors: text(row.source_errors) };
}
export function readIndex(value: unknown): IndexStatus {
  const row = obj(value, ["schema", "embedding", "corpus", "index_source_registration", "store", "formats"]); if (row.schema !== 1) invalid();
  const embedding = obj(row.embedding, ["configured", "model"], ["dim", "detail"]);
  const corpus = obj(row.corpus, ["indexed_sources", "indexed_chunks", "state"], ["freshness"]);
  return { schema: 1, embedding: { configured: bool(embedding.configured), model: text(embedding.model, 128),
    ...(embedding.dim === undefined ? {} : { dim: number(embedding.dim, 1024) }),
    ...(embedding.detail === undefined ? {} : { detail: embedding.detail === null ? null : text(embedding.detail) }) },
    corpus: { indexed_sources: number(corpus.indexed_sources), indexed_chunks: number(corpus.indexed_chunks), state: text(corpus.state, 128),
      ...(corpus.freshness === undefined ? {} : { freshness: text(corpus.freshness) }) },
    index_source_registration: text(row.index_source_registration), store: text(row.store), formats: text(row.formats) };
}

export const workspaceApi = {
  list: async (signal?: AbortSignal) => readWorkspaceList(await api.get<unknown>("/api/workspaces", { signal })),
  get: async (id: string, signal?: AbortSignal) => {
    const view = readWorkspaceView(await api.get<unknown>(`/api/workspaces/${workspaceId(id)}`, { signal }));
    if (view.workspace.id !== id) invalid(); return view;
  },
  control: async (action: WorkspaceAction) => {
    const view = readWorkspaceView(await api.post<unknown>("/api/workspaces/control", { request: action }));
    if ("id" in action && (view.workspace.id !== action.id || BigInt(view.workspace.revision) <= BigInt(action.expected_revision))) invalid();
    if (action.action === "create" && view.workspace.revision !== "1") invalid();
    return view;
  },
  search: async (id: string, query: string, signal?: AbortSignal) => readSearch(await api.post<unknown>(`/api/workspaces/${workspaceId(id)}/read`, { command: "search", query, limit: 10 }, { signal })),
  source: async (id: string, source: string, offset = 0, revision?: string, signal?: AbortSignal) =>
    readSource(await api.post<unknown>(`/api/workspaces/${workspaceId(id)}/read`, { command: "source", path: source, offset, max_chars: 4096, ...(revision ? { revision } : {}) }, { signal })),
  previewSource: async (id: string, link: ReadLink, source: string, offset = 0, revision?: string, signal?: AbortSignal) => {
    const result = readSource(await api.post<unknown>(`/api/workspaces/${workspaceId(id)}/read`, {
      request: { command: "source", path: source, link_id: workspaceId(link.id), offset, max_chars: 4096, ...(revision ? { revision } : {}) },
      target: link.target,
    }, { signal }));
    if (result.path !== source || result.offset !== offset || result.source_workspace_id !== link.source_workspace_id
      || !result.imported || revision !== undefined && result.revision !== revision) invalid();
    return result;
  },
  previewNote: async (id: string, link: ReadLink, noteId: string, signal?: AbortSignal) => {
    const result = readNotePreview(await api.post<unknown>(`/api/workspaces/${workspaceId(id)}/read`, {
      request: { command: "note", note_id: workspaceId(noteId), link_id: workspaceId(link.id) }, target: link.target,
    }, { signal }));
    if (result.note.id !== noteId || result.source_workspace_id !== link.source_workspace_id || !result.imported) invalid();
    return result;
  },
  index: async (id: string, signal?: AbortSignal) => readIndex(await api.post<unknown>(`/api/workspaces/${workspaceId(id)}/read`, { command: "index" }, { signal })),
  indexSource: async (id: string, revision: string, tag: string) => {
    const row = obj(await api.post<unknown>(`/api/workspaces/${workspaceId(id)}/index-source`, { expected_revision: revision, tag_id: workspaceId(tag) }),
      ["configuration_saved", "directory", "coverage", "indexing"]);
    if (row.configuration_saved !== true) invalid(); return { directory: path(row.directory), coverage: text(row.coverage), indexing: text(row.indexing) };
  },
  conversation: async (workspace: string | null) => {
    const response = obj(await api.post<unknown>("/api/sessions", workspace ? { workspace_id: workspaceId(workspace) } : {}), ["session"]);
    if (!record(response.session) || typeof response.session.id !== "string" || !/^ses_[A-Za-z0-9_-]+$/.test(response.session.id)
      || (response.session.workspace_id ?? null) !== workspace) invalid();
    return response.session.id;
  },
};

let selected: string | null = null;
const listeners = new Set<() => void>();
export function selectWorkspace(id: string | null) { selected = id === null ? null : workspaceId(id); listeners.forEach((listener) => listener()); }
export function useWorkspaceSelection(): string | null {
  return useSyncExternalStore((listener) => { listeners.add(listener); return () => listeners.delete(listener); }, () => selected, () => null);
}
