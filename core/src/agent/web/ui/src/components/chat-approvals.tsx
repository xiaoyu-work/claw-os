import { useCallback, useEffect, useRef, useState } from "react";
import { ChevronRight } from "lucide-react";
import { api, ApiError } from "@/lib/api";
import { APPROVAL_LABELS, APPROVAL_STATUS_LABELS, readApprovalBundle, readAuthorization, type ApprovalBundle, type ApprovalChoice, type ApprovalView, type AuthorizationView } from "@/lib/approvals";
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
  const primaryChoice = grantChoices.includes("allow_once") ? "allow_once" : grantChoices[0];
  const authorized = ["confirmed", "consumed", "completed"].includes(review.status);
  const details = [
    ...(review.reason ? [{ label: "Agent reason", value: review.reason }] : []),
    { label: "Permission", value: review.verb },
    { label: "Risk", value: review.risk },
    ...review.details.filter((detail) => detail.label !== "Requester-supplied reason"),
    { label: "Request", value: review.id },
    { label: "Revision", value: review.revision },
  ];
  return (
    <div className="px-4 py-3" aria-label={`Permission request: ${review.label}`} data-review-id={review.id}>
      <div className="flex flex-wrap items-start gap-x-4 gap-y-2">
        <details className="group min-w-0 flex-1 basis-64">
          <summary className="flex min-h-8 cursor-pointer list-none items-center gap-3 rounded-sm outline-none focus-visible:ring-2 focus-visible:ring-ring [&::-webkit-details-marker]:hidden">
            <ChevronRight className="h-4 w-4 shrink-0 text-muted-foreground transition-transform group-open:rotate-90" aria-hidden="true" />
            <span className="min-w-0 flex-1">
              <span className="block truncate text-sm font-medium" title={review.label}>{review.label}</span>
              <span className="block truncate text-xs text-muted-foreground" title={review.target}>{review.target}</span>
            </span>
            {(review.status === "pending" || review.risk === "high" || review.risk === "critical") && (
              <span className={`shrink-0 rounded px-1.5 py-0.5 text-[11px] ${
                review.risk === "high" || review.risk === "critical"
                  ? "bg-destructive/10 text-destructive" : "bg-muted text-muted-foreground"
              }`} aria-label={`${review.risk} risk`}>{review.risk}</span>
            )}
            <span className={`shrink-0 text-xs ${
              authorized ? "text-emerald-600 dark:text-emerald-400" :
                review.status === "pending" ? "text-amber-600 dark:text-amber-400" :
                  review.status === "failed" ? "text-destructive" : "text-muted-foreground"
            }`}>{APPROVAL_STATUS_LABELS[review.status]}</span>
          </summary>
          <dl className="ml-7 mt-3 grid gap-2 border-t pt-3 text-xs">
            {details.map((detail, index) => (
              <div key={index} className="grid gap-0.5 sm:grid-cols-[9rem_minmax(0,1fr)] sm:gap-3">
                <dt className="text-muted-foreground">{detail.label}</dt>
                <dd className="min-w-0 whitespace-pre-wrap break-words">{detail.value}</dd>
              </div>
            ))}
          </dl>
        </details>
        {mutable && (
          <div className="flex flex-wrap items-center gap-2">
            {grantChoices.map((choice) => <Button key={choice} size="sm"
              variant={choice === primaryChoice ? "default" : "outline"}
              disabled={busy || !!error} onClick={() => void decide(choice)}>{APPROVAL_LABELS[choice]}</Button>)}
            {review.choices.includes("deny") && <Button size="sm" variant="ghost" disabled={busy || !!error} onClick={() => void decide("deny")}>Deny</Button>}
            {error && <Button size="sm" variant="outline" onClick={() => { setError(null); onChanged(); }}>Refresh and review again</Button>}
          </div>
        )}
      </div>
      {review.diagnostic && <p role="alert" className="mt-2 text-xs text-destructive">{review.diagnostic}</p>}
      {error && <p role="alert" className="mt-2 text-xs text-destructive">{error}</p>}
      {authorization && <OsAuthorizationDialog initial={authorization} onFinished={finishAuthorization} />}
    </div>
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
      {bundle.reviews.length > 0 && <div className="divide-y overflow-hidden rounded-lg border">
        {bundle.reviews.map((review) => <ApprovalCard key={review.id} review={review} onChanged={refresh} readOnly={recent} />)}
      </div>}
      {bundle.app_reviews.map((review) => <Card key={review.id} className="p-3 text-xs">
        {review.label} requires the OS App-review presenter. <code>cos review show {review.id}</code>
      </Card>)}
      {!compact && bundle.reviews.length === 0 && bundle.app_reviews.length === 0 && <p className="text-sm text-muted-foreground">No approval records in this view.</p>}
    </div>
  );
}
