import { useEffect, useState } from "react";
import { api } from "@/lib/api";
import { readFollowUps, type FollowUp, type FollowUpQueue } from "@/lib/followups";
import { Button } from "./ui/button";

export function FollowUpQueuePanel({ taskId, version }: { taskId: string; version: number }) {
  const [queue, setQueue] = useState<FollowUpQueue | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [selection, setSelection] = useState<FollowUp[] | null>(null);
  const [refresh, setRefresh] = useState(0);
  useEffect(() => {
    const controller = new AbortController();
    let loading = false;
    const load = async () => {
      if (loading || controller.signal.aborted) return;
      loading = true;
      try {
        const value = readFollowUps(await api.get(`/api/tasks/${encodeURIComponent(taskId)}/queue`, { signal: controller.signal }), taskId);
        if (!controller.signal.aborted) { setQueue(value); setError(null); }
      } catch (failure: unknown) {
        if (!controller.signal.aborted) { setQueue(null); setError(failure instanceof Error ? failure.message : "Follow-up queue unavailable."); }
      } finally { loading = false; }
    };
    void load();
    const timer = window.setInterval(() => void load(), 3_000);
    return () => { controller.abort(); window.clearInterval(timer); };
  }, [taskId, version, refresh]);
  const cancel = async (ids: string[]) => {
    setBusy(true); setError(null);
    try {
      for (const id of ids.slice().reverse()) {
        if (id === taskId) throw new Error("The current task is not a follow-up cancellation target.");
        const result = await api.post<{ cancelled?: boolean; cancel_requested?: boolean }>(`/api/tasks/${encodeURIComponent(id)}/stop`);
        if (result.cancelled !== true && result.cancel_requested !== true) throw new Error("Job cancellation was not acknowledged. Refresh to inspect its current state.");
      }
      setSelection(null); setRefresh((value) => value + 1);
    } catch (failure: unknown) {
      setError(`${failure instanceof Error ? failure.message : "Cancellation failed."} Some earlier Jobs may have been cancelled; no cancellation was retried.`);
    } finally { setBusy(false); }
  };
  if (error) return <div role="alert" className="mt-2 text-xs text-destructive">{error}
    <Button size="sm" variant="outline" disabled={busy} onClick={() => setRefresh((value) => value + 1)}>Refresh queue</Button>
  </div>;
  if (!queue || (!queue.tasks.length && !queue.truncated)) return null;
  const displayedTasks = selection || queue.tasks;
  return (
    <details className="mt-2 rounded border p-2 text-xs" aria-label="Durable follow-up queue">
      <summary className="cursor-pointer">Follow-ups ({displayedTasks.length})</summary>
      <p className="my-2 text-muted-foreground">Stopping the current task does not cancel these Jobs. If a selected Job has just started, its cancellation is requested too; admitted effects are not undone.</p>
      {displayedTasks.map((task, index) => <div key={task.id} className="flex items-start justify-between gap-2 border-t py-2">
        <div><p className="whitespace-pre-wrap break-words">{index + 1}. {task.prompt}</p><p className="text-muted-foreground">{task.status} &middot; {task.id}</p></div>
        <Button size="sm" variant="outline" disabled={busy} onClick={() => void cancel([task.id])}>Cancel this follow-up</Button>
      </div>)}
      {queue.truncated && <p className="text-amber-600">Queue coverage is incomplete. Open Tasks for the remaining records.</p>}
      {!selection ? queue.tasks.length > 0 && <Button size="sm" variant="outline" disabled={busy} onClick={() => setSelection(queue.tasks.slice())}>Cancel listed follow-ups</Button> : <div className="mt-2 flex items-center gap-2">
        <span>Cancel only these {selection.length} selected Jobs, not the current task or new arrivals?</span>
        <Button size="sm" variant="destructive" disabled={busy} onClick={() => void cancel(selection.map((task) => task.id))}>Confirm cancel listed follow-ups</Button>
        <Button size="sm" variant="outline" disabled={busy} onClick={() => setSelection(null)}>Keep queue</Button>
      </div>}
    </details>
  );
}
