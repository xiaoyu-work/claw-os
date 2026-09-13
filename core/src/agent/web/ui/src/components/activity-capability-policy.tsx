import { useEffect, useRef, useState } from "react";

import { Button } from "@/components/ui/button";
import { Card } from "@/components/ui/card";
import { Input } from "@/components/ui/input";
import { activityError, type useActivityCapabilityPolicy } from "@/hooks/use-activities";
import {
  CAPABILITY_POLICY_MODES, MAX_POLICY_RULES, MAX_POLICY_SCOPES, MAX_POLICY_BYTES,
  capabilityPolicyApi, capabilityPolicyModeLabels, capabilityScopeKinds, emptyCapabilityScope,
  readCapabilityPolicyDraft,
  type CapabilityPolicyCatalog, type CapabilityPolicyExpectation, type CapabilityPolicyRule,
} from "@/lib/capability-policy";

const selectClass = "h-9 min-w-0 rounded-md border bg-background px-3 text-sm";
type RuleRow = { key: number; rule: CapabilityPolicyRule };

export function ActivityCapabilityPolicyPanel({
  activityId, ownerUid, editable, disabled, view, mutate,
}: {
  activityId: string;
  ownerUid: number;
  editable: boolean;
  disabled: boolean;
  view: ReturnType<typeof useActivityCapabilityPolicy>;
  mutate: (action: () => Promise<unknown>, message: string) => Promise<boolean>;
}) {
  const [editing, setEditing] = useState(false);
  const [expected, setExpected] = useState<CapabilityPolicyExpectation>({ revision: null, enabled: true, ownerUid });
  const [rows, setRows] = useState<RuleRow[]>([]);
  const [writeError, setWriteError] = useState<string | null>(null);
  const [needsRefresh, setNeedsRefresh] = useState(false);
  const [refreshed, setRefreshed] = useState<CapabilityPolicyExpectation | null>(null);
  const nextKey = useRef(0);
  const pending = useRef(false);
  const mounted = useRef(true);
  useEffect(() => {
    mounted.current = true;
    return () => { mounted.current = false; };
  }, []);

  const data = view.data;
  const policy = data?.capability_policy;
  const identityError = data && (data.activity_id !== activityId || (policy && policy.activity_id !== activityId))
    ? "Capability-policy response does not match the selected Activity."
    : policy && policy.owner_uid !== ownerUid ? "Capability-policy owner does not match this Activity." : null;
  const error = view.error || identityError;
  const readBlocked = disabled || view.loading || !!error || !view.data;
  const currentRevision = policy?.revision ?? null;
  const stale = editing && !!view.data && expected.revision !== currentRevision;
  const blocked = readBlocked || needsRefresh || stale;
  const canRebase = !!refreshed && refreshed.revision === currentRevision
    && refreshed.enabled === (policy?.enabled ?? true) && !readBlocked && !needsRefresh;
  const draft = { rules: rows.map((row) => row.rule) };
  let draftError: string | null = null;
  if (view.data) {
    try {
      readCapabilityPolicyDraft(draft, view.data.catalog);
    } catch (cause) {
      draftError = activityError(cause);
    }
  }

  function edit() {
    if (blocked || !editable) return;
    setExpected({ revision: currentRevision, enabled: policy?.enabled ?? true, ownerUid });
    setRows((policy?.rules ?? []).map((rule) => ({
      key: nextKey.current++, rule: { ...rule, scopes: rule.scopes.map((scope) => ({ ...scope })) },
    })));
    setRefreshed(null);
    setWriteError(null);
    setEditing(true);
  }

  async function refresh() {
    const fresh = await view.refresh();
    if (!mounted.current || !fresh || (fresh.capability_policy && fresh.capability_policy.owner_uid !== ownerUid)) return;
    setNeedsRefresh(false);
    setWriteError(null);
    setRefreshed({
      revision: fresh.capability_policy?.revision ?? null,
      enabled: fresh.capability_policy?.enabled ?? true, ownerUid,
    });
  }

  async function write(action: () => Promise<unknown>, message: string, closeEdit = false) {
    if (pending.current || blocked) return;
    pending.current = true;
    let failure: string | null = null;
    try {
      const saved = await mutate(async () => {
        try {
          return await action();
        } catch (cause) {
          failure = activityError(cause);
          throw cause;
        }
      }, message);
      if (!mounted.current) return;
      if (saved) {
        setWriteError(null);
        setRefreshed(null);
        if (closeEdit) setEditing(false);
      } else {
        setWriteError(failure ?? "The policy change could not be confirmed.");
        setNeedsRefresh(true);
        setRefreshed(null);
      }
    } finally {
      pending.current = false;
    }
  }

  return (
    <Card role="region" aria-label="Activity capability policy" className="min-w-0 gap-3 p-4">
      <div className="flex items-center justify-between gap-3">
        <h3 className="text-sm font-medium">Capability policy</h3>
        <Button size="sm" variant="outline" aria-label="Refresh capability policy"
          disabled={disabled || view.loading} onClick={() => void refresh()}>Refresh</Button>
      </div>
      <p className="text-xs text-muted-foreground">
        These rules constrain capabilities; they never grant permission. Normal keeps existing permission
        and provenance checks. Ask requires exact, single-use approval even for held capabilities.
        Broader scopes do not satisfy Ask; Ask approvals are used once even if consent said session or forever.
        Deny cannot be overridden by approval.
      </p>
      {error ? (
        <p role="alert" className="text-sm text-destructive">{error} Saved rules are hidden until refresh succeeds.</p>
      ) : !view.data ? (
        <p className="text-sm text-muted-foreground">Loading capability policy...</p>
      ) : policy ? (
        <>
          <dl className="grid gap-1 text-sm">
            <div><dt className="inline font-medium">Policy: </dt><dd className="inline">{policy.enabled ? "Enabled" : "Disabled"}</dd></div>
            <div><dt className="inline font-medium">Revision: </dt><dd className="inline">{policy.revision}</dd></div>
            <div><dt className="inline font-medium">Updated: </dt><dd className="inline break-all"><time dateTime={policy.updated_at}>{policy.updated_at}</time></dd></div>
          </dl>
          {!policy.enabled && <p className="text-sm font-medium">Disabled: all controlled capability checks are blocked, not unrestricted.</p>}
          {policy.rules.length ? (
            <div className="grid gap-2" aria-label="Saved capability rules">
              {policy.rules.map((rule) => (
                <article key={rule.verb} className="grid min-w-0 gap-1 rounded-md border p-3">
                  <h4 className="break-words text-sm font-medium">{rule.verb} — {capabilityPolicyModeLabels[rule.mode]}</h4>
                  {rule.mode === "deny" ? <p className="text-xs">Denied for every scope of this verb.</p>
                    : <ul className="grid gap-1 text-xs">{rule.scopes.map((scope, index) => (
                      <li key={index}><code className="whitespace-pre-wrap break-all">
                        {scope.kind === "wild" ? "wild (unscoped / explicit wildcard)" : `${scope.kind}: ${scope.value}`}
                      </code></li>
                    ))}</ul>}
                </article>
              ))}
            </div>
          ) : <p className="text-sm text-muted-foreground">No rules. An enabled empty policy adds no constraints and grants no permissions.</p>}
          <div className="flex flex-wrap gap-2">
            <Button size="sm" variant="outline" disabled={blocked || !editable || editing} onClick={edit}>Edit capability policy</Button>
            <Button size="sm" variant="outline" disabled={blocked || editing || (!policy.enabled && !editable)}
              onClick={() => {
                if (!data) return;
                void write(
                  () => capabilityPolicyApi.enable(activityId, policy, !policy.enabled, data.catalog),
                  policy.enabled ? "Activity capability policy disabled; controlled capability checks are blocked."
                    : "Activity capability policy explicitly enabled; ordinary permission checks still apply.",
                );
              }}>{policy.enabled ? "Disable capability policy" : "Enable capability policy"}</Button>
          </div>
        </>
      ) : (
        <>
          <p className="text-sm text-muted-foreground">No Activity capability policy is configured. Existing permission checks remain in force.</p>
          <Button className="w-fit" size="sm" variant="outline" disabled={blocked || !editable || editing} onClick={edit}>
            Configure capability policy
          </Button>
        </>
      )}
      <p className="text-xs text-muted-foreground">
        Missing rules mean Normal. A scoped Normal or Ask rule requires the entire requested capability
        to be covered by at least one listed scope; otherwise it is denied. App approval is settled
        all-or-none once per one-shot invocation or stateful tool call. Later effects recheck the
        immutable root-grant policy binding without asking again.
      </p>
      <p className="text-xs text-muted-foreground">
        Editing preserves enabled state. Policy changes stop old-policy attempts through normal cleanup,
        without undoing effects already admitted. A first policy also invalidates existing attempts.
        Denying fs.delete alone does not protect content against fs.write or proc.exec.
      </p>
      <p className="text-xs text-muted-foreground">
        This is not process-wide kernel protection against a compromised same-UID Agent.
        The exact App sandbox and broker remain authoritative.
      </p>
      {writeError && <p role="alert" className="text-sm text-destructive">{writeError} Refresh explicitly before trying again. Your draft is retained.</p>}
      {editing && (
        <form aria-label="Edit capability rules" className="grid gap-3 border-t pt-3" onSubmit={(event) => {
          event.preventDefault();
          if (blocked || !editable || draftError || !view.data) return;
          const catalog = view.data.catalog;
          void write(
            () => capabilityPolicyApi.set(activityId, expected, draft, catalog),
            "Activity capability policy saved; no permissions were granted.", true,
          );
        }}>
          <p className="text-xs text-muted-foreground">
            {expected.revision === null ? "Creating revision 1, initially enabled." : `Editing revision ${expected.revision}; saving will preserve its enabled state.`}
            {" "}At most 64 unique capability verbs, 32 scopes per rule and 16 KiB per draft.
          </p>
          {!editable && <p role="alert" className="text-sm text-muted-foreground">Reopen this Activity to save. Your unsaved policy draft is retained.</p>}
          {stale && (
            <div className="grid gap-2">
              <p role="alert" className="text-sm text-destructive">
                The saved policy changed while this draft was open. Refresh explicitly, compare the saved
                rules above, then choose whether to use the refreshed revision. No change is retried automatically.
              </p>
              <Button type="button" className="w-fit" size="sm" variant="outline" disabled={!canRebase || !editable}
                onClick={() => {
                  if (canRebase && refreshed) {
                    setExpected(refreshed);
                    setRefreshed(null);
                  }
                }}>Use refreshed revision for this draft</Button>
            </div>
          )}
          <fieldset disabled={readBlocked || !editable} className="grid min-w-0 gap-3">
            {data && rows.map((row, index) => (
              <CapabilityRuleEditor key={row.key} rule={row.rule} index={index} catalog={data.catalog}
                used={rows.filter((other) => other.key !== row.key).map((other) => other.rule.verb)}
                onChange={(rule) => setRows((current) => current.map((item) => item.key === row.key ? { ...item, rule } : item))}
                onRemove={() => setRows((current) => current.filter((item) => item.key !== row.key))} />
            ))}
            {!rows.length && <p className="text-sm text-muted-foreground">Empty rules add no constraints, not permission. Add a rule to constrain a capability.</p>}
            <Button type="button" className="w-fit" size="sm" variant="outline"
              disabled={rows.length >= MAX_POLICY_RULES || !view.data?.catalog.verbs.some((verb) => !rows.some((row) => row.rule.verb === verb.verb))}
              onClick={() => setRows((current) => [...current, {
                key: nextKey.current++, rule: { verb: "", mode: "normal", scopes: [] },
              }])}>Add capability rule</Button>
          </fieldset>
          {draftError && <p role="alert" className="text-sm text-destructive">{draftError}</p>}
          <div className="flex flex-wrap gap-2">
            <Button type="submit" disabled={blocked || !editable || !!draftError}>Save capability policy</Button>
            <Button type="button" variant="outline" disabled={disabled}
              onClick={() => { setEditing(false); setRows([]); }}>Cancel policy edit</Button>
          </div>
        </form>
      )}
    </Card>
  );
}

