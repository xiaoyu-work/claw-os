import { describe, expect, test } from "bun:test";

import {
  accumulateTurnUsage,
  appendReasoningSummary,
  restoreHistoryMessages,
  type ChatMessage,
} from "../src/lib/chat-history";

describe("restoreHistoryMessages", () => {
  test("hides prompt injections and folds tool-result rows into tool cards", () => {
    const messages = restoreHistoryMessages([
      { id: 1, role: "user", text: "hi" },
      {
        id: 2,
        role: "injected",
        text: "[skills_catalog]\nsecret prompt context",
      },
      {
        id: 3,
        role: "assistant",
        text: "",
        tool_calls: [{ name: "cos_sysinfo" }],
      },
      {
        id: 4,
        role: "user",
        text: "",
        tool_results: [{ text: "large raw result", is_error: false }],
      },
      { id: 5, role: "system", text: "system prompt" },
      { id: 6, role: "assistant", text: "Hello!" },
    ]);

    expect(messages).toHaveLength(3);
    expect(messages.map((message) => message.text)).toEqual([
      "hi",
      "",
      "Hello!",
    ]);
    expect(messages[1].tools).toEqual([
      {
        id: "3-0",
        name: "cos_sysinfo",
        isError: false,
        finished: true,
      },
    ]);
    expect(JSON.stringify(messages)).not.toContain("skills_catalog");
    expect(JSON.stringify(messages)).not.toContain("system prompt");
    expect(JSON.stringify(messages)).not.toContain("large raw result");
  });
});

describe("live chat presentation", () => {
  test("collects reasoning summaries and provider usage across turns", () => {
    const message: ChatMessage = {
      id: "assistant",
      role: "assistant",
      text: "",
      tools: [],
      reasoning: [],
      warnings: [],
      status: "streaming",
    };

    appendReasoningSummary(message, { summary: ["Checking context", ""] });
    appendReasoningSummary(message, {
      summary: ["Checking context", "Comparing sources"],
    });
    accumulateTurnUsage(message, {
      usage: {
        input_tokens: 100,
        output_tokens: 20,
        cache_read_tokens: 40,
        cache_write_tokens: 0,
      },
    });
    accumulateTurnUsage(message, {
      usage: {
        input_tokens: 30,
        output_tokens: 10,
        cache_read_tokens: 0,
        cache_write_tokens: 5,
      },
    });

    expect(message.reasoning).toEqual([
      "Checking context",
      "Comparing sources",
    ]);
    expect(message.usage).toEqual({
      inputTokens: 130,
      outputTokens: 30,
      cacheReadTokens: 40,
      cacheWriteTokens: 5,
    });
  });

  test("ignores malformed presentation values", () => {
    const message: ChatMessage = {
      id: "assistant",
      role: "assistant",
      text: "",
      tools: [],
      reasoning: [],
      warnings: [],
      status: "streaming",
    };

    appendReasoningSummary(message, { summary: ["", 12, null] });
    accumulateTurnUsage(message, {
      usage: {
        input_tokens: -1,
        output_tokens: "4",
        cache_read_tokens: Number.MAX_SAFE_INTEGER + 1,
      },
    });

    expect(message.reasoning).toEqual([]);
    expect(message.usage).toEqual({
      inputTokens: 0,
      outputTokens: 0,
      cacheReadTokens: 0,
      cacheWriteTokens: 0,
    });
  });
});
