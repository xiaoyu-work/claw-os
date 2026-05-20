/**
 * Chat surface. Replaces OA's `useChat` (which is tied to `@ai-sdk/react`
 * and Vercel's edge runtime) with a small custom hook that streams from
 * cos's `/api/chat` SSE endpoint.
 *
 * Visual structure mirrors OA's `session-chat-content.tsx`:
 *   - centered column with max-width
 *   - alternating message bubbles (user vs assistant)
 *   - inline tool-call cards
 *   - sticky composer at the bottom
 *
 * Frames consumed (from core/src/agent/web/routes/chat.rs):
 *   text, tool_use_start, tool_input_delta, tool_use, tool_result,
 *   tool_start, warning, turn_done, done, error.
 */

import { ArrowUp, Loader2, Square, Wrench, AlertTriangle } from "lucide-react";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { marked } from "marked";

import { api, streamSse } from "@/lib/api";
import { useRoute } from "@/lib/router";
import { cn } from "@/lib/utils";
import { Button } from "@/components/ui/button";
import { Card } from "@/components/ui/card";
import { Textarea } from "@/components/ui/textarea";

type ToolCall = {
  id: string;
  name: string;
  input?: any;
  inputAccum: string;
  result?: any;
  isError?: boolean;
  finished: boolean;
  progress?: string;
};

type Msg = {
  id: string;
  role: "user" | "assistant" | "system";
  text: string;
  tools: ToolCall[];
  warnings: string[];
  status: "streaming" | "done" | "error" | "interrupted";
  error?: string;
};

function uid() {
  return Math.random().toString(36).slice(2, 10);
}

export function ChatPage({ meta }: { meta: any }) {
  const route = useRoute();
  const sessionFromRoute = route.startsWith("/chat/") ? route.slice("/chat/".length) : "";
  const [sessionId, setSessionId] = useState<string>(sessionFromRoute);
  const [messages, setMessages] = useState<Msg[]>([]);
  const [input, setInput] = useState("");
  const [busy, setBusy] = useState(false);
  const abortRef = useRef<AbortController | null>(null);
  const scrollRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    setSessionId(sessionFromRoute);
    if (!sessionFromRoute) {
      setMessages([]);
      return;
    }
    let cancelled = false;
    api
      .get<{ messages?: any[] } | any[]>(`/api/sessions/${sessionFromRoute}/history`)
      .then((r) => {
        if (cancelled) return;
        const list: any[] = Array.isArray(r) ? r : r?.messages || [];
        const restored: Msg[] = list.map((m, i) => ({
          id: String(m.id ?? i),
          role: (m.role || m.kind || "assistant") as Msg["role"],
          text: m.text || m.content || "",
          tools: [],
          warnings: [],
          status: "done",
        }));
        setMessages(restored);
      })
      .catch(() => {});
    return () => {
      cancelled = true;
    };
  }, [sessionFromRoute]);

  useEffect(() => {
    const el = scrollRef.current;
    if (!el) return;
    el.scrollTo({ top: el.scrollHeight, behavior: "smooth" });
  }, [messages]);

  const send = useCallback(async () => {
    const text = input.trim();
    if (!text || busy) return;
    setInput("");
    const userMsg: Msg = {
      id: uid(),
      role: "user",
      text,
      tools: [],
      warnings: [],
      status: "done",
    };
    const asstMsg: Msg = {
      id: uid(),
      role: "assistant",
      text: "",
      tools: [],
      warnings: [],
      status: "streaming",
    };
    setMessages((m) => [...m, userMsg, asstMsg]);
    setBusy(true);
    const ac = new AbortController();
    abortRef.current = ac;

    try {
      await streamSse(
        "/api/chat",
        { prompt: text, session_id: sessionId || undefined },
        (event, data) => {
          setMessages((m) => {
            const copy = m.slice();
            const last = copy[copy.length - 1];
            if (!last || last.role !== "assistant") return m;
            applyFrame(last, event, data);
            if (event === "session" && data?.session_id) {
              setSessionId(String(data.session_id));
            }
            return copy;
          });
        },
        ac.signal,
      );
    } catch (e: any) {
      setMessages((m) => {
        const copy = m.slice();
        const last = copy[copy.length - 1];
        if (last && last.role === "assistant") {
          last.status = ac.signal.aborted ? "interrupted" : "error";
          last.error = ac.signal.aborted ? "stopped" : e?.message || "stream error";
        }
        return copy;
      });
    } finally {
      setBusy(false);
      abortRef.current = null;
    }
  }, [input, busy, sessionId]);

  const stop = useCallback(() => {
    abortRef.current?.abort();
  }, []);

  const placeholder = useMemo(() => {
    if (!meta) return "Ask cos anything…";
    return `Ask ${meta.model || meta.provider || "cos"}…`;
  }, [meta]);

  return (
    <div className="flex h-full min-h-0 flex-col">
      <div ref={scrollRef} className="flex-1 overflow-y-auto px-4">
        <div className="mx-auto flex max-w-3xl flex-col gap-6 py-8">
          {messages.length === 0 ? (
            <EmptyState meta={meta} />
          ) : (
            messages.map((m) => <Message key={m.id} m={m} />)
          )}
        </div>
      </div>
      <div className="border-t bg-background/80 px-4 py-3 backdrop-blur">
        <div className="mx-auto flex max-w-3xl items-end gap-2">
          <Textarea
            value={input}
            onChange={(e) => setInput(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "Enter" && !e.shiftKey) {
                e.preventDefault();
                send();
              }
            }}
            placeholder={placeholder}
            className="min-h-[44px] resize-none"
            rows={1}
          />
          {busy ? (
            <Button size="icon" variant="destructive" onClick={stop} title="Stop">
              <Square className="h-4 w-4" />
            </Button>
          ) : (
            <Button size="icon" onClick={send} disabled={!input.trim()} title="Send">
              <ArrowUp className="h-4 w-4" />
            </Button>
          )}
        </div>
      </div>
    </div>
  );
}

