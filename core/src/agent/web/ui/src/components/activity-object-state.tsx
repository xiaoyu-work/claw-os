import { useEffect, useRef, useState } from "react";

import { Button } from "@/components/ui/button";
import { Card } from "@/components/ui/card";
import { Input } from "@/components/ui/input";
import { Textarea } from "@/components/ui/textarea";
import { activityError, type useActivityObjectState } from "@/hooks/use-activities";
import { activityApi, type ActivityResource } from "@/lib/activities";
import { oneOf } from "@/lib/api-shapes";
import {
  objectKindLabels, RELATION_KINDS, validityLabels,
  type ObjectRelationKind, type ObjectStateContent, type ObjectStateDraft, type ObjectStateEntry,
} from "@/lib/object-state";

const KINDS = ["user_statement", "agent_inference", "app_report", "relation", "retracted"] as const;
const selectClass = "h-9 min-w-0 rounded-md border bg-background px-3 text-sm";

export function ActivityObjectStatePanel({
  activityId, ownerUid, resources, view, disabled,
}: {
  activityId: string;
  ownerUid: number;
  resources: ActivityResource[];
  view: ReturnType<typeof useActivityObjectState>;
  disabled: boolean;
}) {
  const references = resources.filter((resource) => resource.reference.startsWith("app://"));
  const [reference, setReference] = useState("");
  const [kind, setKind] = useState<ObjectStateContent["kind"]>("user_statement");
  const [text, setText] = useState("");
  const [receiptId, setReceiptId] = useState("");
  const [target, setTarget] = useState("");
  const [relation, setRelation] = useState<ObjectRelationKind>("related_to");
  const [observedAt, setObservedAt] = useState("");
  const [validUntil, setValidUntil] = useState("");
  const [supersedes, setSupersedes] = useState<string | null>(null);
  const [history, setHistory] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const mounted = useRef(true);
  const pending = useRef(false);
  const retry = useRef<{ key: string; draft: ObjectStateDraft } | null>(null);
  useEffect(() => {
    mounted.current = true;
    return () => { mounted.current = false; };
  }, []);
  const identityError = view.data?.entries.some((entry) => entry.owner_uid !== ownerUid)
    ? "Object-state owner does not match this Activity." : null;
  const readError = view.error || identityError;
  const blocked = disabled || busy || !!readError;
  const hasReference = references.some((resource) => resource.reference === reference);
  const observation = kind !== "relation" && kind !== "retracted";
  const complete = hasReference && (kind === "app_report" ? !!receiptId.trim()
    : kind === "relation" ? !!target && target !== reference
    : !!text.trim()) && (kind !== "retracted" || supersedes !== null);
  const entries = readError ? [] : view.data?.entries ?? [];

  function reset() {
    setKind("user_statement");
    setText("");
    setReceiptId("");
    setTarget("");
    setObservedAt("");
    setValidUntil("");
    setSupersedes(null);
    setError(null);
    retry.current = null;
  }

  function revise(entry: ObjectStateEntry, retract: boolean) {
    const content = entry.draft.content;
    setReference(entry.draft.reference);
    setSupersedes(entry.id);
    setKind(retract ? "retracted" : content.kind);
    setText(retract ? "" : content.kind === "user_statement" || content.kind === "agent_inference"
      ? content.text : content.kind === "relation" ? content.note : "");
    setReceiptId(content.kind === "app_report" ? content.receipt_id : "");
    setTarget(content.kind === "relation" ? content.target : "");
    setRelation(content.kind === "relation" ? content.relation : "related_to");
    setObservedAt(retract ? "" : entry.draft.observed_at ?? "");
    setValidUntil(retract ? "" : entry.draft.valid_until ?? "");
    setError(null);
    setNotice(null);
    retry.current = null;
  }

  async function submit() {
    if (blocked || !complete || pending.current) return;
    let content: ObjectStateContent;
    switch (kind) {
      case "user_statement": content = { kind, text }; break;
      case "agent_inference": content = { kind, text }; break;
      case "app_report": content = { kind, receipt_id: receiptId.trim() }; break;
      case "relation": content = { kind, relation, target, note: text }; break;
      case "retracted": content = { kind, reason: text }; break;
    }
    const fields = {
      reference, content, supersedes,
      observed_at: observation && observedAt ? observedAt : null,
      valid_until: observation && validUntil ? validUntil : null,
    };
    const key = JSON.stringify(fields);
    if (retry.current?.key !== key) {
      retry.current = { key, draft: { id: crypto.randomUUID(), ...fields } };
    }
    const draft = retry.current.draft;
    pending.current = true;
    setBusy(true);
    setError(null);
    setNotice(null);
    try {
      const recorded = await activityApi.recordObjectState(activityId, draft);
      if (!mounted.current) return;
      if (recorded.owner_uid !== ownerUid) throw new Error("Object-state acknowledgement has another owner.");
      reset();
      setNotice("Entry recorded. No App data was fetched and no goal or permission was changed.");
      await view.refresh();
    } catch (cause) {
      if (mounted.current) {
        setError(`${activityError(cause)} Keep entry ID ${draft.id} when retrying this submission.`);
      }
    } finally {
      pending.current = false;
      if (mounted.current) setBusy(false);
    }
  }

  return (
    <Card role="region" aria-label="Object state and history" className="min-w-0 gap-4 p-4">
      <div className="flex items-center justify-between gap-3">
        <h3 className="text-sm font-medium">Object state and history</h3>
        <Button size="sm" variant="outline" aria-label="Refresh object state"
          disabled={view.loading || busy} onClick={() => void view.refresh()}>Refresh</Button>
      </div>
      <p className="text-xs text-muted-foreground">
        Caller-reported annotations, not verified facts or authority. Source classifications do not
        prove authorship. A linked App receipt does not prove its result concerns this object.
        Reported time windows do not establish freshness. App data stays with its App.
      </p>
      {readError && <p role="alert" className="text-sm text-destructive">{readError} History is hidden until a successful refresh.</p>}
      {!readError && (
        <>
          <label className="flex items-center gap-2 text-xs">
            <input type="checkbox" checked={history} onChange={(event) => setHistory(event.target.checked)} />
            Show superseded history
          </label>
          {!view.data && view.loading && <p className="text-sm text-muted-foreground">Loading object state...</p>}
          {view.data && !entries.length && <p className="text-sm text-muted-foreground">No object-state entries recorded.</p>}
          {entries.filter((entry) => history || entry.superseded_by === null).map((entry) => (
            <article key={entry.id} aria-label={`Object state: ${entry.id}`} className="grid min-w-0 gap-2 rounded-md border p-3">
              <div className="flex flex-wrap gap-2 text-xs">
                <span className="rounded-full border px-2 py-0.5">{objectKindLabels[entry.draft.content.kind]}</span>
                <span className="rounded-full border px-2 py-0.5">{validityLabels[entry.validity]}</span>
              </div>
              <code className="break-all text-xs">{entry.draft.reference}</code>
              <EntryContent entry={entry} />
              <p className="text-xs text-muted-foreground">
                Recorded <time dateTime={entry.recorded_at}>{new Date(entry.recorded_at).toLocaleString()}</time>
              </p>
              {entry.draft.observed_at && entry.draft.valid_until && (
                <p className="break-words text-xs text-muted-foreground">
                  Reported window: {entry.draft.observed_at} to {entry.draft.valid_until}
                </p>
              )}
              <code className="break-all text-[11px] text-muted-foreground">{entry.id}</code>
              {entry.draft.supersedes && <p className="break-all text-xs">Supersedes {entry.draft.supersedes}</p>}
              {entry.superseded_by ? (
                <p className="break-all text-xs text-muted-foreground">Superseded by {entry.superseded_by}</p>
              ) : entry.draft.content.kind !== "retracted" && (
                <div className="flex gap-2">
                  <Button size="sm" variant="outline" disabled={blocked} onClick={() => revise(entry, false)}>Correct entry</Button>
                  <Button size="sm" variant="ghost" disabled={blocked} onClick={() => revise(entry, true)}>Retract entry</Button>
                </div>
              )}
            </article>
          ))}
        </>
      )}
      <form className="grid gap-3 border-t pt-4" onSubmit={(event) => { event.preventDefault(); void submit(); }}>
        <h4 className="text-sm font-medium">{supersedes ? "Revise an entry (history is preserved)" : "Add object-state entry"}</h4>
        {!references.length && <p className="text-xs text-muted-foreground">Attach an App object reference first.</p>}
        {supersedes && !hasReference && <p className="text-xs text-muted-foreground">Reattach this resource before revising its history.</p>}
        {error && <p role="alert" className="whitespace-pre-wrap text-sm text-destructive">{error}</p>}
        {notice && <p role="status" className="text-sm text-muted-foreground">{notice}</p>}
        <fieldset disabled={blocked} className="grid min-w-0 gap-3 sm:grid-cols-2">
          <label className="grid gap-1.5 text-sm sm:col-span-2">
            Object reference
            <select className={selectClass} value={reference} disabled={supersedes !== null}
              onChange={(event) => setReference(event.target.value)}>
              <option value="">Select an attached App reference</option>
              {references.map((resource) => <option key={resource.reference} value={resource.reference}>{resource.label}: {resource.reference}</option>)}
              {reference && !hasReference && <option value={reference}>{reference} (detached)</option>}
            </select>
          </label>
          <label className="grid gap-1.5 text-sm sm:col-span-2">
            Reported source or entry kind
            <select className={selectClass} value={kind} onChange={(event) => {
              if (oneOf(KINDS, event.target.value)) setKind(event.target.value);
            }}>
              {KINDS.filter((value) => value !== "retracted" || supersedes !== null).map((value) => (
                <option key={value} value={value}>{objectKindLabels[value]}</option>
              ))}
            </select>
          </label>
          {kind === "app_report" ? (
            <label className="grid gap-1.5 text-sm sm:col-span-2">
              Existing Activity receipt ID
              <Input required value={receiptId} onChange={(event) => setReceiptId(event.target.value)} />
            </label>
          ) : (
            <label className="grid gap-1.5 text-sm sm:col-span-2">
              {kind === "retracted" ? "Retraction reason" : kind === "relation" ? "Relationship note (optional)" : "Statement or inference"}
              <Textarea required={kind !== "relation"} maxLength={kind === "relation" ? 2048 : 4096}
                value={text} onChange={(event) => setText(event.target.value)} />
            </label>
          )}
          {kind === "relation" && (
            <>
              <label className="grid gap-1.5 text-sm">
                Relationship
                <select className={selectClass} value={relation} onChange={(event) => {
                  if (oneOf(RELATION_KINDS, event.target.value)) setRelation(event.target.value);
                }}>{RELATION_KINDS.map((value) => <option key={value} value={value}>{value}</option>)}</select>
              </label>
              <label className="grid gap-1.5 text-sm">
                Related object
                <select className={selectClass} value={target} onChange={(event) => setTarget(event.target.value)}>
                  <option value="">Select another reference</option>
                  {references.filter((resource) => resource.reference !== reference).map((resource) => (
                    <option key={resource.reference} value={resource.reference}>{resource.label}: {resource.reference}</option>
                  ))}
                </select>
              </label>
            </>
          )}
          {observation && (
            <>
              <label className="grid gap-1.5 text-sm">
                Reported window start (optional RFC3339)
                <Input value={observedAt} placeholder="2026-09-11T10:00:00Z" onChange={(event) => setObservedAt(event.target.value)} />
              </label>
              <label className="grid gap-1.5 text-sm">
                Reported window end (optional RFC3339)
                <Input value={validUntil} placeholder="2026-09-12T10:00:00Z" onChange={(event) => setValidUntil(event.target.value)} />
              </label>
            </>
          )}
          <div className="flex gap-2 sm:col-span-2">
            <Button type="submit" disabled={!complete}>Record object-state entry</Button>
            <Button type="button" variant="outline" onClick={reset}>Reset entry form</Button>
          </div>
        </fieldset>
      </form>
    </Card>
  );
}

function EntryContent({ entry }: { entry: ObjectStateEntry }) {
  const content = entry.draft.content;
  if (content.kind === "app_report") {
    return <div className="grid gap-1 text-sm">
      <p className="break-all text-xs">Receipt {content.receipt_id}: {entry.receipt?.outcome}</p>
      <p className="text-xs text-muted-foreground">Caller-reported App output, not an OS execution attestation.</p>
      {entry.receipt?.result && <pre className="whitespace-pre-wrap break-words">{entry.receipt.result.preview}</pre>}
      {entry.receipt?.result?.preview_truncated && <p className="text-xs">Receipt preview truncated.</p>}
      {entry.receipt?.error && <p className="whitespace-pre-wrap break-words">{entry.receipt.error}</p>}
    </div>;
  }
  if (content.kind === "relation") {
    return <div className="grid gap-1 text-sm">
      <p className="break-all">{content.relation}: {content.target}</p>
      <p className="whitespace-pre-wrap break-words">{content.note}</p>
      <p className="text-xs text-muted-foreground">Planning link only; it schedules or authorizes nothing.</p>
    </div>;
  }
  return <p className="whitespace-pre-wrap break-words text-sm">
    {content.kind === "retracted" ? content.reason : content.text}
  </p>;
}
