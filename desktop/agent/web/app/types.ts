import type { UIMessage } from "ai";

/**
 * The agent chat UI consumes a thin SSE stream from `cos-agent-bridge`.
 * For now we use the generic `UIMessage` type from the `ai` SDK; richer
 * data parts (tool calls, structured outputs) can be layered back in
 * once the bridge starts emitting them.
 */
export type WebAgentUIMessage = UIMessage;
