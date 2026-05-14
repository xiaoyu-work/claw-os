"use client";

import { type FormEvent, useCallback, useRef, useState } from "react";
import { Send, Sparkles } from "lucide-react";

import { Button } from "@/components/ui/button";
import { Textarea } from "@/components/ui/textarea";

type ChatRole = "user" | "assistant";

interface ChatMessage {
  id: string;
  role: ChatRole;
  content: string;
}

interface SseEvent {
  event: string;
  data: string;
}

function parseSseChunk(
  buffer: string,
): { events: SseEvent[]; remainder: string } {
  const events: SseEvent[] = [];
  let remainder = buffer;

  while (true) {
    const sep = remainder.indexOf("\n\n");
    if (sep === -1) {
      break;
    }

    const block = remainder.slice(0, sep);
    remainder = remainder.slice(sep + 2);

    let event = "message";
    const dataLines: string[] = [];
    for (const rawLine of block.split("\n")) {
      const line = rawLine.replace(/\r$/, "");
      if (line.startsWith("event:")) {
        event = line.slice("event:".length).trim();
      } else if (line.startsWith("data:")) {
        dataLines.push(line.slice("data:".length).trimStart());
      }
    }
    if (dataLines.length > 0) {
      events.push({ event, data: dataLines.join("\n") });
    }
  }

  return { events, remainder };
}

export function ChatShell() {
  const [messages, setMessages] = useState<ChatMessage[]>([]);
  const [input, setInput] = useState("");
  const [streaming, setStreaming] = useState(false);
  const abortRef = useRef<AbortController | null>(null);

  const sendPrompt = useCallback(async (prompt: string) => {
    const userMessage: ChatMessage = {
      id: `u-${Date.now()}`,
      role: "user",
      content: prompt,
    };
    const assistantId = `a-${Date.now()}`;
    setMessages((prev) => [
      ...prev,
      userMessage,
      { id: assistantId, role: "assistant", content: "" },
    ]);

    const controller = new AbortController();
    abortRef.current = controller;
    setStreaming(true);

    try {
      const response = await fetch("/api/chat", {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ prompt }),
        signal: controller.signal,
      });

      if (!response.ok || !response.body) {
        throw new Error(`Chat request failed: ${response.status}`);
      }

      const reader = response.body.getReader();
      const decoder = new TextDecoder();
      let buffer = "";
      let assistantText = "";

      while (true) {
        const { value, done } = await reader.read();
        if (done) {
          break;
        }
        buffer += decoder.decode(value, { stream: true });
        const { events, remainder } = parseSseChunk(buffer);
        buffer = remainder;
        for (const evt of events) {
          if (evt.event === "delta") {
            try {
              const payload = JSON.parse(evt.data) as { text?: string };
              if (payload.text) {
                assistantText += payload.text;
                setMessages((prev) =>
                  prev.map((m) =>
                    m.id === assistantId ? { ...m, content: assistantText } : m,
                  ),
                );
              }
            } catch {
              // ignore non-JSON delta payloads
            }
          }
        }
      }
    } catch (error) {
      if ((error as Error).name !== "AbortError") {
        setMessages((prev) =>
          prev.map((m) =>
            m.id === assistantId
              ? {
                  ...m,
                  content:
                    m.content ||
                    `Error talking to local agent: ${(error as Error).message}`,
                }
              : m,
          ),
        );
      }
    } finally {
      setStreaming(false);
      abortRef.current = null;
    }
  }, []);

  const handleSubmit = useCallback(
    (event: FormEvent<HTMLFormElement>) => {
      event.preventDefault();
      const trimmed = input.trim();
      if (!trimmed || streaming) {
        return;
      }
      setInput("");
      void sendPrompt(trimmed);
    },
    [input, sendPrompt, streaming],
  );

  return (
    <div className="flex min-h-screen flex-col bg-background text-foreground">
      <header className="border-b border-border px-6 py-4">
        <div className="flex items-center gap-3">
          <Sparkles className="size-5 text-primary" />
          <h1 className="text-lg font-semibold">Claw OS Agent</h1>
        </div>
      </header>

      <main className="mx-auto flex w-full max-w-3xl flex-1 flex-col gap-6 px-6 py-8">
        <div className="flex-1 space-y-4">
          {messages.length === 0 ? (
            <p className="text-muted-foreground text-sm">
              Say hi to your Claw OS agent.
            </p>
          ) : (
            messages.map((m) => (
              <div
                key={m.id}
                className={
                  m.role === "user"
                    ? "ml-auto max-w-[80%] rounded-2xl bg-primary px-4 py-2 text-primary-foreground"
                    : "mr-auto max-w-[80%] rounded-2xl bg-muted px-4 py-2 text-foreground"
                }
              >
                <p className="whitespace-pre-wrap text-sm leading-relaxed">
                  {m.content || (m.role === "assistant" ? "…" : "")}
                </p>
              </div>
            ))
          )}
        </div>

        <form
          onSubmit={handleSubmit}
          className="sticky bottom-6 flex items-end gap-2 rounded-2xl border border-border bg-card p-3 shadow-sm"
        >
          <Textarea
            value={input}
            onChange={(e) => setInput(e.target.value)}
            placeholder="Ask Claw OS anything…"
            rows={1}
            className="min-h-9 resize-none border-none bg-transparent shadow-none focus-visible:ring-0"
            onKeyDown={(e) => {
              if (e.key === "Enter" && !e.shiftKey) {
                e.preventDefault();
                (e.currentTarget.form ?? undefined)?.requestSubmit();
              }
            }}
          />
          <Button
            type="submit"
            size="icon"
            disabled={streaming || input.trim().length === 0}
          >
            <Send className="size-4" />
          </Button>
        </form>
      </main>
    </div>
  );
}
