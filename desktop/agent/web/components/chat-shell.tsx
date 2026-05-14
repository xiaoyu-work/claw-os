"use client";

import {
  type FormEvent,
  useCallback,
  useEffect,
  useRef,
  useState,
} from "react";
import { Send } from "lucide-react";

import { BrandSymbol } from "@/components/brand";
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

function readOverlayMode(): boolean {
  if (typeof window === "undefined") return false;
  const params = new URLSearchParams(window.location.search);
  return params.get("overlay") === "1";
}

export function ChatShell() {
  const [messages, setMessages] = useState<ChatMessage[]>([]);
  const [input, setInput] = useState("");
  const [streaming, setStreaming] = useState(false);
  const [isOverlay, setIsOverlay] = useState(false);
  const abortRef = useRef<AbortController | null>(null);
  const inputRef = useRef<HTMLTextAreaElement | null>(null);

  useEffect(() => {
    setIsOverlay(readOverlayMode());
  }, []);

  useEffect(() => {
    if (!isOverlay) return;
    // Overlay mode: pressing Escape (anywhere — even when the
    // textarea has focus) closes the window. chromium --app= windows
    // accept window.close() for any page they themselves opened.
    const handler = (event: KeyboardEvent) => {
      if (event.key === "Escape") {
        event.preventDefault();
        window.close();
      }
    };
    window.addEventListener("keydown", handler);
    // Autofocus the input so the user can start typing immediately
    // after Super+A.
    inputRef.current?.focus();
    return () => window.removeEventListener("keydown", handler);
  }, [isOverlay]);

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

  if (isOverlay) {
    return (
      <div className="flex h-screen flex-col bg-background/95 text-foreground backdrop-blur-xl">
        <header className="flex items-center gap-2 border-b border-border/60 px-4 py-2">
          <BrandSymbol size={18} className="text-primary" />
          <span className="text-xs font-semibold tracking-wide text-muted-foreground uppercase">
            Claw OS Agent
          </span>
          <span className="ml-auto text-[10px] text-muted-foreground">
            Esc to close
          </span>
        </header>

        <main className="flex flex-1 flex-col gap-2 overflow-y-auto px-4 py-3">
          {messages.length === 0 ? (
            <p className="my-auto text-center text-sm text-muted-foreground">
              What can I help you with?
            </p>
          ) : (
            messages.map((m) => (
              <div
                key={m.id}
                className={
                  m.role === "user"
                    ? "ml-auto max-w-[85%] rounded-xl bg-primary px-3 py-1.5 text-sm text-primary-foreground"
                    : "mr-auto max-w-[85%] rounded-xl bg-muted px-3 py-1.5 text-sm text-foreground"
                }
              >
                <p className="whitespace-pre-wrap leading-snug">
                  {m.content || (m.role === "assistant" ? "…" : "")}
                </p>
              </div>
            ))
          )}
        </main>

        <form
          onSubmit={handleSubmit}
          className="flex items-end gap-2 border-t border-border/60 bg-card/80 px-3 py-2"
        >
          <Textarea
            ref={inputRef}
            value={input}
            onChange={(e) => setInput(e.target.value)}
            placeholder="Ask anything…"
            rows={1}
            className="min-h-8 resize-none border-none bg-transparent text-sm shadow-none focus-visible:ring-0"
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
            <Send className="size-3.5" />
          </Button>
        </form>
      </div>
    );
  }

  return (
    <div className="flex min-h-screen flex-col bg-background text-foreground">
      <header className="border-b border-border px-6 py-4">
        <div className="flex items-center gap-3">
          <BrandSymbol size={28} className="text-foreground" />
          <h1 className="text-lg font-semibold tracking-tight">
            Claw OS Agent
          </h1>
        </div>
      </header>

      <main className="mx-auto flex w-full max-w-3xl flex-1 flex-col gap-6 px-6 py-8">
        <div className="flex-1 space-y-4">
          {messages.length === 0 ? (
            <div className="flex h-full min-h-[40vh] flex-col items-center justify-center gap-4 text-center">
              <BrandSymbol
                size={56}
                aria-hidden
                className="text-foreground"
              />
              <div className="space-y-1">
                <p className="text-foreground text-base font-medium">
                  How can your agent help today?
                </p>
                <p className="text-muted-foreground text-sm">
                  Press <kbd className="rounded border border-border bg-muted px-1.5 py-0.5 font-mono text-xs">Super</kbd>
                  {" + "}
                  <kbd className="rounded border border-border bg-muted px-1.5 py-0.5 font-mono text-xs">A</kbd>
                  {" "}from anywhere to summon me.
                </p>
              </div>
            </div>
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
            ref={inputRef}
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