function applyFrame(msg: Msg, event: string, data: any) {
  switch (event) {
    case "text":
      msg.text += typeof data === "string" ? data : data?.delta || "";
      break;
    case "tool_use_start":
      msg.tools.push({
        id: String(data?.id || uid()),
        name: String(data?.name || "tool"),
        inputAccum: "",
        finished: false,
      });
      break;
    case "tool_input_delta": {
      const t = msg.tools.find((t) => t.id === String(data?.id));
      if (t) t.inputAccum += data?.delta || "";
      break;
    }
    case "tool_use": {
      const t =
        msg.tools.find((t) => t.id === String(data?.id)) ||
        msg.tools[msg.tools.length - 1];
      if (t) {
        t.input = data?.input ?? t.input;
        t.name = data?.name || t.name;
      }
      break;
    }
    case "tool_result": {
      const t = msg.tools.find((t) => t.id === String(data?.id));
      if (t) {
        t.result = data?.output ?? data?.content ?? data?.text;
        t.isError = !!data?.is_error;
        t.finished = true;
      }
      break;
    }
    case "tool_start": {
      const t = msg.tools[msg.tools.length - 1];
      if (t) t.progress = data?.message || data?.note || "";
      break;
    }
    case "warning":
      msg.warnings.push(String(data?.message || data));
      break;
    case "turn_done":
    case "done":
      msg.status = "done";
      break;
    case "error":
      msg.status = "error";
      msg.error = String(data?.message || data);
      break;
  }
}

