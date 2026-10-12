import { record } from "./api-shapes";
import { readEvidence } from "./chat-evidence";
import { readMemorySources } from "./memory";
import { accumulateTurnUsage, appendReasoningSummary, type ChatMessage, type ToolCall } from "./chat-history";

function warning(message: ChatMessage, text: string) {
  if (!message.warnings.includes(text)) message.warnings.push(text);
}

export function markToolsUnconfirmed(message: ChatMessage) {
  for (const tool of message.tools) {
    if (!tool.finished) tool.phase = "unconfirmed";
  }
}

function tool(message: ChatMessage, data: unknown): ToolCall | null {
  if (!record(data) || typeof data.id !== "string" || !data.id) {
    warning(message, "Tool progress omitted its exact call identity.");
    return null;
  }
  const existing = message.tools.find((candidate) => candidate.id === data.id);
  const name = typeof data.name === "string" && data.name ? data.name : existing?.name || "Unnamed tool";
  if (existing) {
    existing.name = name;
    return existing;
  }
  const created: ToolCall = { id: data.id, name, finished: false, phase: "preparing" };
  message.tools.push(created);
  return created;
}

function evidence(message: ChatMessage, data: unknown) {
  try {
    message.evidence = readEvidence(data);
  } catch (error: unknown) {
    delete message.evidence;
    warning(message, error instanceof Error ? error.message : "Evidence metadata is unavailable.");
  }
}

export function applyFrame(message: ChatMessage, event: string, data: unknown) {
  const payload = record(data) ? data : {};
  switch (event) {
    case "task":
      if (typeof payload.task_id === "string") message.taskId = payload.task_id;
      break;
    case "text":
      if (typeof data === "string") message.text += data;
      else if (typeof payload.delta === "string") message.text += payload.delta;
      else warning(message, "Text progress omitted its delta.");
      break;
    case "tool_use_start":
    case "tool_use": {
      const call = tool(message, payload);
      if (call && !call.finished && call.phase !== "running") call.phase = "preparing";
      break;
    }
    case "tool_start": {
      const call = tool(message, payload);
      if (call) {
        call.phase = "running";
        call.finished = false;
        delete call.isError;
        delete call.latencyMs;
        delete call.bytesReturned;
        delete call.errorPreview;
      }
      break;
    }
    case "tool_result": {
      const call = tool(message, payload);
      if (!call) break;
      const ok = typeof payload.ok === "boolean" ? payload.ok :
        typeof payload.is_error === "boolean" ? !payload.is_error : null;
      call.finished = ok !== null;
      call.isError = ok === false;
      call.phase = ok === null ? "unconfirmed" : ok ? "completed" : "failed";
      if (ok === null) warning(message, "Tool result omitted its confirmed outcome.");
      if (typeof payload.latency_ms === "number" && Number.isSafeInteger(payload.latency_ms) && payload.latency_ms >= 0) call.latencyMs = payload.latency_ms;
      if (typeof payload.bytes_returned === "number" && Number.isSafeInteger(payload.bytes_returned) && payload.bytes_returned >= 0) call.bytesReturned = payload.bytes_returned;
      if (ok === false && typeof payload.error_preview === "string") call.errorPreview = payload.error_preview;
      break;
    }
    case "reasoning": appendReasoningSummary(message, data); break;
    case "turn_done": accumulateTurnUsage(message, data); break;
    case "evidence": evidence(message, data); break;
    case "memory_sources":
      try {
        const existing = message.memorySources || [];
        for (const source of readMemorySources(payload.sources)) {
          if (!existing.some((read) => read.source === source.source && read.revision === source.revision)) existing.push(source);
        }
        message.memorySources = existing;
      } catch (error: unknown) { warning(message, error instanceof Error ? error.message : "Memory references are unavailable."); }
      break;
    case "warning": warning(message, stringifyServerMessage(data, "warning")); break;
    case "done":
      if (payload.evidence != null) evidence(message, payload.evidence);
      message.status = "done";
      markToolsUnconfirmed(message);
      break;
    case "error":
      message.status = "error";
      message.error = stringifyServerMessage(data, "stream error");
      markToolsUnconfirmed(message);
      break;
  }
}

function stringifyServerMessage(data: unknown, fallback: string): string {
  if (typeof data === "string") return data;
  if (!record(data)) return fallback;
  if (typeof data.message === "string" && data.message) return data.message;
  if (typeof data.error === "string" && data.error) {
    const detail = record(data.details) && typeof data.details.message === "string" ? data.details.message :
      typeof data.details === "string" ? data.details : "";
    const fix = typeof data.fix === "string" ? data.fix :
      record(data.details) && typeof data.details.fix === "string" ? data.details.fix : "";
    return [data.error, detail, fix ? `Fix: ${fix}` : ""].filter(Boolean).join(" - ");
  }
  return typeof data.details === "string" ? data.details : JSON.stringify(data);
}
