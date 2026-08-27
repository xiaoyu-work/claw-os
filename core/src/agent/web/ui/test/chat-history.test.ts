import { describe, expect, test } from "bun:test";

import { restoreHistoryMessages } from "../src/lib/chat-history";

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