function CapabilityRuleEditor({ rule, index, catalog, used, onChange, onRemove }: {
  rule: CapabilityPolicyRule;
  index: number;
  catalog: CapabilityPolicyCatalog;
  used: string[];
  onChange: (rule: CapabilityPolicyRule) => void;
  onRemove: () => void;
}) {
  const verb = catalog.verbs.find((entry) => entry.verb === rule.verb);
  return (
    <fieldset className="grid min-w-0 gap-3 rounded-md border p-3">
      <legend className="px-1 text-sm font-medium">Capability rule {index + 1}</legend>
      <div className="grid min-w-0 gap-3 sm:grid-cols-2">
        <label className="grid min-w-0 gap-1.5 text-sm">
          Rule {index + 1} capability
          <select className={selectClass} value={rule.verb} required onChange={(event) => {
            const selected = catalog.verbs.find((entry) => entry.verb === event.target.value);
            onChange({ ...rule, verb: selected?.verb ?? "", scopes: selected && rule.mode !== "deny" ? [emptyCapabilityScope(selected)] : [] });
          }}>
            <option value="">Choose a capability</option>
            {catalog.verbs.map((entry) => <option key={entry.verb} value={entry.verb} disabled={used.includes(entry.verb)}>{entry.verb} — {entry.label}</option>)}
          </select>
        </label>
        <label className="grid min-w-0 gap-1.5 text-sm">
          Rule {index + 1} mode
          <select className={selectClass} value={rule.mode} onChange={(event) => {
            const mode = CAPABILITY_POLICY_MODES.find((value) => value === event.target.value);
            if (mode) onChange({
              ...rule, mode, scopes: mode === "deny" ? [] : rule.scopes.length ? rule.scopes : verb ? [emptyCapabilityScope(verb)] : [],
            });
          }}>
            {CAPABILITY_POLICY_MODES.map((mode) => <option key={mode} value={mode}>{capabilityPolicyModeLabels[mode]}</option>)}
          </select>
        </label>
      </div>
      {verb && <p className="text-xs text-muted-foreground">{verb.description}</p>}
      {verb?.scope_kind === "path" && rule.mode !== "deny" && (
        <p className="text-xs text-muted-foreground">Use canonical absolute Linux paths, without ~, $ placeholders or dot segments. The broker validates the saved scopes.</p>
      )}
      {rule.mode === "deny" ? <p className="text-xs">Deny the entire verb. No approval can override this rule.</p>
        : verb && (
          <>
            {rule.scopes.map((scope, scopeIndex) => (
              <div key={scopeIndex} className="grid min-w-0 gap-2">
                {capabilityScopeKinds(verb).length > 1 && (
                  <label className="grid gap-1.5 text-sm">
                    Rule {index + 1} scope {scopeIndex + 1} kind
                    <select className={selectClass} value={scope.kind} onChange={(event) => {
                      const kind = capabilityScopeKinds(verb).find((value) => value === event.target.value);
                      if (kind) onChange({ ...rule, scopes: rule.scopes.map((item, position) => position === scopeIndex
                        ? kind === "wild" ? { kind } : { kind, value: "" } : item) });
                    }}>{capabilityScopeKinds(verb).map((kind) => <option key={kind} value={kind}>{kind}</option>)}</select>
                  </label>
                )}
                {scope.kind === "wild" ? <p className="text-xs">Explicit wildcard / unscoped capability. No permission is granted.</p> : (
                  <label className="grid min-w-0 gap-1.5 text-sm">
                    Rule {index + 1} {scope.kind} scope {scopeIndex + 1}
                    <Input required maxLength={MAX_POLICY_BYTES} value={scope.value} onChange={(event) => onChange({
                      ...rule, scopes: rule.scopes.map((item, position) => position === scopeIndex ? { ...scope, value: event.target.value } : item),
                    })} />
                  </label>
                )}
                <Button type="button" className="w-fit" size="sm" variant="ghost"
                  aria-label={`Remove scope ${scopeIndex + 1} from rule ${index + 1}`}
                  onClick={() => onChange({ ...rule, scopes: rule.scopes.filter((_, position) => position !== scopeIndex) })}>Remove scope</Button>
              </div>
            ))}
            <Button type="button" className="w-fit" size="sm" variant="outline" disabled={rule.scopes.length >= MAX_POLICY_SCOPES}
              onClick={() => onChange({ ...rule, scopes: [...rule.scopes, emptyCapabilityScope(verb)] })}>Add scope to rule {index + 1}</Button>
          </>
        )}
      <Button type="button" className="w-fit" size="sm" variant="ghost" onClick={onRemove}>Remove capability rule {index + 1}</Button>
    </fieldset>
  );
}
