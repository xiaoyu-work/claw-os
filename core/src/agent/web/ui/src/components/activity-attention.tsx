import { AlertTriangle, Bell, ShieldCheck } from "lucide-react";

import { Button } from "@/components/ui/button";
import { Card } from "@/components/ui/card";
import type { useActivityAttention } from "@/hooks/use-activities";
import { navigate } from "@/lib/router";

type View = ReturnType<typeof useActivityAttention>;

export function ActivityAttentionPanel({ view }: { view: View }) {
  const attention = view.data;
  return (
    <Card className="min-w-0 gap-4 p-4" aria-label="Activity attention">
      <div className="flex flex-wrap items-center justify-between gap-2">
        <div>
          <h3 className="text-sm font-medium">Attention</h3>
          <p className="text-xs text-muted-foreground">
            Shared task, decision, and notification state. Reading or acknowledging a notification is not consent.
          </p>
        </div>
        <Button size="sm" variant="outline" disabled={view.loading}
          onClick={() => void view.refresh()}>Refresh attention</Button>
      </div>
      {view.error && <p role="alert" className="text-sm text-destructive">{view.error}</p>}
      {!attention && view.loading && <p className="text-sm text-muted-foreground">Loading attention…</p>}
      {attention && (
        <>
          <div className="grid gap-2 text-xs sm:grid-cols-2 xl:grid-cols-5">
            <Count label="Queued" value={attention.counts.queued} />
            <Count label="Running" value={attention.counts.running} />
            <Count label="Waiting" value={attention.counts.waiting} />
            <Count label="Failed" value={attention.counts.failed} />
            <Count label="Indeterminate" value={attention.counts.indeterminate} />
          </div>
          <section className="grid gap-2" aria-label="Activity decisions">
            <h4 className="flex items-center gap-2 text-sm font-medium">
              <ShieldCheck className="h-4 w-4" /> Decisions ({attention.totals.decisions})
            </h4>
            {attention.decisions.length === 0 && (
              <p className="text-xs text-muted-foreground">No retained permission decisions need attention.</p>
            )}
            {attention.decisions.map((decision) => (
              <div key={`${decision.job_id}:${decision.id}`} className="grid gap-1 rounded-lg border p-3 text-xs">
                <div className="flex flex-wrap items-center justify-between gap-2">
                  <span className="font-medium">{decision.verb ?? "Permission details unavailable"}</span>
                  <span>{decision.status.replace("_", " ")}</span>
                </div>
                <p className="break-words text-muted-foreground">{decision.reason ?? decision.error}</p>
                {decision.review_id && decision.status === "pending" && (
                  <Button size="sm" variant="outline" className="w-fit"
                    onClick={() => navigate("/approvals")}>Open OS reviews</Button>
                )}
                {decision.status === "approved" && (
                  <p className="text-muted-foreground">
                    Historical consent only; current authority and execution outcome are checked separately.
                  </p>
                )}
              </div>
            ))}
            {attention.has_more.decisions && <p className="text-xs text-muted-foreground">More decisions are retained.</p>}
          </section>
          <section className="grid gap-2" aria-label="Activity issues">
            <h4 className="flex items-center gap-2 text-sm font-medium">
              <AlertTriangle className="h-4 w-4" /> Issues ({attention.totals.issues})
            </h4>
            {attention.issues.map((issue) => (
              <div key={`${issue.job_id}:${issue.kind}`} className="grid gap-1 rounded-lg border p-3 text-xs">
                <div className="flex flex-wrap items-center justify-between gap-2">
                  <span className="font-medium">{issue.title}</span>
                  <span>{issue.kind.replace("_", " ")}</span>
                </div>
                <p className="break-words text-muted-foreground">{issue.message}</p>
              </div>
            ))}
            {attention.issues.length === 0 && <p className="text-xs text-muted-foreground">No retained execution issues.</p>}
          </section>
          <section className="grid gap-2" aria-label="Activity notifications">
            <h4 className="flex items-center gap-2 text-sm font-medium">
              <Bell className="h-4 w-4" /> Notifications ({attention.totals.notifications})
            </h4>
            {attention.notifications.map((notification) => (
              <div key={notification.id} className="grid gap-1 rounded-lg border p-3 text-xs">
                <div className="flex flex-wrap items-center justify-between gap-2">
                  <span className="font-medium">{notification.title}</span>
                  <span>{notification.state}</span>
                </div>
                <p className="break-words text-muted-foreground">{notification.body}</p>
              </div>
            ))}
            {attention.notifications.length === 0 && <p className="text-xs text-muted-foreground">No retained task notifications.</p>}
          </section>
        </>
      )}
    </Card>
  );
}

function Count({ label, value }: { label: string; value: number }) {
  return <div className="rounded-md border px-3 py-2"><span className="font-medium">{value}</span> {label}</div>;
}
