import { useState } from "react";
import { Loader2 } from "lucide-react";

import { Button } from "@/components/ui/button";
import { Card } from "@/components/ui/card";
import { Input } from "@/components/ui/input";
import { OperationEffects } from "@/components/operation-effects";
import type { useActivityObjects } from "@/hooks/use-activities";
import type { ActivityObjectAttachment, ObjectDescription } from "@/lib/activities";

export function ActivityObjectsPanel({
  view, editable, editing, disabled, onAttach,
}: {
  view: ReturnType<typeof useActivityObjects>;
  editable: boolean;
  editing: boolean;
  disabled: boolean;
  onAttach: (attachment: ActivityObjectAttachment) => Promise<boolean>;
}) {
  const [label, setLabel] = useState("");
  const [appId, setAppId] = useState("");
  const [objectType, setObjectType] = useState("");
  const [objectId, setObjectId] = useState("");
  const [revision, setRevision] = useState("");
  const canAttach = editable && !editing && !disabled;
  const complete = !!label.trim() && !!appId.trim() && !!objectType.trim() && objectId.length > 0;
  const data = view.data;

  return (
    <Card role="region" aria-label="App object references" className="min-w-0 gap-4 p-4">
      <div className="flex items-center justify-between gap-3">
        <h3 className="text-sm font-medium">App object references</h3>
        <Button size="sm" variant="outline" aria-label="Refresh object descriptions"
          disabled={view.loading || disabled} onClick={() => void view.refresh()}>
          {view.loading ? <Loader2 className="h-3.5 w-3.5 animate-spin" /> : "Refresh"}
        </Button>
      </div>
      <p className="text-xs text-muted-foreground">
        Declared confirms only a signed App manifest and its object type, not object existence,
        freshness, or permission. Viewing or attaching a reference does not run App code or read object data.
      </p>
      {view.error ? (
        <p role="alert" className="text-sm text-destructive">
          {view.error} Object descriptions are hidden until a successful refresh.
        </p>
      ) : data ? (
        <div className="grid min-w-0 gap-3">
          {data.objects.length === 0 && (
            <p className="text-sm text-muted-foreground">No App object references. Plain resources remain unchanged.</p>
          )}
          {data.objects.map((entry, index) => (
            <section key={`${entry.reference}-${index}`} aria-label={`Object reference: ${entry.label}`}
              className="grid min-w-0 gap-2 rounded-md border p-3">
              <div className="flex flex-wrap items-center justify-between gap-2">
                <h4 className="break-words text-sm font-medium">{entry.label}</h4>
                <span className="rounded-full border px-2 py-0.5 text-xs">
                  {entry.status === "declared" ? "Declared (manifest only)"
                    : entry.status === "unavailable" ? "Unavailable" : "Invalid reference"}
                </span>
              </div>
              <code className="whitespace-pre-wrap break-all text-xs text-muted-foreground">{entry.reference}</code>
              {entry.status === "declared" ? (
                <Declaration activityId={data.activity_id} description={entry.description} />
              ) : (
                <p className="whitespace-pre-wrap break-words text-sm text-destructive">{entry.error}</p>
              )}
            </section>
          ))}
        </div>
      ) : (
        <p className="text-sm text-muted-foreground">Loading object declarations…</p>
      )}
      <form className="grid gap-3 border-t pt-4" onSubmit={(event) => {
        event.preventDefault();
        if (!canAttach || !complete) return;
        void onAttach({
          label,
          object: {
            app_id: appId, object_type: objectType, object_id: objectId,
            ...(revision === "" ? {} : { revision }),
          },
        }).then((attached) => {
          if (attached) {
            setLabel("");
            setAppId("");
            setObjectType("");
            setObjectId("");
            setRevision("");
          }
        });
      }}>
        <h4 className="text-sm font-medium">Attach an App object</h4>
        <p className="text-xs text-muted-foreground">
          The broker verifies the App and type, then saves its canonical reference in Resources.
          The opaque ID is sent unchanged; no URL or command is constructed here.
        </p>
        {!editable && <p className="text-xs text-muted-foreground">Reopen this Activity before attaching an object.</p>}
        {editing && <p className="text-xs text-muted-foreground">Finish editing planning details before attaching an object.</p>}
        <fieldset disabled={!canAttach} className="grid min-w-0 gap-3 sm:grid-cols-2">
          <label className="grid gap-1.5 text-sm sm:col-span-2">
            Reference label
            <Input required maxLength={240} value={label} onChange={(event) => setLabel(event.target.value)} />
          </label>
          <label className="grid gap-1.5 text-sm">
            App ID
            <Input required maxLength={128} value={appId} placeholder="kv"
              onChange={(event) => setAppId(event.target.value)} />
          </label>
          <label className="grid gap-1.5 text-sm">
            Object type
            <Input required maxLength={64} value={objectType} placeholder="entry"
              onChange={(event) => setObjectType(event.target.value)} />
          </label>
          <label className="grid gap-1.5 text-sm sm:col-span-2">
            Opaque object ID
            <Input required maxLength={1024} value={objectId} placeholder="release.status"
              onChange={(event) => setObjectId(event.target.value)} />
          </label>
          <label className="grid gap-1.5 text-sm sm:col-span-2">
            Revision (optional)
            <Input maxLength={128} value={revision} onChange={(event) => setRevision(event.target.value)} />
          </label>
          <Button type="submit" className="w-fit" disabled={!complete}>Attach object reference</Button>
        </fieldset>
      </form>
    </Card>
  );
}

function Declaration({ activityId, description }: { activityId: string; description: ObjectDescription }) {
  const { object, invocation } = description;
  return (
    <div className="grid min-w-0 gap-2 text-sm">
      <p className="break-words font-medium">{description.object_label}</p>
      <p className="whitespace-pre-wrap break-words text-muted-foreground">{description.object_summary}</p>
      <dl className="grid min-w-0 gap-1 text-xs">
        <div><dt className="inline font-medium">App: </dt><dd className="inline break-words">{description.app_name} ({object.app_id}), version {description.app_version}</dd></div>
        <div><dt className="inline font-medium">Type: </dt><dd className="inline break-all">{object.object_type}</dd></div>
        <div><dt className="inline font-medium">Opaque ID: </dt><dd className="inline whitespace-pre-wrap break-all">{object.object_id}</dd></div>
        {object.revision !== undefined && (
          <div><dt className="inline font-medium">Revision: </dt><dd className="inline whitespace-pre-wrap break-all">{object.revision}</dd></div>
        )}
      </dl>
      <details className="min-w-0">
        <summary className="cursor-pointer text-xs font-medium">Invocation metadata (not executed)</summary>
        <p className="my-2 text-xs text-muted-foreground">
          Structured arguments only, not a shell command. Explicit App execution would still require ordinary capability and approval checks.
        </p>
        <pre className="max-h-48 overflow-y-auto whitespace-pre-wrap break-words rounded-md bg-muted p-3 text-xs">
          {JSON.stringify({ app_id: invocation.app_id, operation: invocation.operation, args: invocation.args }, null, 2)}
        </pre>
      </details>
      <OperationEffects activityId={activityId} objectReference={description.reference}
        appVersion={description.app_version} invocation={invocation} />
    </div>
  );
}
