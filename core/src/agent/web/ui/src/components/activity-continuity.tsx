import { useEffect, useRef, useState } from "react";

import { Button } from "@/components/ui/button";
import { Card } from "@/components/ui/card";
import { activityError } from "@/hooks/use-activities";
import {
  MAX_CONTINUITY_DOCUMENT_BYTES,
  continuityApi,
  continuityFilename,
  continuityJson,
  parseContinuityFile,
  type ContinuityDocument,
} from "@/lib/activity-continuity";

export function ActivityContinuityPanel({
  selectedId,
  existingIds,
  onImported,
  onSelectImported,
}: {
  selectedId: string | null;
  existingIds: ReadonlySet<string>;
  onImported: () => Promise<unknown>;
  onSelectImported: (id: string) => void;
}) {
  const [fileName, setFileName] = useState("");
  const [document, setDocument] = useState<ContinuityDocument | null>(null);
  const [placement, setPlacement] = useState("");
  const [confirmed, setConfirmed] = useState(false);
  const [busy, setBusy] = useState<"export" | "import" | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const mounted = useRef(true);
  const selectionRevision = useRef(0);
  const fileRevision = useRef(0);
  const exportAbort = useRef<AbortController | null>(null);

  useEffect(() => {
    mounted.current = true;
    return () => {
      mounted.current = false;
      exportAbort.current?.abort();
    };
  }, []);

  useEffect(() => {
    selectionRevision.current += 1;
    exportAbort.current?.abort();
    setError(null);
    setNotice(null);
    setBusy(null);
  }, [selectedId]);

  async function exportSelected() {
    if (!selectedId || busy) return;
    const revision = selectionRevision.current;
    const controller = new AbortController();
    exportAbort.current?.abort();
    exportAbort.current = controller;
    setBusy("export");
    setError(null);
    setNotice(null);
    try {
      const exported = await continuityApi.export(selectedId, controller.signal);
      if (!mounted.current || controller.signal.aborted
        || revision !== selectionRevision.current || selectedId === null) return;
      const blob = new Blob([`${continuityJson(exported)}\n`], { type: "application/json" });
      const url = URL.createObjectURL(blob);
      const link = window.document.createElement("a");
      link.href = url;
      link.download = continuityFilename(exported);
      link.click();
      window.setTimeout(() => URL.revokeObjectURL(url), 0);
      setNotice(`Exported continuity v1 revision ${exported.lineage.revision}.`);
    } catch (cause) {
      if (mounted.current && !controller.signal.aborted
        && revision === selectionRevision.current) setError(activityError(cause));
    } finally {
      if (mounted.current && revision === selectionRevision.current) setBusy(null);
    }
  }

  async function chooseFile(file: File | null) {
    const revision = ++fileRevision.current;
    setFileName(file?.name ?? "");
    setDocument(null);
    setPlacement("");
    setConfirmed(false);
    setError(null);
    setNotice(null);
    if (!file) return;
    if (file.size > MAX_CONTINUITY_DOCUMENT_BYTES) {
      setError(`Activity continuity file exceeds ${MAX_CONTINUITY_DOCUMENT_BYTES} bytes.`);
      return;
    }
    try {
      const parsed = await parseContinuityFile(await file.text());
      if (!mounted.current || revision !== fileRevision.current) return;
      setDocument(parsed);
      setNotice("Continuity v1 document validated. Choose and confirm placement before import.");
    } catch (cause) {
      if (mounted.current && revision === fileRevision.current) setError(activityError(cause));
    }
  }

  async function importDocument() {
    if (!document || placement !== "local" || !confirmed || busy) return;
    const revision = selectionRevision.current;
    const submitted = document;
    setBusy("import");
    setError(null);
    setNotice(null);
    try {
      const imported = await continuityApi.import(submitted, existingIds);
      if (!mounted.current || revision !== selectionRevision.current) return;
      await onImported();
      if (!mounted.current || revision !== selectionRevision.current) return;
      onSelectImported(imported.activity.id);
      setNotice(
        `Imported continuity revision ${imported.continuity_revision} as a new paused Activity.`,
      );
      setDocument(null);
      setFileName("");
      setPlacement("");
      setConfirmed(false);
    } catch (cause) {
      if (mounted.current && revision === selectionRevision.current) setError(activityError(cause));
    } finally {
      if (mounted.current && revision === selectionRevision.current) setBusy(null);
    }
  }

  return (
    <Card role="region" aria-label="Activity continuity" className="min-w-0 gap-3 p-4">
      <div className="flex flex-wrap items-center justify-between gap-3">
        <div>
          <h2 className="text-sm font-medium">Portable Activity continuity</h2>
          <p className="text-xs text-muted-foreground">
            Explicitly export the selected Activity or import a bounded JSON file.
          </p>
        </div>
        <Button size="sm" variant="outline" disabled={!selectedId || busy !== null}
          onClick={() => void exportSelected()}>
          {busy === "export" ? "Exporting…" : "Export selected Activity"}
        </Button>
      </div>
      <p className="text-xs text-muted-foreground">
        Continuity preserves only intent, semantic references, safe finite rules, and scheduling
        preference. It is not live sync or backup/restore. Imports create a new paused Activity
        and start no work. Credentials, grants, approvals, capability policy, monetary settings
        or ledger, Jobs, Sessions, receipts, effects, results, object state or history, audit
        evidence, and execution proof do not migrate.
      </p>
      {!selectedId && (
        <p className="text-xs text-muted-foreground">
          Select an Activity before exporting. Export never changes the source Activity or its work.
        </p>
      )}
      <div className="grid gap-3 border-t pt-3">
        <label className="grid gap-1.5 text-sm">
          Continuity JSON file
          <input type="file" accept=".json,application/json" disabled={busy !== null}
            aria-label="Continuity JSON file"
            className="block min-w-0 text-sm"
            onChange={(event) => void chooseFile(event.currentTarget.files?.[0] ?? null)} />
        </label>
        <p className="text-xs text-muted-foreground">
          The browser reads one local file of at most {MAX_CONTINUITY_DOCUMENT_BYTES} bytes.
          No server path is accepted or sent.
        </p>
        {fileName && <p className="break-all text-xs text-muted-foreground">Selected file: {fileName}</p>}
        {document && (
          <div className="grid gap-1 text-sm">
            <p><span className="font-medium">Validated title: </span>{document.intent.title}</p>
            <p><span className="font-medium">Lineage: </span>
              <code className="break-all">{document.lineage.id}</code>
            </p>
            <p><span className="font-medium">Revision: </span>{document.lineage.revision}</p>
          </div>
        )}
        <label className="grid gap-1.5 text-sm">
          Execution placement
          <select value={placement} disabled={!document || busy !== null}
            aria-label="Execution placement"
            className="h-9 min-w-0 rounded-md border bg-background px-3 text-sm"
            onChange={(event) => {
              setPlacement(event.target.value);
              setConfirmed(false);
            }}>
            <option value="">Choose placement…</option>
            <option value="local">Local machine</option>
          </select>
        </label>
        <label className="flex items-start gap-2 text-sm">
          <input type="checkbox" checked={confirmed}
            disabled={!document || placement !== "local" || busy !== null}
            onChange={(event) => setConfirmed(event.target.checked)} />
          <span>
            I confirm local placement. Import creates a new paused Activity and starts no work;
            destination capability, approval, budget, and worker controls remain authoritative.
          </span>
        </label>
        <Button className="w-fit" disabled={!document || placement !== "local" || !confirmed || busy !== null}
          onClick={() => void importDocument()}>
          {busy === "import" ? "Importing…" : "Import paused Activity"}
        </Button>
      </div>
      {error && <p role="alert" className="text-sm text-destructive">{error}</p>}
      {notice && <p role="status" className="text-sm text-muted-foreground">{notice}</p>}
    </Card>
  );
}
