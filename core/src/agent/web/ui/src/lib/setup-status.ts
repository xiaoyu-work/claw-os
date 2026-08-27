export type SetupStatusView = {
  configured: boolean;
  ready: boolean;
  reason: string;
};

export function readSetupStatus(status: any): SetupStatusView {
  const provider =
    typeof status?.provider === "string" ? status.provider.trim() : "";
  const inferredConfigured =
    provider !== "" && provider !== "none" && provider !== "mock";
  const configured =
    typeof status?.configured === "boolean"
      ? status.configured
      : inferredConfigured;

  return {
    configured,
    ready: status?.ready === true,
    reason: setupStatusReason(status?.reason),
  };
}

function setupStatusReason(reason: any): string {
  if (typeof reason === "string") return reason;
  if (!reason || typeof reason !== "object") return "";
  const summary = typeof reason.error === "string" ? reason.error : "";
  const details = typeof reason.details === "string" ? reason.details : "";
  const fix = typeof reason.fix === "string" ? `Fix: ${reason.fix}` : "";
  return [summary, details, fix].filter(Boolean).join(" — ");
}
