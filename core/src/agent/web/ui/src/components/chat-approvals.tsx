import { useCallback, useEffect, useRef, useState } from "react";
import { api, ApiError } from "@/lib/api";
import { APPROVAL_LABELS, readApprovalBundle, readAuthorization, type ApprovalBundle, type ApprovalChoice, type ApprovalView, type AuthorizationView } from "@/lib/approvals";
import { Button } from "./ui/button";
import { Card } from "./ui/card";
import { OsAuthorizationDialog } from "./os-authorization";

export function ApprovalCard({ review, onChanged, readOnly = false }: {
  review: ApprovalView; onChanged: () => void; readOnly?: boolean;
}) {
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [authorization, setAuthorization] = useState<AuthorizationView | null>(null);
  const activeAuthorization = useRef<string | null>(null);
  useEffect(() => {
    setError(null);
  }, [review.id, review.revision]);
  useEffect(() => () => {
    const id = activeAuthorization.current;
    if (id) void api.post(`/api/authorizations/${id}/cancel`).catch(() => {
      console.warn("OS authorization cancellation was not confirmed; inspect the request outcome.");
    });
  }, [review.id]);
  const finishAuthorization = (failure?: string) => {
    const id = activeAuthorization.current;
    activeAuthorization.current = null;
    setAuthorization(null);
    setBusy(false);
    setError(failure || null);
    if (failure && id) void api.post(`/api/authorizations/${id}/cancel`).catch(() => {
      console.warn("OS authorization cancellation was not confirmed; inspect the request outcome.");
    });
    onChanged();
  };
  const decide = async (selected: ApprovalChoice) => {
    const displayedRevision = review.revision;
    setBusy(true);
    setError(null);
    try {
      if (selected === "deny") {
        await api.post(`/api/approvals/${encodeURIComponent(review.id)}/decision`, {
          revision: displayedRevision, choice: selected,
        });
        onChanged();
      } else {
        const value: unknown = await api.post(`/api/approvals/${encodeURIComponent(review.id)}/authorization`, {
          revision: displayedRevision, choice: selected,
        }, { signal: AbortSignal.timeout(10_000) });
        const session = readAuthorization(value, { review_id: review.id, revision: displayedRevision });
        if (session.choice !== selected) throw new Error("The OS returned a different authorization choice.");
        activeAuthorization.current = session.id;
        setAuthorization(session);
      }
    } catch (failure: unknown) {
      const hint = failure instanceof ApiError && typeof failure.body?.hint === "string" ? failure.body.hint : "";
      setError([failure instanceof Error ? failure.message : "OS approval failed.", hint].filter(Boolean).join(" "));
    } finally {
      if (!activeAuthorization.current) setBusy(false);
    }
  };
  const mutable = !readOnly && review.status === "pending" && !review.diagnostic;
  const grantChoices = review.choices.filter((candidate) => candidate !== "deny");
  return (
    <Card className="grid gap-2 border-amber-500/30 p-3" aria-label={`Permission request: ${review.label}`}>
      <div className="flex items-center justify-between gap-2">
        <h3 className="text-sm font-semibold">{review.label}</h3>
        <span className="text-xs text-muted-foreground">{review.risk} &middot; {review.status}</span>
      </div>
      <p className="text-xs text-muted-foreground">{review.description}</p>
      <p className="break-words text-sm"><strong>Target:</strong> {review.target}</p>
      {review.reason && <p className="text-xs"><span className="text-muted-foreground">Agent-provided reason: </span>{review.reason}</p>}
      {review.diagnostic && <p role="alert" className="text-xs text-destructive">{review.diagnostic}</p>}
      <details className="text-xs">
        <summary className="cursor-pointer text-muted-foreground">Technical details</summary>
        <p className="mt-1 break-all">{review.verb} &middot; {review.id} &middot; revision {review.revision}</p>
        {review.details.map((detail, index) => <p key={index} className="break-words">{detail.label}: {detail.value}</p>)}
      </details>
      {mutable && (
        <>
          <div className="flex flex-wrap gap-2">
            {grantChoices.map((choice) => <Button key={choice} size="sm"
              variant={choice === "allow_once" ? "default" : "outline"}
              disabled={busy || !!error} onClick={() => void decide(choice)}>{APPROVAL_LABELS[choice]}</Button>)}
            {review.choices.includes("deny") && <Button size="sm" variant="outline" disabled={busy || !!error} onClick={() => void decide("deny")}>Deny</Button>}
            {error && <Button size="sm" variant="outline" onClick={() => { setError(null); onChanged(); }}>Refresh and review again</Button>}
          </div>
          <p className="text-[11px] text-muted-foreground">OS authentication opens here. Authorization does not confirm task completion.</p>
        </>
      )}
      {error && <p role="alert" className="text-xs text-destructive">{error}</p>}
      {authorization && <OsAuthorizationDialog initial={authorization} onFinished={finishAuthorization} />}
    </Card>
  );
}

export function ApprovalViews({ taskId, recent = false, compact = false }: {
  taskId?: string; recent?: boolean; compact?: boolean;
}) {
  const [bundle, setBundle] = useState<ApprovalBundle | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [version, setVersion] = useState(0);
  const refresh = useCallback(() => setVersion((value) => value + 1), []);
  useEffect(() => {
    const controller = new AbortController();
    let loading = false;
    const load = async () => {
      if (loading || controller.signal.aborted) return;
      loading = true;
      try {
        const path = taskId ? `/api/tasks/${encodeURIComponent(taskId)}/approvals` :
          recent ? "/api/approvals/reviews/recent" : "/api/approvals/reviews";
        const value = await api.get(path, { signal: controller.signal });
        const next = readApprovalBundle(value, taskId);
        if (!controller.signal.aborted) { setBundle(next); setError(null); }
      } catch (failure: unknown) {
        if (!controller.signal.aborted) {
          setBundle(null);
          setError(failure instanceof Error ? failure.message : "OS approvals unavailable.");
        }
      } finally {
        loading = false;
      }
    };
    void load();
    const timer = window.setInterval(() => void load(), 3_000);
    return () => { controller.abort(); window.clearInterval(timer); };
  }, [taskId, recent, version]);
  if (error) return <div role="alert" className="grid gap-2 text-xs text-destructive">
    {error}<Button size="sm" variant="outline" onClick={refresh}>Refresh approvals</Button>
  </div>;
  if (!bundle) return compact ? null : <p className="text-sm text-muted-foreground">Loading OS approvals...</p>;
  if (compact && bundle.reviews.length === 0 && bundle.app_reviews.length === 0) return null;
  return (
    <div className="grid gap-3" aria-label={taskId ? "Current task approvals" : "OS approval inventory"}>
      {taskId && <p className="text-xs font-medium">Permission needed for this task</p>}
      {bundle.reviews.map((review) => <ApprovalCard key={review.id} review={review} onChanged={refresh} readOnly={recent} />)}
      {bundle.app_reviews.map((review) => <Card key={review.id} className="p-3 text-xs">
        {review.label} requires the OS App-review presenter. <code>cos review show {review.id}</code>
      </Card>)}
      {!compact && bundle.reviews.length === 0 && bundle.app_reviews.length === 0 && <p className="text-sm text-muted-foreground">No approval records in this view.</p>}
    </div>
  );
}
