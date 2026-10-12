import { oneOf, record, strings } from "./api-shapes";

const STATUSES = ["not-required", "verified", "partial", "missing", "invalid"] as const;
export type EvidencePresentation = {
  status: typeof STATUSES[number];
  required: boolean;
  verified_claims: number;
  total_claims: number;
  source_count: number;
  sources: { tool_call_id: string; tool_name: string; is_error: boolean; result_sha256: string; result_bytes: number }[];
  warnings: string[];
  interpretation: string;
  truncated: boolean;
};

function count(value: unknown): value is number {
  return typeof value === "number" && Number.isSafeInteger(value) && value >= 0;
}

export function readEvidence(value: unknown): EvidencePresentation {
  if (!record(value) || !oneOf(STATUSES, value.status) || typeof value.required !== "boolean" ||
      !count(value.verified_claims) || !count(value.total_claims) ||
      value.verified_claims > value.total_claims || !Array.isArray(value.sources) ||
      value.sources.length > 128 || !strings(value.warnings) || value.warnings.length > 32 ||
      typeof value.interpretation !== "string") {
    throw new Error("The evidence report has an invalid presentation shape.");
  }
  const sources = value.sources.map((source: unknown) => {
    if (!record(source) || typeof source.tool_call_id !== "string" ||
        typeof source.tool_name !== "string" || typeof source.is_error !== "boolean" ||
        typeof source.result_sha256 !== "string" || !/^[a-f0-9]{64}$/.test(source.result_sha256) ||
        !count(source.result_bytes)) {
      throw new Error("The evidence report has an invalid source reference.");
    }
    return {
      tool_call_id: source.tool_call_id, tool_name: source.tool_name,
      is_error: source.is_error, result_sha256: source.result_sha256, result_bytes: source.result_bytes,
    };
  });
  const sourceCount = value.source_count ?? sources.length;
  if (!count(sourceCount) || sourceCount < sources.length ||
      (value.truncated !== undefined && typeof value.truncated !== "boolean")) {
    throw new Error("The evidence report has invalid coverage metadata.");
  }
  return {
    status: value.status, required: value.required,
    verified_claims: value.verified_claims, total_claims: value.total_claims,
    source_count: sourceCount, sources, warnings: value.warnings,
    interpretation: value.interpretation, truncated: value.truncated === true,
  };
}

export const EVIDENCE_LABELS: Record<EvidencePresentation["status"], string> = {
  "not-required": "No live-system evidence required",
  verified: "Source binding verified",
  partial: "Source binding partial",
  missing: "Source evidence missing",
  invalid: "Source references invalid",
};

export const EVIDENCE_CAVEAT =
  "Source binding does not prove factual correctness, execution effects, or goal completion.";
