import { useState } from "react";

import { Button } from "@/components/ui/button";
import { Card } from "@/components/ui/card";
import { Input } from "@/components/ui/input";
import type { useActivityExecutionLimits } from "@/hooks/use-activities";
import { executionLimitsApi } from "@/lib/execution-limits";

export function ActivityExecutionLimitsPanel({
  activityId, ownerUid, editable, disabled, view, mutate,
}: {
  activityId: string;
  ownerUid: number;
  editable: boolean;
  disabled: boolean;
  view: ReturnType<typeof useActivityExecutionLimits>;
  mutate: (action: () => Promise<unknown>, message: string) => Promise<boolean>;
}) {
  const [editing, setEditing] = useState(false);
  const [revision, setRevision] = useState<number | null>(null);
  const [attempts, setAttempts] = useState("10");
  const [turns, setTurns] = useState("5");
  const [expires, setExpires] = useState("");
  const limits = view.data?.execution_limits;
  const identityError = limits && limits.owner_uid !== ownerUid
    ? "Execution-limit owner does not match this Activity." : null;
  const error = view.error || identityError;
  const blocked = disabled || !!error || !view.data;
  const maxAttempts = Number(attempts);
  const maxTurns = Number(turns);
  const valid = Number.isInteger(maxAttempts) && maxAttempts >= 1 && maxAttempts <= 1000
    && Number.isInteger(maxTurns) && maxTurns >= 1 && maxTurns <= 100 && !!expires.trim();

  function edit() {
    setRevision(limits?.revision ?? null);
    setAttempts(String(limits?.limits.max_attempts ?? 10));
    setTurns(String(limits?.limits.max_turns_per_attempt ?? 5));
    setExpires(limits?.limits.expires_at ?? "");
    setEditing(true);
  }

  return (
    <Card role="region" aria-label="Activity execution limits" className="min-w-0 gap-3 p-4">
      <div className="flex items-center justify-between gap-3">
        <h3 className="text-sm font-medium">Execution limits</h3>
        <Button size="sm" variant="outline" aria-label="Refresh execution limits"
          disabled={disabled || view.loading} onClick={() => void view.refresh()}>Refresh</Button>
      </div>
      <p className="text-xs text-muted-foreground">
        Finite attempts, model turns and expiry constrain this Activity; they grant no capabilities.
        An attempt is charged before execution, so a failed startup or recovery can consume an attempt.
        Editing a policy does not reset usage or re-enable it.
      </p>
      {error ? <p role="alert" className="text-sm text-destructive">{error} Limits are hidden until refresh succeeds.</p>
        : !view.data ? <p className="text-sm text-muted-foreground">Loading execution limits...</p>
        : limits ? (
          <>
            <dl className="grid gap-1 text-sm">
              <div><dt className="inline font-medium">Policy: </dt><dd className="inline">{limits.enabled ? "Enabled" : "Disabled"}</dd></div>
              <div><dt className="inline font-medium">Attempts used: </dt><dd className="inline">{limits.used_attempts} / {limits.limits.max_attempts}</dd></div>
              <div><dt className="inline font-medium">Remaining attempts: </dt><dd className="inline">{Math.max(0, limits.limits.max_attempts - limits.used_attempts)}</dd></div>
              <div><dt className="inline font-medium">Turns per attempt: </dt><dd className="inline">{limits.limits.max_turns_per_attempt}</dd></div>
              <div><dt className="inline font-medium">Expires: </dt><dd className="inline break-all">{limits.limits.expires_at}</dd></div>
              <div><dt className="inline font-medium">Revision: </dt><dd className="inline">{limits.revision}</dd></div>
            </dl>
            {limits.used_attempts >= limits.limits.max_attempts
              && <p className="text-xs text-muted-foreground">Attempt ceiling reached. New attempts require an explicit policy change.</p>}
            {Date.parse(limits.limits.expires_at) <= Date.now()
              && <p className="text-xs text-muted-foreground">Policy expiry has passed. The broker makes the authoritative time decision.</p>}
            <div className="flex flex-wrap gap-2">
              <Button size="sm" variant="outline" disabled={blocked || !editable || editing} onClick={edit}>Edit execution limits</Button>
              <Button size="sm" variant="outline" disabled={blocked || (!limits.enabled && !editable)}
                onClick={() => void mutate(
                  async () => {
                    const saved = await executionLimitsApi.enable(activityId, limits.revision, !limits.enabled);
                    if (saved.owner_uid !== ownerUid) throw new Error("Execution-limit acknowledgement has another owner.");
                    return saved;
                  },
                  limits.enabled ? "Activity execution limits disabled; usage is retained." : "Activity execution limits explicitly enabled.",
                )}>{limits.enabled ? "Disable bounded work" : "Enable bounded work"}</Button>
            </div>
          </>
        ) : (
          <>
            <p className="text-sm text-muted-foreground">No Activity execution-limit policy is configured. Existing task behavior is unchanged.</p>
            <Button className="w-fit" size="sm" variant="outline" disabled={blocked || !editable || editing} onClick={edit}>
              Configure execution limits
            </Button>
          </>
        )}
      <p className="text-xs text-muted-foreground">
        Disabling, expiring or changing a policy stops affected attempts through normal cancellation.
        It does not undo effects already admitted by the broker. Disabled means no bounded work, not unlimited work.
      </p>
      {editing && editable && (
        <form className="grid gap-3 border-t pt-3" onSubmit={(event) => {
          event.preventDefault();
          if (blocked || !valid) return;
          void mutate(
            async () => {
              const saved = await executionLimitsApi.set(activityId, revision, {
                max_attempts: maxAttempts, max_turns_per_attempt: maxTurns, expires_at: expires,
              });
              if (saved.owner_uid !== ownerUid) throw new Error("Execution-limit acknowledgement has another owner.");
              return saved;
            },
            "Activity execution limits saved without resetting usage.",
          ).then((saved) => { if (saved) setEditing(false); });
        }}>
          <p className="text-xs text-muted-foreground">
            {revision === null ? "Create an explicit finite policy." : `Editing revision ${revision}; stale changes are refused, never overwritten automatically.`}
          </p>
          <fieldset disabled={blocked} className="grid gap-3 sm:grid-cols-2">
            <label className="grid gap-1.5 text-sm">
              Maximum Activity attempts
              <Input type="number" min={1} max={1000} required value={attempts} onChange={(event) => setAttempts(event.target.value)} />
            </label>
            <label className="grid gap-1.5 text-sm">
              Maximum turns per attempt
              <Input type="number" min={1} max={100} required value={turns} onChange={(event) => setTurns(event.target.value)} />
            </label>
            <label className="grid gap-1.5 text-sm sm:col-span-2">
              Execution policy expiry (RFC3339)
              <Input required value={expires} placeholder="2026-09-18T00:00:00Z" onChange={(event) => setExpires(event.target.value)} />
            </label>
            <div className="flex gap-2 sm:col-span-2">
              <Button type="submit" disabled={!valid}>Save execution limits</Button>
              <Button type="button" variant="outline" onClick={() => setEditing(false)}>Cancel limit edit</Button>
            </div>
          </fieldset>
        </form>
      )}
    </Card>
  );
}
