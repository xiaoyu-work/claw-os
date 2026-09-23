export type ToolCall = {
  id: string;
  name: string;
  isError?: boolean;
  finished: boolean;
};

export type TokenUsage = {
  inputTokens: number;
  outputTokens: number;
  cacheReadTokens: number;
  cacheWriteTokens: number;
};

export type ChatMessage = {
  id: string;
  role: "user" | "assistant";
  text: string;
  tools: ToolCall[];
  reasoning: string[];
  usage?: TokenUsage;
  warnings: string[];
  status: "streaming" | "done" | "error" | "interrupted";
  error?: string;
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

export function restoreHistoryMessages(rows: any[]): ChatMessage[] {
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
        const tool =
          (requestedId &&
            previous.tools.find(
              (candidate) => candidate.id === requestedId,
            )) ||
          previous.tools.find((candidate) => !candidate.finished);
        if (tool) {
          tool.isError = result?.is_error === true;
          tool.finished = true;
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
      tools,
      reasoning: [],
      warnings: [],
      status: "done",
    });
  }

  // A restored transcript is no longer executing. Legacy rows may omit a
  // matching result, but showing those calls as "running…" forever is wrong.
  for (const message of restored) {
    for (const tool of message.tools) tool.finished = true;
  }

  return restored;
}
