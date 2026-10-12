import { EVIDENCE_CAVEAT, EVIDENCE_LABELS, type EvidencePresentation } from "@/lib/chat-evidence";

export function ChatEvidence({ evidence }: { evidence: EvidencePresentation }) {
  if (!evidence.required && evidence.status === "not-required" && evidence.source_count === 0) return null;
  return (
    <details className="rounded-md border border-muted px-3 py-2 text-xs">
      <summary className="cursor-pointer font-medium">
        Evidence provenance &middot; {EVIDENCE_LABELS[evidence.status]}
      </summary>
      <div className="mt-2 grid gap-2">
        <p>Bound citations: {evidence.verified_claims}/{evidence.total_claims}</p>
        <p>{evidence.interpretation}</p>
        <p className="text-muted-foreground">{EVIDENCE_CAVEAT}</p>
        {evidence.sources.map((source, index) => (
          <div key={`${source.tool_call_id}-${index}`} className="break-all rounded bg-muted px-2 py-1">
            {source.tool_name} &middot; {source.tool_call_id}
            {source.is_error && <span className="text-destructive"> &middot; failed tool result</span>}
            <div className="text-muted-foreground">Result SHA-256: {source.result_sha256}</div>
          </div>
        ))}
        {evidence.warnings.map((warning, index) => <p key={index} className="text-amber-600 dark:text-amber-400">{warning}</p>)}
        {evidence.truncated && <p className="text-muted-foreground">Showing a bounded subset of source metadata and warnings.</p>}
      </div>
    </details>
  );
}
