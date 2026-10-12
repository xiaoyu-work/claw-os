export type ToolCall = {
  id: string;
  name: string;
  isError?: boolean;
  finished: boolean;
  latencyMs?: number;
  bytesReturned?: number;
  errorPreview?: string;
  phase?: "preparing" | "running" | "completed" | "failed" | "unconfirmed";
};

export type TokenUsage = {
  inputTokens: number;
  outputTokens: number;
  cacheReadTokens: number;
  cacheWriteTokens: number;
};

export type ChatAttachment = {
  name: string;
  mediaType: string;
  bytes: number;
  dataUrl?: string;
  sha256?: string;
};

export type ChatMessage = {
  id: string;
  role: "user" | "assistant";
  text: string;
  attachments: ChatAttachment[];
  tools: ToolCall[];
  reasoning: string[];
  usage?: TokenUsage;
  warnings: string[];
  status: "streaming" | "done" | "error" | "interrupted";
  error?: string;
  taskId?: string;
  evidence?: EvidencePresentation;
  memorySources?: MemoryAccess[];
};

export type RetainedTask = {
  id: string;
  status: string;
  presentation?: {
    available: boolean;
    reasoning: string[];
    usage: {
      input_tokens: number; output_tokens: number;
      cache_read_tokens: number; cache_write_tokens: number;
    } | null;
    tools: {
      id: string; name: string; started: boolean; ok: boolean | null;
      latency_ms: number | null; bytes_returned: number | null; error_preview: string | null;
    }[];
    attachments: { name: string; media_type: string; bytes: number; sha256: string }[];
    diagnostic?: string | null;
    evidence?: EvidencePresentation | null;
    memory_sources?: MemoryAccess[];
  } | null;
};

export type ActiveConversationTask = {
  id: string;
  prompt: string;
};

export function appendReasoningSummary(message: ChatMessage, data: any) {
  const summaries = Array.isArray(data?.summary)
    ? data.summary.filter(
        (summary: unknown): summary is string =>
          typeof summary === "string" && summary.trim().length > 0,
      )
    : [];
  for (const summary of summaries) {
    if (!message.reasoning.includes(summary)) {
      message.reasoning.push(summary);
    }
  }
}

export function accumulateTurnUsage(message: ChatMessage, data: any) {
  const usage = data?.usage;
  if (!usage || typeof usage !== "object") return;

  const turn: TokenUsage = {
    inputTokens: tokenCount(usage.input_tokens),
    outputTokens: tokenCount(usage.output_tokens),
    cacheReadTokens: tokenCount(usage.cache_read_tokens),
    cacheWriteTokens: tokenCount(usage.cache_write_tokens),
  };
  const current = message.usage || {
    inputTokens: 0,
    outputTokens: 0,
    cacheReadTokens: 0,
    cacheWriteTokens: 0,
  };
  message.usage = {
    inputTokens: current.inputTokens + turn.inputTokens,
    outputTokens: current.outputTokens + turn.outputTokens,
    cacheReadTokens: current.cacheReadTokens + turn.cacheReadTokens,
    cacheWriteTokens: current.cacheWriteTokens + turn.cacheWriteTokens,
  };
}

function tokenCount(value: unknown): number {
  return typeof value === "number" && Number.isSafeInteger(value) && value >= 0
    ? value
    : 0;
}

export function restoreForActiveTask(
  rows: any[],
  task: ActiveConversationTask,
  jobs: RetainedTask[] = [],
): ChatMessage[] {
  const retained = rows.filter(
    (row) => row?.task_id !== task.id || row?.is_user_prompt === true,
  );
  const restored = restoreHistoryMessages(retained, jobs.filter((job) => job.id !== task.id));
  const promptRetained = rows.some(
    (row) => row?.task_id === task.id && row?.is_user_prompt === true,
  );
  if (!promptRetained) {
    restored.push({
      id: `task-user-${task.id}`,
      role: "user",
      text: task.prompt,
      attachments: [],
      tools: [],
      reasoning: [],
      warnings: [],
      status: "done",
      taskId: task.id,
    });
  }
  const presentation = jobs.find((job) => job.id === task.id)?.presentation;
  const user = restored.find((message) => message.taskId === task.id && message.role === "user");
  if (user && presentation) {
    user.attachments = presentation.attachments.map((attachment) => ({
      name: attachment.name, mediaType: attachment.media_type,
      bytes: attachment.bytes, sha256: attachment.sha256,
    }));
  }
  restored.push({
    id: `task-assistant-${task.id}`,
    role: "assistant",
    text: "",
    attachments: [],
    tools: [],
    reasoning: [],
    warnings: [],
    status: "streaming",
    taskId: task.id,
  });
  return restored;
}

