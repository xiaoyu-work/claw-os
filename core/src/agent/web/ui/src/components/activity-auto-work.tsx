import { useEffect, useRef, useState } from "react";
import { Button } from "@/components/ui/button";
import { Card } from "@/components/ui/card";
import { Input } from "@/components/ui/input";
import { Textarea } from "@/components/ui/textarea";
import type { useActivityAutoWork } from "@/hooks/use-activities";
import { autoWorkApi, type WorkDraft } from "@/lib/activity-auto-work";

export function ActivityAutoWorkPanel({ activityId, ownerUid, active, editable, disabled, view, mutate }: {
  activityId: string; ownerUid: number; active: boolean; editable: boolean; disabled: boolean;
  view: ReturnType<typeof useActivityAutoWork>;
  mutate: (action: () => Promise<unknown>, notice: string) => Promise<boolean>;
}) {
  const [form, setForm] = useState<WorkDraft | null>(null);
  const [revision, setRevision] = useState<string | null>(null);
  const [review, setReview] = useState(false);
  const [nextTask, setNextTask] = useState("");
  const [reviewNote, setReviewNote] = useState("");
  const mounted = useRef(true);
  useEffect(() => {
    mounted.current = true;
    return () => { mounted.current = false; };
  }, [activityId]);
  const work = view.data?.work;
  const error = view.error || (work && work.owner_uid !== ownerUid ? "Automatic-work owner does not match this Activity." : null);
  const blocked = disabled || !!error || !view.data;
  const needsReview = work?.phase === "review" || work?.phase === "held";
  async function change(action: () => Promise<unknown>, notice: string) {
    if (await mutate(action, notice) && mounted.current) {
      setForm(null); setReview(false);
    }
  }
  return (
    <Card role="region" aria-label="Activity automatic work" className="min-w-0 gap-3 p-4">
      <div className="flex items-center justify-between gap-3">
        <h3 className="text-sm font-medium">Automatic work</h3>
        <Button size="sm" variant="outline" disabled={disabled || view.loading} onClick={() => void view.refresh()}>Refresh automatic work</Button>
      </div>
      <p className="text-xs text-muted-foreground">
        Explicitly enabled, bounded segments run through the existing daemon and Job queue.
        A finished segment can continue immediately or wait for its configured time/event.
        Finite execution limits, ordinary permissions, approvals and budgets still apply.
      </p>
      <p className="text-xs text-muted-foreground">
        Handoffs, completed-step labels and artifact references are caller reports, not verified effects.
        Goal completion still requires your confirmation. Stopping progression does not cancel an existing Job.
      </p>
      {error ? <p role="alert" className="text-destructive">{error} Refresh before changing work.</p> : !view.data ? (
        <p className="text-sm text-muted-foreground">Loading automatic work...</p>
      ) : work ? (
        <>
          <p className="text-sm">
            {work.enabled ? "Automatic progression enabled" : "Automatic progression stopped"} · {work.phase === "running" ? "Job in flight" : work.phase}
            {" · Revision "}{work.revision}
          </p>
          {work.message && <p className="whitespace-pre-wrap break-words text-sm">{work.message}</p>}
          <p className="whitespace-pre-wrap break-words text-sm">Next task: {work.next_task}</p>
          {work.next_wake_at && <p className="text-xs">Next waiting check: <time dateTime={work.next_wake_at}>{work.next_wake_at}</time></p>}
          {work.current && <a className="text-sm underline" href={`#/tasks/${encodeURIComponent(work.current.job_id)}`}>Inspect current Job</a>}
          {work.last_checkpoint?.checkpoint && (
            <div className="grid gap-2 border-t pt-3">
              <h4 className="text-sm font-medium">Latest reported handoff</h4>
              <p className="whitespace-pre-wrap break-words text-sm">{work.last_checkpoint.checkpoint.summary}</p>
              {work.last_checkpoint.checkpoint.completed_steps.map((step, index) => <p key={index} className="text-xs">Reported step: {step}</p>)}
              {work.last_checkpoint.checkpoint.artifacts.map((reference) => <code key={reference} className="break-all text-xs">{reference}</code>)}
            </div>
          )}
          <div className="flex flex-wrap gap-2">
            {work.enabled ? (
              <Button size="sm" variant="outline" disabled={blocked || !!form || review}
                onClick={() => void change(() => autoWorkApi.enabled(activityId, ownerUid, work.revision, false), "Automatic progression stopped; any current Job is unchanged.")}>
                Stop automatic progression
              </Button>
            ) : needsReview ? (
              <Button size="sm" disabled={blocked || !active || !!work.current || !!form || review} onClick={() => {
                setRevision(work.revision); setNextTask(""); setReviewNote(""); setReview(true);
              }}>Review and resume work</Button>
            ) : (
              <Button size="sm" disabled={blocked || !active || !!work.current || !!form || review}
                onClick={() => void change(() => autoWorkApi.enabled(activityId, ownerUid, work.revision, true), "Automatic progression explicitly enabled.")}>
                Start automatic work
              </Button>
            )}
          </div>
        </>
      ) : <p className="text-sm text-muted-foreground">Automatic progression is not configured or enabled.</p>}
      {!form && !review && (
        <Button className="w-fit" size="sm" variant="outline"
          disabled={blocked || !editable || !!work?.current || needsReview}
          onClick={() => {
            setRevision(work?.revision ?? null);
            setForm(work?.config ?? { task: "", check_interval_secs: 3600, wake_event: null });
          }}>{work ? "Edit automatic work" : "Configure automatic work"}</Button>
      )}
      {form && (
        <form className="grid gap-3 border-t pt-3" onSubmit={(event) => {
          event.preventDefault();
          if (!blocked) void change(() => autoWorkApi.set(activityId, ownerUid, revision, form), "Automatic work configured, initially stopped.");
        }}>
          <p className="text-xs">Editing revision {revision ?? "new"}. Conflicts retain this draft; refresh never rebases it.</p>
          <label className="grid gap-1 text-sm">Owner task
            <Textarea value={form.task} maxLength={8192} disabled={disabled} onChange={(event) => setForm({ ...form, task: event.target.value })} />
          </label>
          <label className="grid gap-1 text-sm">Waiting-check interval (seconds, default one hour)
            <Input type="number" min={60} max={86400} step={1} value={form.check_interval_secs} disabled={disabled}
              onChange={(event) => setForm({ ...form, check_interval_secs: Number(event.target.value) })} />
          </label>
          <label className="grid gap-1 text-sm">Optional existing event source
            <Input value={form.wake_event?.source ?? ""} disabled={disabled} onChange={(event) => setForm({
              ...form, wake_event: event.target.value || form.wake_event?.event_type
                ? { source: event.target.value, event_type: form.wake_event?.event_type ?? "" } : null,
            })} />
          </label>
          <label className="grid gap-1 text-sm">Optional existing event type
            <Input value={form.wake_event?.event_type ?? ""} disabled={disabled} onChange={(event) => setForm({
              ...form, wake_event: event.target.value || form.wake_event?.source
                ? { source: form.wake_event?.source ?? "", event_type: event.target.value } : null,
            })} />
          </label>
          <p className="text-xs text-muted-foreground">Use an actual producer and both event fields. This does not install a watcher or grant access.</p>
          <div className="flex gap-2">
            <Button type="submit" disabled={blocked || !form.task.trim()}>Save stopped configuration</Button>
            <Button type="button" variant="outline" disabled={disabled} onClick={() => setForm(null)}>Discard work draft</Button>
          </div>
        </form>
      )}
      {review && revision && (
        <form className="grid gap-3 border-t pt-3" onSubmit={(event) => {
          event.preventDefault();
          if (!blocked) void change(() => autoWorkApi.enabled(activityId, ownerUid, revision, true, nextTask, reviewNote), "Inspected work explicitly resumed with a new next task.");
        }}>
          <p className="text-xs">Inspect the prior Job and external state. Uncertain effects are not replayed automatically. Reviewing does not verify them.</p>
          <label className="grid gap-1 text-sm">Safe next task
            <Textarea aria-label="Safe next task" placeholder="Safe next task after inspection" value={nextTask} maxLength={8192}
              disabled={disabled} onChange={(event) => setNextTask(event.target.value)} />
          </label>
          <label className="grid gap-1 text-sm">Work review note
            <Textarea aria-label="Work review note" placeholder="What you inspected" value={reviewNote} maxLength={2048}
              disabled={disabled} onChange={(event) => setReviewNote(event.target.value)} />
          </label>
          <div className="flex gap-2">
            <Button type="submit" disabled={blocked || !nextTask.trim() || !reviewNote.trim()}>Resume inspected work</Button>
            <Button type="button" variant="outline" disabled={disabled} onClick={() => setReview(false)}>Keep stopped</Button>
          </div>
        </form>
      )}
      <p className="text-xs text-muted-foreground">
        If starting requires scheduler permission, review the exact request in <a href="#/approvals" className="underline">Approvals</a> and explicitly start again.
      </p>
    </Card>
  );
}
