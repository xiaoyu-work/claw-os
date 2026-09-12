import { useEffect, useRef, useState } from "react";
import { Loader2 } from "lucide-react";

import { ActivityForm } from "@/components/activity-form";
import { ActivityObjectsPanel } from "@/components/activity-objects";
import { ActivityObjectStatePanel } from "@/components/activity-object-state";
import { ActivityExecutionLimitsPanel } from "@/components/activity-execution-limits";
import { ActivityReceiptsPanel } from "@/components/activity-receipts";
import { ActivityWork } from "@/components/activity-work";
import { Button } from "@/components/ui/button";
import { Card } from "@/components/ui/card";
import { Textarea } from "@/components/ui/textarea";
import { activityError, useActivity, useActivityObjects, useActivityObjectState, useActivityReceipts, useActivityExecutionLimits } from "@/hooks/use-activities";
import { activityApi, activityStateLabels, type ActivityState } from "@/lib/activities";
import { cn } from "@/lib/utils";

export function ActivityStateBadge({ state }: { state: ActivityState }) {
  return (
    <span className={cn(
      "w-fit rounded-full border px-2 py-0.5 text-[11px] font-medium",
      state === "active" && "text-blue-600 dark:text-blue-400",
      state === "paused" && "text-amber-600 dark:text-amber-400",
      state === "completed" && "text-emerald-600 dark:text-emerald-400",
      state === "cancelled" && "text-muted-foreground",
    )}>
      {activityStateLabels[state]}
    </span>
  );
}

