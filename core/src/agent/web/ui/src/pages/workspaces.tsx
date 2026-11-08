import { useCallback, useEffect, useRef, useState } from "react";
import { Button } from "@/components/ui/button";
import { Card } from "@/components/ui/card";
import { Input } from "@/components/ui/input";
import { Textarea } from "@/components/ui/textarea";
import { navigate } from "@/lib/router";
import { workspaceApi, selectWorkspace, useWorkspaceSelection, type Workspace, type WorkspaceView, type WorkspaceAction,
  type Draft, type SearchResult, type SourcePage, type IndexStatus, type ReadTarget, type SelectedSource, type ReadLink, type NotePreview } from "@/lib/workspaces";

const emptyDraft: Draft = { name: "", description: "", context: "" };
export function WorkspacesPage() {
  const selection = useWorkspaceSelection();
  const [records, setRecords] = useState<Workspace[]>([]);
  const [view, setView] = useState<WorkspaceView | null>(null);
  const [draft, setDraft] = useState<Draft>(emptyDraft);
  const [draftRevision, setDraftRevision] = useState("");
  const [createName, setCreateName] = useState("");
  const [tagPath, setTagPath] = useState("");
  const [noteTitle, setNoteTitle] = useState("");
  const [noteContent, setNoteContent] = useState("");
  const [activity, setActivity] = useState("");
  const [query, setQuery] = useState("");
  const [sourcePath, setSourcePath] = useState("");
  const [search, setSearch] = useState<SearchResult | null>(null);
  const [source, setSource] = useState<SourcePage | null>(null);
  const [sourcePreview, setSourcePreview] = useState<ReadLink | null>(null);
  const [notePreview, setNotePreview] = useState<NotePreview | null>(null);
  const [index, setIndex] = useState<IndexStatus | null>(null);
  const [origin, setOrigin] = useState<WorkspaceView | null>(null);
  const [originId, setOriginId] = useState("");
  const [noteIds, setNoteIds] = useState<string[]>([]);
  const [sourceTag, setSourceTag] = useState("");
  const [selectedPath, setSelectedPath] = useState("");
  const [selectedSources, setSelectedSources] = useState<SelectedSource[]>([]);
  const [targetKind, setTargetKind] = useState<ReadTarget["kind"]>("conversation");
  const [targetId, setTargetId] = useState("");
  const [expiry, setExpiry] = useState("60");
  const [deleteView, setDeleteView] = useState<WorkspaceView | null>(null);
  const [deleteConfirmed, setDeleteConfirmed] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const alive = useRef(true);
  const generation = useRef(0);
  const pending = useRef(false);
  const read = useRef<AbortController | null>(null);
  const dirty = useRef(false);

  const adopt = useCallback((next: WorkspaceView) => {
    setView(next);
    setSource(null); setSourcePreview(null); setNotePreview(null);
    setRecords((records) => {
      const retained = records.filter((record) => record.id !== next.workspace.id);
      return next.workspace.state === "deleted" ? retained : [...retained, next.workspace];
    });
    if (!dirty.current) { setDraft({ name: next.workspace.name, description: next.workspace.description, context: next.workspace.context }); setDraftRevision(next.workspace.revision); }
  }, []);
  const refresh = useCallback(async () => {
    if (pending.current) return;
    read.current?.abort();
    const abort = new AbortController(); read.current = abort;
    const serial = ++generation.current;
    try {
      const records = await workspaceApi.list(abort.signal);
      const view = selection ? await workspaceApi.get(selection, abort.signal) : null;
      if (!alive.current || serial !== generation.current || abort.signal.aborted) return;
      setRecords(records); if (view) adopt(view); else setView(null);
      setError(null);
    } catch (cause) {
      if (alive.current && serial === generation.current && !abort.signal.aborted) setError(message(cause));
    } finally { if (read.current === abort) read.current = null; }
  }, [selection, adopt]);
  useEffect(() => {
    alive.current = true; dirty.current = false; setView(null); setSearch(null); setSource(null); setIndex(null);
    setSourcePreview(null); setNotePreview(null);
    setOrigin(null); setNoteIds([]); setSelectedSources([]); setDeleteView(null); setError(null); setNotice(null);
    void refresh();
    return () => { alive.current = false; ++generation.current; read.current?.abort(); };
  }, [refresh]);

  async function perform(action: WorkspaceAction) {
    if (pending.current) return;
    pending.current = true; read.current?.abort(); const serial = ++generation.current;
    setBusy(true); setError(null); setNotice(null);
    try {
      const next = await workspaceApi.control(action);
      if (alive.current && serial === generation.current) {
        if (action.action === "create") { dirty.current = false; selectWorkspace(next.workspace.id); }
        if (action.action === "update") dirty.current = false;
        adopt(next); setSearch(null); setSource(null); setOrigin(null); setNoteIds([]); setSelectedSources([]);
        setSourcePreview(null); setNotePreview(null);
        window.dispatchEvent(new CustomEvent("cos:workspaces-changed"));
        setDeleteView(null); setDeleteConfirmed(false);
        setNotice(next.workspace.cleanup_pending ? "Sharing is deleted. Exact Job retirement is pending, not proven complete. Refresh and explicitly repeat cleanup."
          : "Workspace change acknowledged. Original files and conversation/execution evidence remain.");
      }
    } catch (cause) {
      if (alive.current && serial === generation.current) setError(`${message(cause)} Refresh before retrying. No mutation is replayed automatically.`);
    } finally { pending.current = false; if (alive.current) setBusy(false); }
  }
  async function inspect<T>(operation: (signal: AbortSignal) => Promise<T>, apply: (result: T) => void) {
    if (pending.current) return;
    read.current?.abort(); const abort = new AbortController(); read.current = abort;
    const serial = ++generation.current; setBusy(true); setError(null);
    try { const result = await operation(abort.signal); if (alive.current && serial === generation.current && !abort.signal.aborted) apply(result); }
    catch (cause) { if (alive.current && serial === generation.current && !abort.signal.aborted) setError(message(cause)); }
    finally { if (read.current === abort) read.current = null; if (alive.current && serial === generation.current) setBusy(false); }
  }
  const receiveSource = (page: SourcePage) => { setSource(page); setSourcePreview(null); };
  async function registerIndex(tag: string) {
    if (!view || pending.current) return;
    pending.current = true; const serial = ++generation.current; read.current?.abort(); setBusy(true); setError(null);
    try { const result = await workspaceApi.indexSource(view.workspace.id, view.workspace.revision, tag);
      if (alive.current && serial === generation.current) setNotice(`${result.coverage} ${result.indexing}`); }
    catch (cause) { if (alive.current && serial === generation.current) setError(`${message(cause)} Registration is not replayed.`); }
    finally { pending.current = false; if (alive.current) setBusy(false); }
  }
  async function newConversation() {
    if (!view || pending.current) return;
    pending.current = true; setBusy(true);
    try { const id = await workspaceApi.conversation(view.workspace.id); window.dispatchEvent(new CustomEvent("cos:sessions-changed")); navigate(`/chat/${id}`); }
    catch (cause) { if (alive.current) setError(`${message(cause)} Creation is not retried automatically.`); }
    finally { pending.current = false; if (alive.current) setBusy(false); }
  }
  const workspace = view?.workspace;
  const closed = workspace?.state !== "active";
  const disabled = busy || !!error || !view;
  const binding = workspace ? { id: workspace.id, expected_revision: workspace.revision } : null;
  const edit = (field: keyof Draft, value: string) => { dirty.current = true; setDraft((draft) => ({ ...draft, [field]: value })); };
  return <section aria-label="Workspaces" className="h-full overflow-y-auto p-4"><div className="mx-auto grid max-w-5xl gap-4">
    <div className="flex items-center justify-between"><h2 className="text-xl font-semibold">Workspaces</h2>
      <Button variant="outline" disabled={busy} onClick={() => void refresh()}>Refresh Workspaces</Button></div>
    <p className="text-sm text-muted-foreground">Long-lived shared context above Activities, not a coding worktree or cwd. Conversations remain independent. Tags share only reads; they grant no write, execution, delivery or App permissions.</p>
    {error && <p role="alert" className="text-destructive">{error}</p>}
    {notice && <p role="status" className="text-sm">{notice}</p>}
    <Card className="gap-3 p-4"><h3 className="font-medium">Select or create</h3>
      <select aria-label="Selected Workspace" className="rounded-md border bg-background p-2 text-sm" value={selection ?? ""} disabled={busy}
        onChange={(event) => selectWorkspace(event.target.value || null)}><option value="">No Workspace selected</option>
        {records.map((record) => <option key={record.id} value={record.id}>{record.name} · {record.state}</option>)}</select>
      <div className="flex gap-2"><Input aria-label="New Workspace name" value={createName} maxLength={240} disabled={busy} onChange={(event) => setCreateName(event.target.value)} />
        <Button disabled={busy || !createName.trim()} onClick={() => void perform({ action: "create", draft: { ...emptyDraft, name: createName } })}>Create Workspace</Button></div>
    </Card>
    {view && workspace && binding && <>
      <Card className="gap-3 p-4"><h3 className="font-medium">{workspace.name} · {workspace.state} · revision {workspace.revision}</h3>
        <code className="break-all text-xs">{workspace.id}</code>
        <div className="flex flex-wrap gap-2"><Button disabled={disabled || closed} onClick={() => void newConversation()}>New conversation in this Workspace</Button>
          {workspace.state !== "deleted" && <Button variant="outline" disabled={disabled} onClick={() => void perform({ action: "archive", ...binding, archived: workspace.state === "active" })}>{workspace.state === "active" ? "Archive" : "Restore"}</Button>}
          <Button variant="destructive" disabled={busy || workspace.state === "deleted" && !workspace.cleanup_pending}
            onClick={() => { setDeleteView(view); setDeleteConfirmed(false); }}>{workspace.cleanup_pending ? "Retry checked cleanup" : "Delete Workspace"}</Button></div>
        {workspace.cleanup_pending && <p className="text-sm">Sharing is deleted; worker retirement is still pending. This is not terminal cleanup proof.</p>}
        <label className="grid gap-1 text-sm">Name<Input value={draft.name} disabled={disabled || workspace.state === "deleted"} maxLength={240} onChange={(event) => edit("name", event.target.value)} /></label>
        <label className="grid gap-1 text-sm">Description<Textarea value={draft.description} disabled={disabled || workspace.state === "deleted"} onChange={(event) => edit("description", event.target.value)} /></label>
        <label className="grid gap-1 text-sm">Shared background<Textarea value={draft.context} disabled={disabled || workspace.state === "deleted"} onChange={(event) => edit("context", event.target.value)} /></label>
        <p className="text-xs">Draft revision {draftRevision}. Background is labelled data, not system policy.</p>
        {draftRevision !== workspace.revision && <Button variant="outline" disabled={disabled} onClick={() => setDraftRevision(workspace.revision)}>I reviewed the displayed revision; apply this retained draft to it</Button>}
        <Button className="w-fit" disabled={disabled || workspace.state === "deleted" || !draft.name.trim() || draftRevision !== workspace.revision}
          onClick={() => void perform({ action: "update", id: workspace.id, expected_revision: draftRevision, draft })}>Save shared context</Button>
      </Card>
      <Card className="gap-3 p-4"><h3 className="font-medium">Sourced shared notes</h3>
        {view.notes.map((note) => <div key={note.id} className="grid gap-1 border-t pt-2"><p className="text-sm font-medium">{note.title} · {note.source === "owner_note" ? "Owner note" : `Agent report · Job ${note.source_job_id}`}</p>
          <pre className="whitespace-pre-wrap break-words text-sm">{note.content}</pre><p className="text-xs">Note {note.id} · revision {note.revision}. A note/report is a remembered claim, not current fact or authority.</p>
          <Button className="w-fit" size="sm" variant="outline" disabled={disabled || workspace.state === "deleted"} onClick={() => void perform({ action: "forget_note", ...binding, note_id: note.id })}>Remove shared note</Button></div>)}
        <Input aria-label="Shared note title" placeholder="Note title" value={noteTitle} disabled={disabled || workspace.state === "deleted"} maxLength={240} onChange={(event) => setNoteTitle(event.target.value)} />
        <Textarea aria-label="Shared note content" value={noteContent} disabled={disabled || workspace.state === "deleted"} onChange={(event) => setNoteContent(event.target.value)} />
        <Button className="w-fit" disabled={disabled || workspace.state === "deleted" || !noteTitle.trim() || !noteContent.trim()} onClick={() => void perform({ action: "put_note", ...binding, note: { title: noteTitle, content: noteContent } })}>Add owner note / correction</Button>
      </Card>
      <Card className="gap-3 p-4"><h3 className="font-medium">Read-only files and folders</h3>
        <p className="text-xs">A folder tag includes present and future descendants. Indexing is independent: registering a directory indexes its supported personal files globally, but does not share those files with other Workspaces.</p>
        <div className="flex gap-2"><Input aria-label="File or folder path to share" placeholder="/absolute/personal/file-or-folder" value={tagPath} disabled={disabled || closed} onChange={(event) => setTagPath(event.target.value)} />
          <Button disabled={disabled || closed || !tagPath.startsWith("/")} onClick={() => void perform({ action: "tag", ...binding, path: tagPath })}>Tag file / folder</Button></div>
        {view.tags.map((tag) => <div key={tag.id} className="grid gap-1 border-t pt-2 text-sm"><code className="break-all">{tag.path}</code><p>{tag.kind} · tag {tag.id}</p>
          <div className="flex flex-wrap gap-2"><Button size="sm" variant="outline" disabled={disabled || closed} onClick={() => void registerIndex(tag.id)}>Register this source directory for indexing</Button>
            <Button size="sm" variant="outline" disabled={disabled || workspace.state === "deleted"} onClick={() => void perform({ action: "untag", ...binding, tag_id: tag.id })}>Remove tag</Button>
            {tag.kind === "file" && <Button size="sm" variant="outline" disabled={disabled || closed} onClick={() => void inspect((signal) => workspaceApi.source(workspace.id, tag.path, 0, undefined, signal), receiveSource)}>Read this file</Button>}</div></div>)}
        <Button className="w-fit" variant="outline" disabled={disabled || closed} onClick={() => void inspect((signal) => workspaceApi.index(workspace.id, signal), setIndex)}>Check local index readiness</Button>
        {index && <div className="text-sm"><p>Local model {index.embedding.model}: {index.embedding.configured ? "configured" : "not ready"} · {index.embedding.detail}</p>
          <p>{index.corpus.indexed_sources} indexed sources · {index.corpus.indexed_chunks} chunks · {index.corpus.state}</p><p>{index.corpus.freshness}</p><p>{index.store}</p><p>{index.formats}</p></div>}
        <div className="flex gap-2"><Input aria-label="Workspace semantic query" value={query} disabled={disabled || closed} onChange={(event) => setQuery(event.target.value)} />
          <Button disabled={disabled || closed || !query.trim()} onClick={() => void inspect((signal) => workspaceApi.search(workspace.id, query, signal), setSearch)}>Search shared sources</Button></div>
        {search && <><p className="text-xs">{search.coverage}. Scores are relevance, not confidence. {search.source_errors}</p>
          {search.hits.map((hit) => <div key={`${hit.path}:${hit.key}`} className="grid gap-1 border-t pt-2 text-sm"><code className="break-all">{hit.path}</code>
            <pre className="whitespace-pre-wrap break-words">{hit.snippet}</pre><p>{hit.source} · indexed {new Date(hit.indexed_at_ms).toLocaleString()} · {hit.score.toFixed(3)}</p>
            <Button className="w-fit" size="sm" variant="outline" disabled={disabled} onClick={() => void inspect((signal) => workspaceApi.source(workspace.id, hit.path, 0, hit.revision, signal), receiveSource)}>Read rechecked source</Button></div>)}</>}
        <div className="flex gap-2"><Input aria-label="Exact tagged source path" placeholder="Exact file inside a tagged folder" value={sourcePath} disabled={disabled || closed} onChange={(event) => setSourcePath(event.target.value)} />
          <Button disabled={disabled || closed || !sourcePath.startsWith("/")} onClick={() => void inspect((signal) => workspaceApi.source(workspace.id, sourcePath, 0, undefined, signal), receiveSource)}>Read source</Button></div>
        {source && <div className="grid gap-2 text-sm"><code className="break-all">{source.path} · revision {source.revision}</code><p>{source.source} · {source.offset}/{source.total_chars} characters</p>
          <pre className="whitespace-pre-wrap break-words">{source.content}</pre>
          {source.next_offset !== null && <Button className="w-fit" variant="outline" disabled={disabled || closed} onClick={() => void inspect((signal) => sourcePreview
            ? workspaceApi.previewSource(workspace.id, sourcePreview, source.path, source.next_offset ?? 0, source.revision, signal)
            : workspaceApi.source(workspace.id, source.path, source.next_offset ?? 0, source.revision, signal), setSource)}>Next revision-bound page</Button>}</div>}
      </Card>
      <Card className="gap-3 p-4"><h3 className="font-medium">Activities</h3>
        {view.activity_ids.map((id) => <div key={id} className="flex flex-wrap items-center gap-2 text-sm"><button className="underline" onClick={() => navigate(`/activities/${id}`)}>{id}</button>
          <Button size="sm" variant="outline" disabled={disabled} onClick={() => void perform({ action: "unbind_activity", ...binding, activity_id: id })}>Detach paused Activity</Button></div>)}
        <div className="flex gap-2"><Input aria-label="Activity UUID to attach" value={activity} disabled={disabled || closed} onChange={(event) => setActivity(event.target.value)} />
          <Button disabled={disabled || closed || !activity} onClick={() => void perform({ action: "bind_activity", ...binding, activity_id: activity })}>Attach idle Activity</Button></div>
        <p className="text-xs">Stop exact Activity Jobs before moving it. Existing conversations with history stay in their original context domain.</p>
      </Card>
      <Card className="gap-3 p-4"><h3 className="font-medium">Explicit temporary cross-Workspace reads</h3>
        <p className="text-xs">Select exact notes/files, an existing Job or canonical conversation, and expiry (at most 24 hours). No recursive grant, transcript sharing, automatic import or App authority is created. Revocation prevents future reads; already-sent bytes cannot be unseen.</p>
        <div className="flex gap-2"><select aria-label="Source Workspace" className="rounded-md border bg-background p-2 text-sm" disabled={disabled || closed} value={originId} onChange={(event) => { setOriginId(event.target.value); setOrigin(null); setNoteIds([]); setSelectedSources([]); }}>
          <option value="">Select another active Workspace</option>{records.filter((record) => record.id !== workspace.id && record.state === "active").map((record) => <option key={record.id} value={record.id}>{record.name}</option>)}</select>
          <Button variant="outline" disabled={disabled || closed || !originId} onClick={() => void inspect((signal) => workspaceApi.get(originId, signal), setOrigin)}>Inspect source selection</Button></div>
        {origin && <>{origin.notes.map((note) => <label key={note.id} className="flex items-center gap-2 text-sm"><input type="checkbox" disabled={disabled} checked={noteIds.includes(note.id)}
          onChange={(event) => setNoteIds((ids) => event.target.checked ? [...ids, note.id] : ids.filter((id) => id !== note.id))} />{note.title} · {note.source} · {note.id}</label>)}
          <select aria-label="Source tag" className="rounded-md border bg-background p-2 text-sm" disabled={disabled} value={sourceTag} onChange={(event) => setSourceTag(event.target.value)}>
            <option value="">Select a live source tag</option>{origin.tags.map((tag) => <option key={tag.id} value={tag.id}>{tag.path} · {tag.kind}</option>)}</select>
          <Input aria-label="Exact temporary file selection" value={selectedPath} disabled={disabled} placeholder="Exact file path (directories are not recursive temporary grants)" onChange={(event) => setSelectedPath(event.target.value)} />
          <Button className="w-fit" variant="outline" disabled={disabled || !sourceTag || !selectedPath.startsWith("/") || noteIds.length + selectedSources.length >= 16}
            onClick={() => { setSelectedSources((sources) => [...sources, { tag_id: sourceTag, path: selectedPath }]); setSelectedPath(""); }}>Select this exact file</Button></>}
        {selectedSources.map((source, index) => <div className="flex gap-2 text-sm" key={`${source.path}:${index}`}><code>{source.path}</code><button className="underline" disabled={busy} onClick={() => setSelectedSources((sources) => sources.filter((_, item) => item !== index))}>Remove selection</button></div>)}
        <select aria-label="Temporary read target kind" className="rounded-md border bg-background p-2 text-sm" value={targetKind} disabled={disabled || closed} onChange={(event) => setTargetKind(event.target.value === "job" ? "job" : "conversation")}><option value="conversation">Exact canonical conversation</option><option value="job">Exact current Job</option></select>
        <Input aria-label="Temporary read target ID" value={targetId} disabled={disabled || closed} placeholder={targetKind === "job" ? "Job UUID" : "ses_..."} onChange={(event) => setTargetId(event.target.value)} />
        <label className="grid gap-1 text-sm">Expiry in minutes (1-1440)<Input type="number" min={1} max={1440} value={expiry} disabled={disabled || closed} onChange={(event) => setExpiry(event.target.value)} /></label>
        <Button className="w-fit" disabled={disabled || closed || !origin || !targetId || !noteIds.length && !selectedSources.length || Number(expiry) < 1 || Number(expiry) > 1440}
          onClick={() => { if (origin) void perform({ action: "read_link", ...binding, source_workspace_id: origin.workspace.id, target: { kind: targetKind, id: targetId },
            note_ids: noteIds, sources: selectedSources, expires_at: new Date(Date.now() + Number(expiry) * 60000).toISOString() }); }}>Allow only these selected temporary reads</Button>
        {workspace.read_links.map((link) => <div key={link.id} className="grid gap-1 border-t pt-2 text-sm"><p>{link.id} · from {link.source_workspace_id} · {link.target.kind} {link.target.id} · expires {link.expires_at}</p>
          <p>{link.note_ids.length} notes · {link.sources.length} exact files. Preview uses this exact saved binding, not global access.</p>
          {link.note_ids.map((noteId) => <Button key={noteId} className="w-fit" size="sm" variant="outline" disabled={disabled || closed}
            onClick={() => void inspect((signal) => workspaceApi.previewNote(workspace.id, link, noteId, signal), setNotePreview)}>Preview selected note {noteId}</Button>)}
          {link.sources.map((selected) => <Button key={selected.path} className="w-fit" size="sm" variant="outline" disabled={disabled || closed}
            onClick={() => void inspect((signal) => workspaceApi.previewSource(workspace.id, link, selected.path, 0, undefined, signal), (page) => { setSource(page); setSourcePreview(link); })}>Preview selected file {selected.path}</Button>)}
          <Button className="w-fit" size="sm" variant="outline" disabled={disabled || closed} onClick={() => void perform({ action: "revoke_link", ...binding, link_id: link.id })}>Revoke temporary read</Button></div>)}
        {notePreview && <div className="grid gap-1 text-sm"><p>Imported {notePreview.note.source === "owner_note" ? "owner note" : "Agent report"} · source Workspace {notePreview.source_workspace_id}</p>
          <p>{notePreview.note.title} · revision {notePreview.note.revision}</p><pre className="whitespace-pre-wrap break-words">{notePreview.note.content}</pre><p>{notePreview.evidence}</p></div>}
      </Card>
    </>}
    {deleteView && <Card role="dialog" aria-label="Confirm Workspace deletion" className="gap-3 border-destructive p-4"><h3 className="font-medium">Delete {deleteView.workspace.name}, revision {deleteView.workspace.revision}?</h3>
      <p className="text-sm">Shared context/tags are removed, automatic work stops, Activities pause, exact Jobs cancel, and pending checked retirement is reported. Source files, conversations, receipts and audit are retained.</p>
      <label className="flex items-center gap-2 text-sm"><input type="checkbox" checked={deleteConfirmed} disabled={busy} onChange={(event) => setDeleteConfirmed(event.target.checked)} />I confirm deletion of this displayed Workspace revision</label>
      <div className="flex gap-2"><Button variant="destructive" disabled={busy || !deleteConfirmed} onClick={() => void perform({ action: "delete", id: deleteView.workspace.id, expected_revision: deleteView.workspace.revision, confirm: true })}>Confirm deletion / checked cleanup</Button>
        <Button variant="outline" disabled={busy} onClick={() => setDeleteView(null)}>Keep Workspace</Button></div></Card>}
  </div></section>;
}
function message(cause: unknown) { return cause instanceof Error ? cause.message : "Workspace request failed"; }
