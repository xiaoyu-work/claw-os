import { useState } from "react";
import { Loader2, PackageCheck, ShieldCheck } from "lucide-react";

import { api } from "@/lib/api";
import { type AppReview } from "@/lib/apps";
import { readAuthorization, type AuthorizationView } from "@/lib/approvals";
import { Button } from "./ui/button";
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "./ui/dialog";
import { OsAuthorizationDialog } from "./os-authorization";

export function AppReviewDialog({ review, onConfirmed, onCancel, progress }: {
  review: AppReview; onConfirmed: () => void; onCancel: () => void;
  progress?: { current: number; total: number };
}) {
  const [authorization, setAuthorization] = useState<AuthorizationView | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  async function confirm() {
    setBusy(true);
    setError(null);
    try {
      const value = await api.post(`/api/app-reviews/${encodeURIComponent(review.id)}/authorization`,
        { revision: review.revision }, { signal: AbortSignal.timeout(15_000) });
      setAuthorization(readAuthorization(value, {
        review_id: review.id, revision: review.revision, choice: null,
        action: review.kind === "activation" ? "confirm_activation"
          : review.kind === "update" ? "confirm_update" : "confirm_install",
      }));
    } catch (failure) {
      setError(failure instanceof Error ? failure.message : "App confirmation could not start.");
    } finally {
      setBusy(false);
    }
  }

  if (authorization) return <OsAuthorizationDialog initial={authorization} onFinished={(failure) => {
    setAuthorization(null);
    if (failure) setError(failure);
    else onConfirmed();
  }} />;

  return <Dialog open onOpenChange={(open) => { if (!open && !busy) onCancel(); }}>
    <DialogContent className="max-h-[85vh] max-w-2xl overflow-y-auto">
      <DialogHeader>
        <DialogTitle className="flex items-center gap-2"><PackageCheck className="h-5 w-5" />
          {review.kind === "activation" ? "Review App access" : "Review and install App"}</DialogTitle>
        <DialogDescription>
          {review.subject.name} · {review.subject.app_id} · {review.subject.version}
          {progress && <span className="mt-1 block">App review {progress.current} of {progress.total} in this APT transaction</span>}
        </DialogDescription>
      </DialogHeader>
      <div className="rounded-lg border bg-muted/30 p-3 text-xs">
        <p className="flex items-center gap-2 font-medium"><ShieldCheck className="h-4 w-4" />
          Verified package, separate runtime permissions</p>
        <p className="mt-2 text-muted-foreground">
          Confirmation installs or opens this exact package. It does not grant every requested permission,
          approve AI use, or merge access with another App.
        </p>
      </div>
      <div className="grid gap-2">
        {review.permissions.length === 0 && <p className="text-sm text-muted-foreground">No capability requests are declared.</p>}
        {review.permissions.map((permission) => <div key={permission.id} className="rounded-lg border p-3 text-sm">
          <div className="flex justify-between gap-3"><strong>{permission.label}</strong>
            <span className="text-xs text-muted-foreground">{permission.risk}</span></div>
          <p className="mt-1 break-words">{permission.scope.description}</p>
          <p className="mt-1 text-xs text-muted-foreground">{permission.blurb}</p>
          {permission.condition && <p className="mt-1 text-xs">Condition: {permission.condition}</p>}
          {permission.uses.map((use, index) => {
            if (!use || typeof use !== "object") return null;
            const item = use as Record<string, unknown>;
            return <p key={index} className="mt-1 text-xs text-muted-foreground">
              {[item.surface, item.label, item.purpose].filter((value) => typeof value === "string").join(" · ")}
            </p>;
          })}
        </div>)}
      </div>
      {review.disclosures.length > 0 && <details className="rounded-lg border p-3 text-xs">
        <summary className="cursor-pointer font-medium">Execution and integration details</summary>
        <dl className="mt-3 grid gap-2">{review.disclosures.map((item, index) => <div key={index}>
          <dt className="font-medium">{item.label}</dt><dd className="break-words text-muted-foreground">{item.value}</dd>
        </div>)}</dl>
      </details>}
      {error && <p role="alert" className="text-sm text-destructive">{error}</p>}
      <DialogFooter>
        <Button variant="outline" disabled={busy} onClick={onCancel}>Cancel</Button>
        <Button disabled={busy || !["pending", "confirmed"].includes(review.status)}
          onClick={() => review.status === "confirmed" ? onConfirmed() : void confirm()}>
          {busy && <Loader2 className="mr-2 h-4 w-4 animate-spin" />}
          {review.status === "confirmed" ? "Continue reviewed action"
            : review.kind === "activation" ? "Confirm App access" : "Confirm and install"}
        </Button>
      </DialogFooter>
    </DialogContent>
  </Dialog>;
}