export function ActivityDetailPanel({
  id, onChanged,
}: { id: string; onChanged: () => Promise<unknown> }) {
  const view = useActivity(id);
  const objects = useActivityObjects(id);
  const receipts = useActivityReceipts(id);
  const objectState = useActivityObjectState(id);
  const executionLimits = useActivityExecutionLimits(id);
  const [editing, setEditing] = useState(false);
  const [confirming, setConfirming] = useState(false);
  const [completionNote, setCompletionNote] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const pending = useRef(false);
  const mounted = useRef(true);
  const changed = useRef(onChanged);
  changed.current = onChanged;
  useEffect(() => {
    mounted.current = true;
    return () => { mounted.current = false; };
  }, []);

  async function mutate(action: () => Promise<unknown>, message: string): Promise<boolean> {
    if (pending.current) return false;
    pending.current = true;
    setBusy(true);
    setError(null);
    setNotice(null);
    try {
      await action();
      if (!mounted.current) return false;
      void objects.refresh();
      void receipts.refresh();
      void objectState.refresh();
      void executionLimits.refresh();
      const [fresh] = await Promise.all([view.refresh(), changed.current()]);
      if (!mounted.current) return false;
      setNotice(message);
      return fresh !== null;
    } catch (cause) {
      if (mounted.current) setError(activityError(cause));
      return false;
    } finally {
      pending.current = false;
      if (mounted.current) setBusy(false);
    }
  }

  const detail = view.data;
  const activity = detail?.activity;
  const editable = activity?.state === "active" || activity?.state === "paused";
  const disabled = busy || !!view.error;
  useEffect(() => {
    if (activity && !editable) {
      setEditing(false);
      setConfirming(false);
      setCompletionNote("");
    }
  }, [activity?.state, editable]);

  return (
    <section aria-label="Activity detail" className="grid min-w-0 gap-4">
      <div className="flex items-center justify-between gap-3">
        <h2 className="break-words text-lg font-semibold">{activity?.title ?? "Activity detail"}</h2>
        <Button size="sm" variant="outline" disabled={view.loading || busy}
          aria-label="Refresh activity detail"
          onClick={() => void Promise.all([view.refresh(), objects.refresh(), receipts.refresh(), objectState.refresh(), executionLimits.refresh()])}>
          {view.loading ? <Loader2 className="h-3.5 w-3.5 animate-spin" /> : "Refresh"}
        </Button>
      </div>
      {view.error && (
        <p role="alert" className="text-sm text-destructive">
          {view.error}{activity ? " Showing the last fetched details; refresh before making changes." : ""}
        </p>
      )}
      {error && <p role="alert" className="text-sm text-destructive">{error}</p>}
      {notice && <p role="status" className="text-sm text-muted-foreground">{notice}</p>}
      {!activity && view.loading && <p className="text-sm text-muted-foreground">Loading Activity…</p>}
      {activity && detail && (
        <>
          <Card className="gap-3 p-4">
            <div className="flex flex-wrap items-center gap-2">
              <ActivityStateBadge state={activity.state} />
              <span className="break-all font-mono text-[11px] text-muted-foreground">{activity.id}</span>
            </div>
            <p className="text-xs text-muted-foreground">
              Updated <time dateTime={activity.updated_at}>{new Date(activity.updated_at).toLocaleString()}</time>
            </p>
            <div className="flex flex-wrap gap-2">
              {editable ? (
                <>
                  <Button size="sm" variant="outline" disabled={disabled || editing}
                    onClick={() => { setEditing(true); setConfirming(false); }}>Edit activity</Button>
                  <Button size="sm" variant="outline" disabled={disabled}
                    onClick={() => void mutate(
                      () => activityApi.transition(id, activity.state === "paused" ? "active" : "paused"),
                      activity.state === "paused" ? "Activity resumed." : "Activity paused.",
                    )}>
                    {activity.state === "paused" ? "Resume activity" : "Pause activity"}
                  </Button>
                  <Button size="sm" variant="outline" disabled={disabled || confirming}
                    onClick={() => { setConfirming(true); setEditing(false); setCompletionNote(""); }}>
                    Mark completed
                  </Button>
                  <Button size="sm" variant="ghost" disabled={disabled}
                    onClick={() => void mutate(
                      () => activityApi.transition(id, "cancelled"), "Activity cancelled.",
                    )}>Cancel activity</Button>
                </>
              ) : (
                <Button size="sm" variant="outline" disabled={disabled}
                  onClick={() => void mutate(
                    () => activityApi.transition(id, "active"), "Activity explicitly reopened.",
                  )}>Reopen activity</Button>
              )}
            </div>
            <p className="text-xs text-muted-foreground">
              Pausing or cancelling gates later work; it does not undo or stop current effects.
              Use Tasks to cancel a job. A successful job does not complete this Activity.
            </p>
            {!editable && <p className="text-xs text-muted-foreground">Reopen explicitly to edit or submit more work.</p>}
            {activity.completion_note !== null && (
              <div className="grid gap-1 text-sm">
                <h3 className="font-medium">Completion confirmation</h3>
                <p className="whitespace-pre-wrap break-words">{activity.completion_note}</p>
              </div>
            )}
          </Card>
          {confirming && editable && (
            <Card className="gap-3 p-4">
              <h3 className="font-medium">Confirm the goal is complete</h3>
              <p className="text-xs text-muted-foreground">
                Review the results against your criteria. Only your explicit confirmation completes the goal.
              </p>
              <form className="grid gap-3" onSubmit={(event) => {
                event.preventDefault();
                if (!completionNote.trim() || disabled) return;
                void mutate(
                  () => activityApi.transition(id, "completed", completionNote),
                  "Activity completed with your confirmation.",
                ).then((ok) => { if (ok) setConfirming(false); });
              }}>
                <label className="grid gap-1.5 text-sm">
                  Completion note
                  <Textarea required maxLength={8192} value={completionNote} disabled={disabled}
                    onChange={(event) => setCompletionNote(event.target.value)} />
                </label>
                <div className="flex gap-2">
                  <Button type="submit" disabled={disabled || !completionNote.trim()}>Confirm completion</Button>
                  <Button type="button" variant="outline" disabled={busy}
                    onClick={() => setConfirming(false)}>Not yet</Button>
                </div>
              </form>
            </Card>
          )}
          {editing && editable ? (
            <Card className="gap-4 p-5">
              <h3 className="font-medium">Edit planning details</h3>
              <ActivityForm initial={activity} busy={disabled} submitLabel="Save changes"
                onCancel={() => setEditing(false)}
                onSubmit={async (draft) => {
                  if (await mutate(() => activityApi.update(id, draft), "Activity saved.")) setEditing(false);
                }} />
            </Card>
          ) : (
            <div className="grid min-w-0 gap-4 xl:grid-cols-2">
              <TextCard title="Goal" text={activity.goal} />
              <TextCard title="Completion criteria" text={activity.completion_criteria} />
              <TextCard title="Boundaries" text={activity.boundaries}
                note="Planning guidance, not enforced permissions. Ordinary capabilities and approvals remain authoritative." />
              <Card className="min-w-0 gap-3 p-4">
                <h3 className="text-sm font-medium">Resources</h3>
                <p className="text-xs text-muted-foreground">Inert references, not opened or executed automatically.</p>
                {activity.resources.length ? (
                  <ul className="grid gap-3">
                    {activity.resources.map((resource, index) => (
                      <li key={index} className="grid min-w-0 gap-1 text-sm">
                        <span className="break-words font-medium">{resource.label}</span>
                        <code className="whitespace-pre-wrap break-all text-xs text-muted-foreground">{resource.reference}</code>
                      </li>
                    ))}
                  </ul>
                ) : <p className="text-sm text-muted-foreground">No references added.</p>}
              </Card>
            </div>
          )}
          <ActivityObjectsPanel view={objects} editable={editable} editing={editing} disabled={disabled}
            onAttach={(attachment) => mutate(
              () => activityApi.attachObject(id, attachment),
              "Object reference attached. No App code was executed.",
            )} />
          <ActivityWork detail={detail} disabled={disabled} mutate={mutate} />
          <ActivityExecutionLimitsPanel key={`limits-${id}`} activityId={id} ownerUid={activity.owner_uid}
            editable={editable} disabled={disabled} view={executionLimits} mutate={mutate} />
          <ActivityReceiptsPanel view={receipts} ownerUid={activity.owner_uid} />
          <ActivityObjectStatePanel key={id} activityId={id} ownerUid={activity.owner_uid}
            resources={activity.resources} view={objectState} disabled={disabled} />
        </>
      )}
    </section>
  );
}

function TextCard({ title, text, note }: { title: string; text: string; note?: string }) {
  return (
    <Card className="min-w-0 gap-3 p-4">
      <h3 className="text-sm font-medium">{title}</h3>
      <p className={cn("whitespace-pre-wrap break-words text-sm", !text && "text-muted-foreground")}>
        {text || "Not specified."}
      </p>
      {note && <p className="text-xs text-muted-foreground">{note}</p>}
    </Card>
  );
}
