import { useCallback, useState } from "react";
import { Loader2 } from "lucide-react";

import { Button } from "@/components/ui/button";
import { useActivityView } from "@/hooks/use-activities";
import { activityApi } from "@/lib/activities";
import { effectRecoveryLabels, type OperationInvocation, type OperationPreview, type PlannedEffect } from "@/lib/operation-preview";

type Props = {
  activityId: string;
  objectReference: string;
  appVersion: string;
  invocation: OperationInvocation;
};

export function OperationEffects(props: Props) {
  const { activityId, objectReference, appVersion, invocation } = props;
  const key = JSON.stringify([activityId, objectReference, appVersion, invocation.app_id, invocation.operation, invocation.args]);
  return <ScopedOperationEffects key={key} activityId={activityId} invocation={invocation} />;
}

function ScopedOperationEffects({ activityId, invocation }: Pick<Props, "activityId" | "invocation">) {
  const [input] = useState(() => ({
    app_id: invocation.app_id, operation: invocation.operation, args: [...invocation.args],
  }));
  const read = useCallback((signal: AbortSignal) =>
    activityApi.previewOperation(activityId, input, signal), [activityId, input]);
  const view = useActivityView(read);

  return (
    <section aria-label="Operation effect preview" className="grid min-w-0 gap-3 border-t pt-3">
      <div className="flex flex-wrap items-center justify-between gap-2">
        <h5 className="text-sm font-medium">App-declared effect preview</h5>
        <Button type="button" size="sm" variant="outline" disabled={view.loading} onClick={() => void view.refresh()}>
          {view.loading && <Loader2 className="mr-1 h-3.5 w-3.5 animate-spin" />}
          Preview effects
        </Button>
      </div>
      <p className="text-xs text-muted-foreground">
        Metadata only: not verified changes, safe execution, or authorization.
        Previewing runs no App code and reads no object data or credentials.
      </p>
      {view.loading ? <p role="status" className="text-xs text-muted-foreground">Loading effect declarations...</p>
        : view.error ? <p role="alert" className="text-sm text-destructive">{view.error}</p>
        : view.data ? <PreviewResult preview={view.data} /> : null}
    </section>
  );
}

function PreviewResult({ preview }: { preview: OperationPreview }) {
  return (
    <div className="grid min-w-0 gap-3">
      <div className="grid min-w-0 gap-1 text-xs">
        <p className="break-words font-medium">
          {preview.app_name} ({preview.app_id}), version {preview.app_version}: {preview.operation_label} ({preview.operation})
        </p>
        <p className="break-all text-muted-foreground">Package digest: <code>{preview.package_digest}</code></p>
        <p>Not executed. Authorization not checked. Effects not confirmed.</p>
        <p className="text-muted-foreground">A metadata snapshot, not an execution receipt or file diff. Preview again after App changes.</p>
      </div>
      {!preview.effects_declared || preview.effects.length === 0 ? (
        <p className="rounded-md border p-3 text-sm">
          Effects unknown. No effect declarations were supplied; this is not a read-only guarantee.
        </p>
      ) : (
        <div className="grid min-w-0 gap-2">
          {preview.effects.map((effect, index) => <Effect key={index} effect={effect} />)}
        </div>
      )}
      <p className="text-xs text-muted-foreground">
        Recovery labels are App declarations, not an undo guarantee.
        Reversible does not prove an inverse was recorded; compensation may not erase an external effect.
      </p>
      <p className="text-xs text-muted-foreground">
        Targets are requested values only, not final canonical paths or authorized resources.
        Ordinary execution still resolves targets and performs capability and approval checks.
      </p>
      {preview.unresolved_arguments.length > 0 && (
        <div className="grid gap-1 text-xs">
          <h6 className="font-medium">Unresolved runtime arguments</h6>
          <ul className="grid gap-1">
            {preview.unresolved_arguments.map((name, index) => (
              <li key={index}><code className="whitespace-pre-wrap break-all">{name}</code></li>
            ))}
          </ul>
          <p className="text-muted-foreground">No provider or credential selection has been performed.</p>
        </div>
      )}
      {preview.notes.length > 0 && (
        <div className="grid gap-1 text-xs">
          <h6 className="font-medium">Preview notes</h6>
          <ul className="grid gap-1 text-muted-foreground">
            {preview.notes.map((note, index) => <li key={index} className="whitespace-pre-wrap break-words">{note}</li>)}
          </ul>
        </div>
      )}
    </div>
  );
}

const targetLabels: Record<PlannedEffect["target_state"], string> = {
  requested: "Requested targets",
  unresolved: "Target unresolved",
  unspecified: "Target unspecified by this declaration",
};

function Effect({ effect }: { effect: PlannedEffect }) {
  return (
    <div className="grid min-w-0 gap-2 rounded-md border p-3 text-xs">
      <div className="flex flex-wrap items-center justify-between gap-2">
        <p className="break-words font-medium">{effect.label}</p>
        <span className="rounded-full border px-2 py-0.5">App-declared {effect.kind}</span>
      </div>
      <p>Recovery: {effectRecoveryLabels[effect.recovery]}</p>
      {effect.target_arg !== null && (
        <p className="break-all">Target argument: <code>{effect.target_arg}</code></p>
      )}
      {effect.target_kind !== null && <p>Target kind: {effect.target_kind}</p>}
      <p className="font-medium">{targetLabels[effect.target_state]}</p>
      {effect.requested_targets.length > 0 ? (
        <ul className="grid gap-1">
          {effect.requested_targets.map((target, index) => (
            <li key={index}><code className="whitespace-pre-wrap break-all">{target}</code></li>
          ))}
        </ul>
      ) : <p className="text-muted-foreground">No requested target values were returned. Actual targets remain unknown.</p>}
    </div>
  );
}
