import { useEffect, useState } from "react";
import { api } from "@/lib/api";
import { MEMORY_KINDS, readMemoryEntry, readMemoryList, type MemoryEntry, type MemoryKind } from "@/lib/memory";
import { Button } from "@/components/ui/button";
import { Card } from "@/components/ui/card";
import { Input } from "@/components/ui/input";
import { Textarea } from "@/components/ui/textarea";

const LABELS: Record<MemoryKind, string> = {
  notes: "Notes: user, system understanding and Agent experience",
  apps: "App-reported memory", conversations: "Recorded interactions", semantic: "Derived semantic index",
};

export function MemoryPage() {
  const [kind, setKind] = useState<MemoryKind>("notes");
  const [query, setQuery] = useState("");
  const [search, setSearch] = useState("");
  const [version, setVersion] = useState(0);
  const [entries, setEntries] = useState<MemoryEntry[]>([]);
  const [coverage, setCoverage] = useState("");
  const [hasMore, setHasMore] = useState(false);
  const [selected, setSelected] = useState<MemoryEntry | null>(null);
  const [editor, setEditor] = useState<{ name: string; revision: string; content: string } | null>(null);
  const [confirmForget, setConfirmForget] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [loading, setLoading] = useState(false);
  useEffect(() => {
    const controller = new AbortController();
    setSelected(null); setEditor(null); setConfirmForget(false); setError(null);
    if (kind === "conversations" && !search.trim()) {
      setEntries([]); setCoverage("Enter a query to search interactions. Sessions provides chronological browsing.");
      setLoading(false); return () => controller.abort();
    }
    setLoading(true);
    void api.post("/api/memory/inspect", { kind, query: search, limit: 50 }, { signal: controller.signal })
      .then((value) => {
        const list = readMemoryList(value);
        if (!controller.signal.aborted) { setEntries(list.entries); setCoverage(list.coverage); setHasMore(list.has_more); }
      }).catch((failure: unknown) => {
        if (!controller.signal.aborted) { setEntries([]); setError(failure instanceof Error ? failure.message : "Memory unavailable."); }
      }).finally(() => { if (!controller.signal.aborted) setLoading(false); });
    return () => controller.abort();
  }, [kind, search, version]);
  const open = async (entry: MemoryEntry) => {
    setBusy(true); setError(null); setEditor(null); setConfirmForget(false);
    try {
      setSelected(readMemoryEntry(await api.post("/api/memory/read", { reference: entry.reference })));
    } catch (failure: unknown) {
      setSelected(null); setError(failure instanceof Error ? failure.message : "Memory source unavailable.");
    } finally { setBusy(false); }
  };
  const save = async () => {
    if (!editor) return;
    setBusy(true); setError(null);
    try {
      readMemoryEntry(await api.post("/api/memory/update", editor));
      setVersion((value) => value + 1);
    } catch (failure: unknown) { setError(failure instanceof Error ? failure.message : "Memory edit failed."); }
    finally { setBusy(false); }
  };
  const forget = async () => {
    if (!selected || !confirmForget) return;
    setBusy(true); setError(null);
    try {
      const response = await api.post<{ forgotten?: boolean; conversations_preserved?: boolean }>("/api/memory/forget", {
        reference: selected.reference, revision: selected.revision, confirm: true,
      });
      if (response.forgotten !== true || response.conversations_preserved !== true) throw new Error("Memory deletion was not acknowledged.");
      setVersion((value) => value + 1);
    } catch (failure: unknown) { setError(failure instanceof Error ? failure.message : "Memory deletion failed."); }
    finally { setBusy(false); }
  };
  return (
    <div className="flex h-full flex-col gap-4 overflow-y-auto p-6">
      <div className="flex items-center justify-between">
        <div><h1 className="text-xl font-semibold">Memory</h1><p className="text-xs text-muted-foreground">Owner-scoped remembered data, not system authority or proof of current state.</p></div>
        <Button size="sm" variant="outline" disabled={busy} onClick={() => setVersion((value) => value + 1)}>Refresh memory</Button>
      </div>
      <div className="flex flex-wrap gap-2">
        {MEMORY_KINDS.map((value) => <Button key={value} size="sm" variant={kind === value ? "secondary" : "outline"} disabled={busy}
          onClick={() => { setKind(value); setQuery(""); setSearch(""); }}>{LABELS[value]}</Button>)}
      </div>
      <form className="flex gap-2" onSubmit={(event) => { event.preventDefault(); setSearch(query); }}>
        <Input aria-label="Search memory" value={query} disabled={busy} onChange={(event) => setQuery(event.target.value)} placeholder="Search remembered text or source..." />
        <Button size="sm" disabled={busy}>Search</Button>
      </form>
      <p className="text-xs text-muted-foreground">{coverage}{hasMore && " More matches exist; refine the query."}</p>
      {error && <p role="alert" className="text-sm text-destructive">{error} No edit was retried. Refresh before deciding again.</p>}
      {loading ? <p className="text-sm text-muted-foreground">Loading owner memory...</p> : (
        <div className="grid gap-3 md:grid-cols-2">
          <div className="grid content-start gap-2">
            {kind === "notes" && <Button variant="outline" disabled={busy} onClick={() => {
              setSelected(null); setEditor({ name: "NEW-NOTE.md", revision: "new", content: "" });
            }}>New memory note</Button>}
            {entries.map((entry) => <button key={JSON.stringify(entry.reference)} className="rounded border p-3 text-left hover:bg-muted" disabled={busy}
              onClick={() => void open(entry)}>
              <div className="break-all text-sm font-medium">{entry.source}</div>
              <div className="text-[11px] text-muted-foreground">{entry.category} &middot; {entry.recorded_at_ms === null ? "Time unavailable" : new Date(entry.recorded_at_ms).toLocaleString()}</div>
              <p className="mt-1 whitespace-pre-wrap break-words text-xs">{entry.content}</p>
            </button>)}
            {entries.length === 0 && !error && <p className="text-sm text-muted-foreground">No remembered entries in this bounded view.</p>}
          </div>
          <div className="grid content-start gap-3">
            {selected && <Card className="grid gap-2 p-3" aria-label="Memory source details">
              <h2 className="break-all text-sm font-semibold">{selected.source}</h2>
              <p className="text-xs text-muted-foreground">{selected.provenance}</p>
              <p className="break-all text-[11px] text-muted-foreground">Source revision: {selected.revision}</p>
              <pre className="max-h-72 overflow-auto whitespace-pre-wrap break-words text-xs">{selected.content}</pre>
              <div className="flex flex-wrap gap-2">
                {selected.editable && selected.reference.kind === "note" && <Button size="sm" disabled={busy} onClick={() => {
                  if (selected.reference.kind === "note") setEditor({ name: selected.reference.name, revision: selected.revision, content: selected.content });
                }}>Edit memory note</Button>}
                {!selected.editable && <Button size="sm" variant="outline" disabled={busy} onClick={() => setEditor({
                  name: `CORRECTION-${Date.now()}.md`, revision: "new",
                  content: `User correction for ${selected.source}\nOriginal revision: ${selected.revision}\n\n`,
                })}>Add owner correction</Button>}
                {selected.deletable && <Button size="sm" variant="outline" disabled={busy} onClick={() => setConfirmForget(true)}>Forget learned entry</Button>}
              </div>
              {confirmForget && <div className="grid gap-2 border-t pt-2">
                <p className="text-xs">Forget this exact learned source? Conversation and execution evidence will be preserved. This does not undo any OS/App effects.</p>
                <div className="flex gap-2"><Button size="sm" variant="destructive" disabled={busy} onClick={() => void forget()}>Confirm forget</Button>
                  <Button size="sm" variant="outline" onClick={() => setConfirmForget(false)}>Keep entry</Button></div>
              </div>}
            </Card>}
            {editor && <Card className="grid gap-2 p-3" aria-label="Memory note editor">
              <p className="text-xs text-muted-foreground">Owner/Agent note only. This cannot change system policy, capabilities or another source's provenance.</p>
              <label className="grid gap-1 text-xs">Memory note name<Input aria-label="Memory note name" value={editor.name} disabled={busy || editor.revision !== "new"} onChange={(event) => setEditor({ ...editor, name: event.target.value })} /></label>
              <label className="grid gap-1 text-xs">Memory note content<Textarea aria-label="Memory note content" rows={12} value={editor.content} disabled={busy} onChange={(event) => setEditor({ ...editor, content: event.target.value })} /></label>
              <div className="flex gap-2"><Button size="sm" disabled={busy} onClick={() => void save()}>Save memory note</Button><Button size="sm" variant="outline" disabled={busy} onClick={() => setEditor(null)}>Cancel edit</Button></div>
            </Card>}
          </div>
        </div>
      )}
    </div>
  );
}
