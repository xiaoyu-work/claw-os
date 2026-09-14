import { useState } from "react";

import { Button } from "@/components/ui/button";
import { Card } from "@/components/ui/card";
import { Input } from "@/components/ui/input";
import type { useActivityMonetaryBudget } from "@/hooks/use-activities";
import {
  formatMicrousd,
  MAX_MONETARY_MICROUSD,
  MAX_OUTPUT_TOKENS_PER_TURN,
  monetaryBudgetApi,
  remainingMicrousd,
  validateMonetaryDraft,
  type MonetaryBudgetDraft,
} from "@/lib/monetary-budget";

export function ActivityMonetaryBudgetPanel({
  activityId, ownerUid, editable, disabled, view, mutate,
}: {
  activityId: string;
  ownerUid: number;
  editable: boolean;
  disabled: boolean;
  view: ReturnType<typeof useActivityMonetaryBudget>;
  mutate: (action: () => Promise<unknown>, message: string) => Promise<boolean>;
}) {
  const [editing, setEditing] = useState(false);
  const [revision, setRevision] = useState<string | null>(null);
  const [total, setTotal] = useState("5000000");
  const [inputRate, setInputRate] = useState("250000");
  const [outputRate, setOutputRate] = useState("1000000");
  const [outputLimit, setOutputLimit] = useState("4096");
  const policy = view.data?.monetary_budget;
  const identityError = policy && policy.owner_uid !== ownerUid
    ? "Monetary-budget owner does not match this Activity." : null;
  const error = view.error || identityError;
  const blocked = disabled || !!error || !view.data;
  const draft: MonetaryBudgetDraft = {
    currency: "USD",
    max_total_microusd: total,
    input_microusd_per_million_tokens: inputRate,
    output_microusd_per_million_tokens: outputRate,
    max_output_tokens_per_turn: Number(outputLimit),
  };
  const valid = validateMonetaryDraft(draft);

  function edit() {
    setRevision(policy?.revision ?? null);
    setTotal(policy?.budget.max_total_microusd ?? "5000000");
    setInputRate(policy?.budget.input_microusd_per_million_tokens ?? "250000");
    setOutputRate(policy?.budget.output_microusd_per_million_tokens ?? "1000000");
    setOutputLimit(String(policy?.budget.max_output_tokens_per_turn ?? 4096));
    setEditing(true);
  }

  return (
    <Card role="region" aria-label="Activity monetary budget" className="min-w-0 gap-3 p-4">
      <div className="flex items-center justify-between gap-3">
        <h3 className="text-sm font-medium">Monetary budget</h3>
        <Button size="sm" variant="outline" aria-label="Refresh monetary budget"
          disabled={disabled || view.loading} onClick={() => void view.refresh()}>Refresh</Button>
      </div>
      <p className="text-xs text-muted-foreground">
        Configured USD accounting constrains model turns. These owner-selected rates are not provider
        prices, invoice data, or billing reconciliation, and they grant no permission.
      </p>
      {error ? <p role="alert" className="text-sm text-destructive">{error} Budget data is hidden until refresh succeeds.</p>
        : !view.data ? <p className="text-sm text-muted-foreground">Loading monetary budget...</p>
        : policy ? (
          <>
            <dl className="grid gap-1 text-sm">
              <div><dt className="inline font-medium">Policy: </dt><dd className="inline">{policy.enabled ? "Enabled" : "Disabled"}</dd></div>
              <div><dt className="inline font-medium">Configured accounting total: </dt><dd className="inline">{formatMicrousd(policy.budget.max_total_microusd)}</dd></div>
              <div><dt className="inline font-medium">Spent (configured accounting): </dt><dd className="inline">{formatMicrousd(policy.spent_microusd)}</dd></div>
              <div><dt className="inline font-medium">Reserved (configured accounting): </dt><dd className="inline">{formatMicrousd(policy.reserved_microusd)}</dd></div>
              <div><dt className="inline font-medium">Remaining (configured accounting): </dt><dd className="inline">{formatMicrousd(remainingMicrousd(policy))}</dd></div>
              <div><dt className="inline font-medium">Configured input rate: </dt><dd className="inline">{policy.budget.input_microusd_per_million_tokens} micro-USD / 1,000,000 tokens</dd></div>
              <div><dt className="inline font-medium">Configured output rate: </dt><dd className="inline">{policy.budget.output_microusd_per_million_tokens} micro-USD / 1,000,000 tokens</dd></div>
              <div><dt className="inline font-medium">Maximum output per turn: </dt><dd className="inline">{policy.budget.max_output_tokens_per_turn} tokens</dd></div>
              <div><dt className="inline font-medium">Revision: </dt><dd className="inline">{policy.revision}</dd></div>
            </dl>
            {remainingMicrousd(policy) === 0n
              && <p className="text-xs text-muted-foreground">No configured accounting balance remains. Actual settlement may exceed the configured total; it is never clamped for presentation.</p>}
            <div className="flex flex-wrap gap-2">
              <Button size="sm" variant="outline" disabled={blocked || !editable || editing} onClick={edit}>Edit monetary budget</Button>
              <Button size="sm" variant="outline" disabled={blocked || (!policy.enabled && !editable)}
                onClick={() => void mutate(
                  () => monetaryBudgetApi.enable(activityId, ownerUid, policy, !policy.enabled),
                  policy.enabled
                    ? "Activity monetary budget disabled; configured accounting and ledger totals are retained."
                    : "Activity monetary budget explicitly enabled with the existing configured accounting.",
                )}>{policy.enabled ? "Disable monetary budget" : "Enable monetary budget"}</Button>
            </div>
          </>
        ) : (
          <>
            <p className="text-sm text-muted-foreground">
              No monetary budget is configured. Existing model behavior is unchanged.
            </p>
            <Button className="w-fit" size="sm" variant="outline"
              disabled={blocked || !editable || editing} onClick={edit}>
              Configure monetary budget
            </Button>
          </>
        )}
      <p className="text-xs text-muted-foreground">
        Reservations are conservative upper bounds. Provider errors, missing usage, retries, fallback
        ambiguity, or crashes can retain charges or reservations. This is not a provider invoice.
      </p>
      {editing && editable && (
        <form className="grid gap-3 border-t pt-3" onSubmit={(event) => {
          event.preventDefault();
          if (blocked || !valid) return;
          void mutate(
            () => monetaryBudgetApi.set(activityId, ownerUid, revision, draft, policy ?? undefined),
            "Activity monetary budget saved with exact revision; spent and reserved accounting were not reset.",
          ).then((saved) => { if (saved) setEditing(false); });
        }}>
          <p className="text-xs text-muted-foreground">
            {revision === null
              ? "Create an enabled USD configured-accounting policy at revision 1."
              : `Editing revision ${revision}; stale changes are refused and never rebased automatically.`}
          </p>
          <fieldset disabled={blocked} className="grid gap-3 sm:grid-cols-2">
            <label className="grid gap-1.5 text-sm">
              Currency
              <Input value="USD" readOnly aria-readonly="true" />
            </label>
            <label className="grid gap-1.5 text-sm">
              Maximum configured total (micro-USD)
              <Input inputMode="numeric" required value={total}
                onChange={(event) => setTotal(event.target.value)} />
            </label>
            <label className="grid gap-1.5 text-sm">
              Configured input rate (micro-USD per million tokens)
              <Input inputMode="numeric" required value={inputRate}
                onChange={(event) => setInputRate(event.target.value)} />
            </label>
            <label className="grid gap-1.5 text-sm">
              Configured output rate (micro-USD per million tokens)
              <Input inputMode="numeric" required value={outputRate}
                onChange={(event) => setOutputRate(event.target.value)} />
            </label>
            <label className="grid gap-1.5 text-sm sm:col-span-2">
              Maximum output tokens per turn
              <Input type="number" min={1} max={MAX_OUTPUT_TOKENS_PER_TURN} required
                value={outputLimit} onChange={(event) => setOutputLimit(event.target.value)} />
            </label>
            <p className="text-xs text-muted-foreground sm:col-span-2">
              Each micro-USD amount/rate must be a whole number from 1 to {MAX_MONETARY_MICROUSD.toString()}.
              Updates preserve spent, reserved, enabled state, and creation identity.
            </p>
            <div className="flex gap-2 sm:col-span-2">
              <Button type="submit" disabled={!valid}>Save monetary budget</Button>
              <Button type="button" variant="outline" onClick={() => setEditing(false)}>Cancel budget edit</Button>
            </div>
          </fieldset>
        </form>
      )}
    </Card>
  );
}