function Message({ m }: { m: Msg }) {
  if (m.role === "user") {
    return (
      <div className="flex justify-end">
        <div className="max-w-[80%] rounded-2xl bg-primary px-4 py-2 text-primary-foreground">
          <p className="whitespace-pre-wrap break-words text-sm">{m.text}</p>
        </div>
      </div>
    );
  }
  return (
    <div className="flex flex-col gap-3">
      {m.tools.map((t) => (
        <ToolCard key={t.id} t={t} />
      ))}
      {m.warnings.map((w, i) => (
        <Card key={i} className="border-yellow-500/40 bg-yellow-500/5 px-3 py-2">
          <div className="flex items-center gap-2 text-xs text-yellow-600 dark:text-yellow-400">
            <AlertTriangle className="h-3.5 w-3.5" />
            {w}
          </div>
        </Card>
      ))}
      {m.text && (
        <div
          className="max-w-none text-sm leading-relaxed [&_pre]:overflow-x-auto [&_pre]:rounded [&_pre]:bg-muted [&_pre]:p-3 [&_code]:rounded [&_code]:bg-muted [&_code]:px-1 [&_code]:py-0.5 [&_code]:text-xs [&_pre>code]:bg-transparent [&_pre>code]:p-0 [&_a]:text-primary [&_a]:underline [&_h1]:my-2 [&_h1]:text-base [&_h1]:font-semibold [&_h2]:my-2 [&_h2]:text-sm [&_h2]:font-semibold [&_p]:my-2 [&_ul]:my-2 [&_ul]:list-disc [&_ul]:pl-5 [&_ol]:my-2 [&_ol]:list-decimal [&_ol]:pl-5"
          dangerouslySetInnerHTML={{ __html: marked.parse(m.text) as string }}
        />
      )}
      {m.status === "streaming" && (
        <div className="flex items-center gap-2 text-xs text-muted-foreground">
          <Loader2 className="h-3 w-3 animate-spin" />
          generating…
        </div>
      )}
      {m.status === "error" && m.error && (
        <p className="text-xs text-destructive">{m.error}</p>
      )}
    </div>
  );
}

function ToolCard({ t }: { t: ToolCall }) {
  const [open, setOpen] = useState(false);
  const summary = (() => {
    if (t.input && typeof t.input === "object") {
      const first = Object.entries(t.input)[0];
      if (first) return `${first[0]}: ${JSON.stringify(first[1]).slice(0, 60)}`;
    }
    return t.progress || (t.finished ? "complete" : "running…");
  })();
  return (
    <Card className="border-muted px-3 py-2 text-xs">
      <button
        type="button"
        className="flex w-full items-center justify-between gap-2"
        onClick={() => setOpen((v) => !v)}
      >
        <span className="flex items-center gap-2">
          <Wrench className={cn("h-3.5 w-3.5", t.isError && "text-destructive")} />
          <span className="font-mono font-semibold">{t.name}</span>
          <span className="text-muted-foreground">{summary}</span>
        </span>
        {!t.finished && <Loader2 className="h-3 w-3 animate-spin text-muted-foreground" />}
      </button>
      {open && (
        <div className="mt-2 grid gap-2">
          {t.input != null && (
            <pre className="overflow-x-auto rounded bg-muted px-2 py-1 text-[11px]">
              {JSON.stringify(t.input, null, 2)}
            </pre>
          )}
          {t.result != null && (
            <pre
              className={cn(
                "overflow-x-auto rounded px-2 py-1 text-[11px]",
                t.isError ? "bg-destructive/10 text-destructive" : "bg-muted",
              )}
            >
              {typeof t.result === "string" ? t.result : JSON.stringify(t.result, null, 2)}
            </pre>
          )}
        </div>
      )}
    </Card>
  );
}

function EmptyState({ meta }: { meta: any }) {
  return (
    <div className="grid min-h-[40vh] place-items-center text-center">
      <div className="grid gap-2">
        <h1 className="text-2xl font-semibold tracking-tight">cos agent</h1>
        <p className="text-sm text-muted-foreground">
          {meta?.provider ? (
            <>
              Talking to <span className="font-medium">{meta.provider}</span>
              {meta.model ? (
                <>
                  {" "}
                  · <span className="font-mono">{meta.model}</span>
                </>
              ) : null}
            </>
          ) : (
            "Configure a provider in Settings to start chatting."
          )}
        </p>
      </div>
    </div>
  );
}