export function restoreHistoryMessages(rows: any[], jobs: RetainedTask[] = []): ChatMessage[] {
  const restored: ChatMessage[] = [];

  for (let index = 0; index < rows.length; index++) {
    const row = rows[index];
    const role = row?.role || row?.kind;
    if (role !== "user" && role !== "assistant") continue;

    const text =
      typeof row?.text === "string"
        ? row.text
        : typeof row?.content === "string"
          ? row.content
          : "";
    const calls: any[] = Array.isArray(row?.tool_calls)
      ? row.tool_calls
      : [];
    const results: any[] = Array.isArray(row?.tool_results)
      ? row.tool_results
      : [];

    for (const result of results) {
      const requestedId =
        typeof result?.tool_use_id === "string" ? result.tool_use_id : "";
      for (
        let restoredIndex = restored.length - 1;
        restoredIndex >= 0;
        restoredIndex--
      ) {
        const previous = restored[restoredIndex];
        if (previous.role !== "assistant") continue;
        const tool = requestedId ? previous.tools.find((candidate) => candidate.id === requestedId) : undefined;
        if (tool) {
          tool.isError = result?.is_error === true;
          tool.finished = true;
          tool.phase = tool.isError ? "failed" : "completed";
          break;
        }
      }
    }

    // Provider protocols store tool results as user-role messages, but they
    // belong to the preceding assistant turn rather than user-authored prose.
    if (role === "user" && results.length > 0 && !text.trim()) continue;

    const rowId = String(row?.id ?? index);
    const tools: ToolCall[] = calls.map((call, callIndex) => ({
      id: String(call?.id || `${rowId}-${callIndex}`),
      name: String(call?.name || "tool"),
      finished: false,
    }));
    restored.push({
      id: rowId,
      role,
      text,
      attachments: [],
      tools,
      reasoning: [],
      warnings: [],
      status: "done",
      ...(typeof row?.task_id === "string" ? { taskId: row.task_id } : {}),
    });
  }

  // Missing legacy result identities are unconfirmed, never inferred success.
  for (const message of restored) {
    for (const tool of message.tools) {
      if (!tool.finished) tool.phase = "unconfirmed";
    }
  }

  for (const job of jobs) {
    const presentation = job.presentation;
    if (!presentation) continue;
    const bound = restored.filter((message) => message.taskId === job.id);
    const user = bound.find((message) => message.role === "user");
    const assistant = bound.slice().reverse().find((message) => message.role === "assistant");
    if (user && Array.isArray(presentation.attachments)) {
      user.attachments = presentation.attachments.map((attachment) => ({
        name: attachment.name, mediaType: attachment.media_type,
        bytes: attachment.bytes, sha256: attachment.sha256,
      }));
    }
    if (!assistant) continue;
    if (presentation.memory_sources) {
      try { assistant.memorySources = readMemorySources(presentation.memory_sources); }
      catch (error: unknown) { assistant.warnings.push(error instanceof Error ? error.message : "Retained memory references are unavailable."); }
    }
    if (presentation.evidence) {
      try {
        assistant.evidence = readEvidence(presentation.evidence);
      } catch (error: unknown) {
        assistant.warnings.push(error instanceof Error ? error.message : "Retained evidence is unavailable.");
      }
    }
    if (presentation.diagnostic) assistant.warnings.push(presentation.diagnostic);
    if (!presentation.available) {
      if (!presentation.diagnostic) assistant.warnings.push("Extended presentation details were not retained for this task.");
      continue;
    }
    for (const message of bound) message.tools = [];
    assistant.tools = presentation.tools.map((tool) => ({
      id: tool.id, name: tool.name,
      finished: tool.ok !== null,
      isError: tool.ok === false,
      phase: tool.ok === true ? "completed" : tool.ok === false ? "failed" : "unconfirmed",
      ...(tool.latency_ms !== null ? { latencyMs: tool.latency_ms } : {}),
      ...(tool.bytes_returned !== null ? { bytesReturned: tool.bytes_returned } : {}),
      ...(tool.ok === false && tool.error_preview ? { errorPreview: tool.error_preview } : {}),
    }));
    appendReasoningSummary(assistant, { summary: presentation.reasoning });
    if (presentation.usage) accumulateTurnUsage(assistant, { usage: presentation.usage });
  }

  return restored;
}
import { readEvidence, type EvidencePresentation } from "./chat-evidence";
import { readMemorySources, type MemoryAccess } from "./memory";
