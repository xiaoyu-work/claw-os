import { describe, expect, test } from "bun:test";
import { readWorkspaceView, readSource, readSearch, readNotePreview, workspaceApi, type ReadLink } from "../src/lib/workspaces";

const workspace = "11111111-1111-4111-8111-111111111111";
const other = "22222222-2222-4222-8222-222222222222";
const timestamp = "2026-01-01T00:00:00Z";
function view() {
  return { schema: 1, workspace: { id: workspace, name: "Research", description: "", context: "Small shared context",
    state: "active", revision: "9007199254740993", access_revision: "1", cleanup_pending: false, created_at: timestamp, updated_at: timestamp },
    notes: [], tags: [], activity_ids: [] };
}
describe("Workspace shared wire", () => {
  test("preserves exact displayed revisions and optional old fields", () => {
    expect(readWorkspaceView(view()).workspace.revision).toBe("9007199254740993");
    expect(readWorkspaceView(view()).workspace.read_links).toEqual([]);
    expect(readWorkspaceView(view()).workspace.access_revision).toBe("1");
  });
  test("rejects authority fields, mixed owners/domains and unsafe lifecycle claims", () => {
    expect(() => readWorkspaceView({ ...view(), owner_uid: 1000 })).toThrow();
    expect(() => readWorkspaceView({ ...view(), workspace: { ...view().workspace, cleanup_pending: true } })).toThrow();
    expect(() => readWorkspaceView({ ...view(), tags: [{ id: other, workspace_id: other, path: "/home/user/file.txt", kind: "file", created_at: timestamp }] })).toThrow();
  });
  test("source windows stay exact and never pretend a suffix is complete", () => {
    const source = { path: "/home/user/file.txt", source_workspace_id: workspace, imported: false,
      source: "current_pinned_utf8_file", revision: "a".repeat(64), content: "last page",
      offset: 100, next_offset: null, total_chars: 109, source_complete: false };
    expect(readSource(source).source_complete).toBe(false);
    expect(() => readSource({ ...source, source_complete: true })).toThrow();
  });
  test("rechecked local results cannot relabel model or source identity", () => {
    const hit = { path: "/home/user/file.txt", key: `sha256:${"a".repeat(64)}:0`, revision: "a".repeat(64), snippet: "Relevant text",
      score: 0.6, model: "Qwen3-Embedding-0.6B", indexed_at_ms: 1, source_workspace_id: workspace, imported: false,
      source: "indexed_snapshot_rechecked_against_current_file", read: { command: "source", path: "/home/user/file.txt", revision: "a".repeat(64), offset: 0 } };
    const result = { query: "text", hits: [hit], limit: 10, model: hit.model, score_kind: "cosine_similarity_not_confidence",
      scope: "live_shared_sources_only", coverage: "UTF-8 files", source_errors: "Stale sources excluded" };
    expect(readSearch(result).hits).toHaveLength(1);
    expect(() => readSearch({ ...result, hits: [{ ...hit, model: "stub" }] })).toThrow();
    expect(() => readSearch({ ...result, hits: [{ ...hit, read: { ...hit.read, path: "/home/user/other.txt" } }] })).toThrow();
  });
  test("imported note preview keeps the original source and evidence", () => {
    const preview = { note: { id: workspace, workspace_id: other, title: "Audience", content: "Beginners",
      source: "owner_note", source_job_id: null, revision: "1", created_at: timestamp, updated_at: timestamp },
      imported: true, source_workspace_id: other, evidence: "owner_note_or_caller_report_not_current_fact" };
    expect(readNotePreview(preview).note.workspace_id).toBe(other);
    expect(() => readNotePreview({ ...preview, source_workspace_id: workspace })).toThrow();
    expect(() => readNotePreview({ ...preview, evidence: "owner_confirmed_current_fact" })).toThrow();
    expect(() => readNotePreview({ ...preview, owner_uid: 1001 })).toThrow();
  });
  test("temporary source pagination retains exact link, target and displayed revision", async () => {
    const link: ReadLink = { id: "33333333-3333-4333-8333-333333333333", source_workspace_id: other,
      target: { kind: "job", id: "44444444-4444-4444-8444-444444444444" }, note_ids: [],
      sources: [{ tag_id: workspace, path: "/home/user/file.txt" }], expires_at: timestamp, created_at: timestamp };
    const requests: unknown[] = [];
    const originalFetch = globalThis.fetch;
    globalThis.fetch = (async (_input, options) => {
      const body = JSON.parse(String(options?.body));
      requests.push(body);
      const offset = body.request.offset;
      return Response.json({ path: "/home/user/file.txt", source_workspace_id: other, imported: true,
        source: "current_pinned_utf8_file", revision: "a".repeat(64), content: offset ? "last!" : "first",
        offset, next_offset: offset ? null : 5, total_chars: 10, source_complete: false });
    }) as typeof fetch;
    try {
      const first = await workspaceApi.previewSource(workspace, link, "/home/user/file.txt");
      await workspaceApi.previewSource(workspace, link, first.path, first.next_offset ?? 0, first.revision);
      expect(requests).toEqual([
        { request: { command: "source", path: first.path, link_id: link.id, offset: 0, max_chars: 4096 }, target: link.target },
        { request: { command: "source", path: first.path, link_id: link.id, offset: 5, max_chars: 4096, revision: first.revision }, target: link.target },
      ]);
    } finally { globalThis.fetch = originalFetch; }
  });
});
