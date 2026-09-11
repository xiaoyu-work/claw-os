import { Loader2 } from "lucide-react";

import { Button } from "@/components/ui/button";
import { Card } from "@/components/ui/card";
import type { useActivityReceipts } from "@/hooks/use-activities";
import type { ActivityReceipt, ReceiptDeclaration, ReceiptReport, ResultSummary } from "@/lib/activity-receipts";
import { effectRecoveryLabels } from "@/lib/operation-preview";

const outcomeLabels: Record<ReceiptReport["outcome"], string> = {
  returned: "Returned (caller report)",
  reported_error: "Reported error (caller report)",
  indeterminate: "Indeterminate (caller report)",
};

export function ActivityReceiptsPanel({
  view, ownerUid,
}: {
  view: ReturnType<typeof useActivityReceipts>;
  ownerUid: number;
}) {
  const data = view.data;
  const error = view.error || (data?.receipts.some((receipt) => receipt.owner_uid !== ownerUid)
    ? "Receipt owner does not match this Activity." : null);

  return (
    <Card role="region" aria-label="Caller-reported receipts" className="min-w-0 gap-4 p-4">
      <div className="flex flex-wrap items-center justify-between gap-2">
        <h3 className="text-sm font-medium">Caller-reported receipts</h3>
        <Button type="button" size="sm" variant="outline" aria-label="Refresh receipts"
          disabled={view.loading} onClick={() => void view.refresh()}>
          {view.loading ? <Loader2 className="h-3.5 w-3.5 animate-spin" /> : "Refresh"}
        </Button>
      </div>
      <p className="text-xs text-muted-foreground">
        Immutable caller reports, not OS-confirmed execution or mutation evidence.
        They grant no permissions and do not establish that the Activity goal was achieved.
        Previews and errors are bounded and may be redacted.
      </p>
      <p className="text-xs text-muted-foreground">
        An error or indeterminate outcome does not prove that no side effect occurred.
        An indeterminate report does not establish whether an App process started.
      </p>
      {error ? (
        <p role="alert" className="text-sm text-destructive">{error} Receipts are hidden until a successful refresh.</p>
      ) : data ? (
        <div className="grid min-w-0 gap-3">
          {data.receipts.length === 0 && (
            <p className="text-sm text-muted-foreground">
              No caller-reported receipts recorded. Missing receipts do not prove that no operation occurred.
            </p>
          )}
          {data.receipts.map((receipt) => <Receipt key={receipt.id} receipt={receipt} />)}
          {data.receipts.length === 100 && <p className="text-xs text-muted-foreground">Showing up to 100 recorded receipts.</p>}
        </div>
      ) : <p className="text-sm text-muted-foreground">Loading receipts...</p>}
    </Card>
  );
}

function Receipt({ receipt }: { receipt: ActivityReceipt }) {
  const { report } = receipt;
  return (
    <article aria-label={`Receipt ${receipt.id}`} className="grid min-w-0 gap-3 rounded-md border p-4">
      <div className="flex flex-wrap items-center justify-between gap-2">
        <h4 className="break-all text-sm font-medium">Reported App: {report.app_id}</h4>
        <span className="rounded-full border px-2 py-0.5 text-xs">{outcomeLabels[report.outcome]}</span>
      </div>
      <dl className="grid min-w-0 gap-1 text-xs">
        <div><dt className="inline font-medium">Source: </dt><dd className="inline">Caller-reported (not an execution attestation)</dd></div>
        <div><dt className="inline font-medium">Reported operation: </dt><dd className="inline break-all"><code>{report.operation}</code></dd></div>
        <div><dt className="inline font-medium">Receipt ID: </dt><dd className="inline break-all"><code>{receipt.id}</code></dd></div>
        <div><dt className="inline font-medium">Caller report ID: </dt><dd className="inline break-all"><code>{report.id}</code></dd></div>
        <div><dt className="inline font-medium">Reported package digest: </dt><dd className="inline break-all"><code>{report.package_digest}</code></dd></div>
        <div>
          <dt className="inline font-medium">Recorded by broker: </dt>
          <dd className="inline"><time dateTime={receipt.received_at} title={receipt.received_at}>{new Date(receipt.received_at).toLocaleString()}</time></dd>
        </div>
      </dl>
      <p className="text-xs text-muted-foreground">Recording time is not a claim about when an external operation executed.</p>
      {report.error !== null && (
        <div className="grid gap-1">
          <h5 className="text-xs font-medium">Reported error</h5>
          <p className="whitespace-pre-wrap break-words text-sm text-destructive">{report.error || "The report supplied no error text."}</p>
        </div>
      )}
      {report.result !== null ? <Result result={report.result} />
        : <p className="text-xs text-muted-foreground">No result summary was reported.</p>}
      {receipt.declaration !== null ? <Declaration declaration={receipt.declaration} /> : (
        <div className="grid gap-1 rounded-md border p-3">
          <h5 className="text-xs font-medium">App declaration unavailable</h5>
          <p className="whitespace-pre-wrap break-words text-sm text-destructive">
            {receipt.declaration_error || "The broker supplied no declaration diagnostic."}
          </p>
        </div>
      )}
    </article>
  );
}

function Result({ result }: { result: ResultSummary }) {
  return (
    <div className="grid min-w-0 gap-2">
      <h5 className="text-xs font-medium">Reported result summary</h5>
      <p className="text-xs">Format: {result.kind}. Reported original bytes: {result.bytes.toLocaleString()}.</p>
      <p className="break-all text-xs">Reported original-output SHA-256: <code>{result.sha256}</code></p>
      <p className="text-xs text-muted-foreground">
        The digest is a reported output hash, not OS execution proof.
        Displayed text is bounded and may be redacted; it cannot reconstruct the original bytes.
      </p>
      {result.preview_truncated && <p className="text-xs font-medium">Preview truncated.</p>}
      {result.preview ? (
        <pre className="max-h-56 overflow-y-auto whitespace-pre-wrap break-words rounded-md bg-muted p-3 text-xs">{result.preview}</pre>
      ) : <p className="text-xs text-muted-foreground">No preview text was retained.</p>}
    </div>
  );
}

function Declaration({ declaration }: { declaration: ReceiptDeclaration }) {
  return (
    <div className="grid min-w-0 gap-2 rounded-md border p-3 text-xs">
      <h5 className="font-medium">Recorded App declaration (metadata only)</h5>
      <p className="break-words">{declaration.operation_label}, App version {declaration.app_version}</p>
      <p className="text-muted-foreground">
        Authenticated at recording time only. This historical snapshot does not establish
        current App validity after changes or revocation.
        Matching signed manifest metadata does not authenticate the reported execution,
        confirm effects, or prove goal achievement. Recovery labels are not an undo guarantee.
      </p>
      {declaration.effects.length === 0 ? (
        <p className="text-muted-foreground">No effect declarations were supplied. Actual effects remain unknown.</p>
      ) : (
        <ul className="grid gap-2">
          {declaration.effects.map((effect, index) => (
            <li key={index} className="grid min-w-0 gap-1 border-t pt-2">
              <p className="break-words font-medium">{effect.label}</p>
              <p>App-declared {effect.kind}. Recovery: {effectRecoveryLabels[effect.recovery]}.</p>
              {effect.target_arg !== null && <p className="break-all">Declared target argument: <code>{effect.target_arg}</code></p>}
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}
