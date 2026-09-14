import { useState } from "react";

import { Button } from "@/components/ui/button";
import { Card } from "@/components/ui/card";
import type { useActivitySchedulingPriority } from "@/hooks/use-activities";
import {
  schedulingPriorities,
  schedulingPriorityApi,
  type ActivitySchedulingPriority,
} from "@/lib/scheduling-priority";

const labels: Record<ActivitySchedulingPriority, string> = {
  foreground: "Foreground",
  standard: "Standard",
  background: "Background",
};

export function ActivitySchedulingPriorityPanel({
  activityId, ownerUid, editable, disabled, view, mutate,
}: {
  activityId: string;
  ownerUid: number;
  editable: boolean;
  disabled: boolean;
  view: ReturnType<typeof useActivitySchedulingPriority>;
  mutate: (action: () => Promise<unknown>, message: string) => Promise<boolean>;
}) {
  const [editing, setEditing] = useState(false);
  const [revision, setRevision] = useState<string | null>(null);
  const [selected, setSelected] = useState<ActivitySchedulingPriority>("standard");
  const policy = view.data?.scheduling_policy;
  const identityError = policy && policy.owner_uid !== ownerUid
    ? "Scheduling-priority owner does not match this Activity."
    : null;
  const error = view.error || identityError;
  const blocked = disabled || !!error || !view.data;

  function edit() {
    setRevision(policy?.revision ?? null);
    setSelected(policy?.priority ?? "standard");
    setEditing(true);
  }

  return (
    <Card role="region" aria-label="Activity scheduling priority" className="min-w-0 gap-3 p-4">
      <div className="flex items-center justify-between gap-3">
        <h3 className="text-sm font-medium">Scheduling priority</h3>
        <Button size="sm" variant="outline" aria-label="Refresh scheduling priority"
          disabled={disabled || view.loading} onClick={() => void view.refresh()}>
          Refresh
        </Button>
      </div>
      <p className="text-xs text-muted-foreground">
        Foreground is preferred at pending admission, standard preserves ordinary compatibility,
        and background may be deferred. Work waiting at least 30 minutes is aged ahead of non-aged
        work in FIFO order to prevent starvation.
      </p>
      <p className="text-xs text-muted-foreground">
        This changes pending admission only. It grants no authority, capability, consent, completion,
        execution proof, preemption, cancellation, guaranteed latency, or provider quality of service.
      </p>
      {error ? (
        <p role="alert" className="text-sm text-destructive">
          {error} Priority data is hidden until refresh succeeds.
        </p>
      ) : !view.data ? (
        <p className="text-sm text-muted-foreground">Loading scheduling priority...</p>
      ) : policy ? (
        <>
          <dl className="grid gap-1 text-sm">
            <div><dt className="inline font-medium">Current priority: </dt><dd className="inline">{labels[policy.priority]}</dd></div>
            <div><dt className="inline font-medium">Revision: </dt><dd className="inline">{policy.revision}</dd></div>
          </dl>
          <Button className="w-fit" size="sm" variant="outline"
            disabled={blocked || !editable || editing} onClick={edit}>
            Edit scheduling priority
          </Button>
        </>
      ) : (
        <>
          <p className="text-sm text-muted-foreground">
            No scheduling policy is configured. Standard FIFO-compatible admission applies.
          </p>
          <Button className="w-fit" size="sm" variant="outline"
            disabled={blocked || !editable || editing} onClick={edit}>
            Configure scheduling priority
          </Button>
        </>
      )}
      {!editable && (
        <p className="text-xs text-muted-foreground">
          Reopen this Activity before changing its scheduling priority.
        </p>
      )}
      {editing && editable && (
        <form className="grid gap-3 border-t pt-3" onSubmit={(event) => {
          event.preventDefault();
          if (blocked) return;
          void mutate(
            () => schedulingPriorityApi.set(
              activityId,
              ownerUid,
              revision,
              selected,
              policy ?? undefined,
            ),
            `Activity scheduling priority saved as ${labels[selected].toLowerCase()} with exact revision.`,
          ).then((saved) => {
            if (saved) setEditing(false);
          });
        }}>
          <p className="text-xs text-muted-foreground">
            {revision === null
              ? "Create a revision-1 admission policy. This does not start or interrupt work."
              : `Editing revision ${revision}; stale changes are refused and never rebased automatically.`}
          </p>
          <fieldset disabled={blocked} className="grid gap-2">
            <legend className="mb-1 text-sm font-medium">Pending admission class</legend>
            <div className="flex flex-wrap gap-2">
              {schedulingPriorities.map((priority) => (
                <Button key={priority} type="button"
                  variant={selected === priority ? "default" : "outline"}
                  aria-pressed={selected === priority}
                  onClick={() => setSelected(priority)}>
                  {labels[priority]}
                </Button>
              ))}
            </div>
            <div className="flex gap-2 pt-1">
              <Button type="submit">Save scheduling priority</Button>
              <Button type="button" variant="outline" onClick={() => setEditing(false)}>
                Cancel priority edit
              </Button>
            </div>
          </fieldset>
        </form>
      )}
    </Card>
  );
}
