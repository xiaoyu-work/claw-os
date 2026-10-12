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
 *   task, text, reasoning, tool_use_start, tool_use, tool_result, tool_start,
 *   warning, turn_done, done, error.
 */

import {
  AlertTriangle,
  ArrowUp,
  Brain,
  Gauge,
  ListPlus,
  Loader2,
  Paperclip,
  Square,
  Wrench,
  X,
} from "lucide-react";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";

import { api } from "@/lib/api";
import { applyFrame, markToolsUnconfirmed } from "@/lib/chat-presentation";
import { ChatEvidence } from "@/components/chat-evidence";
import {
  streamDurableTask,
  taskReplay,
  TaskStreamError,
  type TaskIdentity,
  type ChatHistoryResponse,
} from "@/lib/chat-stream";
import {
  formatAttachmentBytes,
  readImageAttachments,
  type PendingImageAttachment,
} from "@/lib/chat-attachments";
import {
  restoreForActiveTask,
  restoreHistoryMessages,
  type ChatMessage,
  type TokenUsage,
  type ToolCall,
} from "@/lib/chat-history";
import { renderSafeMarkdown } from "@/lib/safe-markdown";
import { useRoute, navigate } from "@/lib/router";
import { cn } from "@/lib/utils";
import { Button } from "@/components/ui/button";
import { Card } from "@/components/ui/card";
import { Textarea } from "@/components/ui/textarea";

type Msg = ChatMessage;

type HistoryResponse = ChatHistoryResponse;

const ACTIVE_TASK_STATUSES = new Set([
  "pending",
  "running",
  "waiting_approval",
]);

function uid() {
  return Math.random().toString(36).slice(2, 10);
}

export function ChatPage({ meta }: { meta: any }) {
  const route = useRoute();
  const sessionFromRoute = route.startsWith("/chat/") ? route.slice("/chat/".length) : "";
  const [sessionId, setSessionId] = useState<string>(sessionFromRoute);
  const [messages, setMessages] = useState<Msg[]>([]);
  const [input, setInput] = useState("");
  const [attachments, setAttachments] = useState<PendingImageAttachment[]>([]);
  const [attachmentError, setAttachmentError] = useState<string | null>(null);
  const [restoring, setRestoring] = useState(false);
  const [blockedByTask, setBlockedByTask] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [queueing, setQueueing] = useState(false);
  const [queuedCount, setQueuedCount] = useState(0);
  const [restoreVersion, setRestoreVersion] = useState(0);
  const [reconnecting, setReconnecting] = useState<string | null>(null);
  const abortRef = useRef<AbortController | null>(null);
  const fileInputRef = useRef<HTMLInputElement | null>(null);
  const taskIdRef = useRef<string>("");
  const queueTailTaskIdRef = useRef<string>("");
  const queuedTaskIdsRef = useRef<string[]>([]);
  const queueWatchersRef = useRef<Map<string, AbortController>>(new Map());
  const stopRequestedRef = useRef(false);
  const scrollRef = useRef<HTMLDivElement>(null);
  // When the server assigns a session id mid-stream we push the URL to
  // `/chat/<id>`. That route change fires the history-load effect below
  // again, which would otherwise overwrite the in-flight conversation
  // with a stale server snapshot. Stash the just-assigned id so the
  // effect can recognise it and skip the reload.
  const skipReloadFor = useRef<string>("");

  const replayTask = useCallback(async (task: TaskIdentity, signal: AbortSignal) => {
    const { history, job } = await taskReplay(task, signal);
    if (signal.aborted) return;
    setMessages(restoreForActiveTask(history.messages || [], job, history.jobs || []));
    taskIdRef.current = task.taskId;
  }, []);

  const recoveringNotice = useCallback((notice: string) => {
    setReconnecting(notice);
    setMessages((current) => {
      const copy = current.slice();
      const last = copy[copy.length - 1];
      if (last?.role === "assistant") markToolsUnconfirmed(last);
      return copy;
    });
  }, []);

  useEffect(() => () => {
    abortRef.current?.abort();
    for (const watcher of queueWatchersRef.current.values()) watcher.abort();
    queueWatchersRef.current.clear();
  }, []);

  useEffect(() => {
    setSessionId(sessionFromRoute);
    setBlockedByTask(null);
    if (skipReloadFor.current === sessionFromRoute && sessionFromRoute) {
      skipReloadFor.current = "";
      setRestoring(false);
      return;
    }
    abortRef.current?.abort();
    abortRef.current = null;
    taskIdRef.current = "";
    stopRequestedRef.current = false;
    setBusy(false);
    setReconnecting(null);
    for (const watcher of queueWatchersRef.current.values()) watcher.abort();
    queueWatchersRef.current.clear();
    if (!sessionFromRoute) {
      setRestoring(false);
      setAttachments([]);
      setAttachmentError(null);
      setQueuedCount(0);
      queueTailTaskIdRef.current = "";
      queuedTaskIdsRef.current = [];
      setMessages([]);
      return;
    }
    setAttachments([]);
    setAttachmentError(null);
    setQueuedCount(0);
    queueTailTaskIdRef.current = "";
    queuedTaskIdsRef.current = [];
    setRestoring(true);
    let cancelled = false;
    const controller = new AbortController();
    const restore = async () => {
      try {
        const response = await api.get<HistoryResponse | any[]>(
          `/api/sessions/${sessionFromRoute}/history`,
          { signal: controller.signal },
        );
        if (cancelled) return;
        const rows = Array.isArray(response)
          ? response
          : response?.messages || [];
        const jobs = Array.isArray(response)
          ? []
          : Array.isArray(response?.jobs)
            ? response.jobs
            : [];
        const activeTasks = jobs.filter((job) =>
          ACTIVE_TASK_STATUSES.has(job.status),
        );
        const activeTask = activeTasks[0];
        const retainedJobs = !Array.isArray(response) && response.task_bindings_complete === true ? jobs : [];
        setRestoring(false);
        if (!activeTask) {
          const restored = restoreHistoryMessages(rows, retainedJobs);
          if (!Array.isArray(response) && response.presentation_error) {
            restored[restored.length - 1]?.warnings.push(response.presentation_error);
          }
          setMessages(restored);
          return;
        }
        queueTailTaskIdRef.current =
          activeTasks[activeTasks.length - 1]?.id || activeTask.id;
        queuedTaskIdsRef.current = activeTasks
          .slice(1)
          .map((task) => task.id);
        setQueuedCount(queuedTaskIdsRef.current.length);
        if (!Array.isArray(response) && response.task_bindings_complete !== true) {
          const restored = restoreHistoryMessages(rows);
          restored.push({
            id: `task-unavailable-${activeTask.id}`,
            role: "assistant",
            text: "",
            attachments: [],
            tools: [],
            reasoning: [],
            warnings: [
              response.task_bindings_error ||
                "The active task cannot be safely reconstructed in Chat. Open Tasks to monitor or stop it.",
            ],
            status: "error",
            error: "Live task replay unavailable",
          });
          setMessages(restored);
          setBlockedByTask(activeTask.id);
          return;
        }

        setMessages(restoreForActiveTask(rows, activeTask, retainedJobs));
        setBusy(true);
        abortRef.current = controller;
        taskIdRef.current = activeTask.id;
        await streamDurableTask({
          path: `/api/tasks/${encodeURIComponent(activeTask.id)}/stream`,
          body: { cursor: 0 },
          task: { taskId: activeTask.id, sessionId: sessionFromRoute },
          signal: controller.signal,
          onReplay: (task) => replayTask(task, controller.signal),
          onRecovering: recoveringNotice,
          onFrame: (event, data) => {
            if (cancelled) return;
            setReconnecting(null);
            setMessages((current) => {
              const copy = current.slice();
              const last = copy[copy.length - 1];
              if (!last || last.role !== "assistant") return current;
              applyFrame(last, event, data);
              return copy;
            });
          },
        });
        if (
          !cancelled &&
          !controller.signal.aborted &&
          activeTasks.length > 1 &&
          !queueWatchersRef.current.has(activeTask.id)
        ) {
          setRestoreVersion((version) => version + 1);
        }
      } catch (error: any) {
        if (cancelled) return;
        if (error instanceof TaskStreamError && error.task) {
          setBlockedByTask(error.task.taskId);
        }
        setRestoring(false);
        if (controller.signal.aborted) {
          setMessages((current) => {
            const copy = current.slice();
            const last = copy[copy.length - 1];
            if (last?.role === "assistant" && last.status === "streaming") {
              last.status = "interrupted";
              last.error = "stopped";
              markToolsUnconfirmed(last);
            }
            return copy;
          });
          return;
        }
        setMessages((current) => {
          const copy = current.slice();
          const last = copy[copy.length - 1];
          if (last?.role === "assistant" && last.status === "streaming") {
            last.status = "error";
            markToolsUnconfirmed(last);
            last.error = error?.message || "Failed to restore live task";
            return copy;
          }
          return [
            ...copy,
            {
              id: uid(),
              role: "assistant",
              text: "",
              attachments: [],
              tools: [],
              reasoning: [],
              warnings: [],
              status: "error",
              error: error?.message || "Failed to load session history",
            },
          ];
        });
      } finally {
        if (!cancelled && abortRef.current === controller) {
          setBusy(false);
          setReconnecting(null);
          abortRef.current = null;
          taskIdRef.current = "";
        }
      }
    };
    void restore();
    return () => {
      cancelled = true;
      controller.abort();
      if (abortRef.current === controller) {
        abortRef.current = null;
        taskIdRef.current = "";
      }
    };
  }, [sessionFromRoute, restoreVersion, replayTask, recoveringNotice]);

  useEffect(() => {
    const el = scrollRef.current;
    if (!el) return;
    el.scrollTo({ top: el.scrollHeight, behavior: "smooth" });
  }, [messages]);

  const cancelTask = useCallback(
    async (taskId: string, controller: AbortController) => {
      try {
        const result = await api.post<{
          cancelled?: boolean;
          cancel_requested?: boolean;
        }>(`/api/tasks/${encodeURIComponent(taskId)}/stop`);
        if (result.cancelled || result.cancel_requested) {
          controller.abort();
        }
      } catch (error: any) {
        setMessages((current) => {
          const copy = current.slice();
          const last = copy[copy.length - 1];
          if (last?.role === "assistant") {
            last.warnings.push(error?.message || "Failed to stop task");
          }
          return copy;
        });
      }
    },
    [],
  );

  const addAttachments = useCallback(
    async (files: File[]) => {
      if (files.length === 0) return;
      try {
        const added = await readImageAttachments(files, attachments);
        setAttachments((current) => [...current, ...added]);
        setAttachmentError(null);
      } catch (error: any) {
        setAttachmentError(error?.message || "Failed to attach image");
      }
    },
    [attachments],
  );

  const send = useCallback(async () => {
    const text = input.trim();
    if (!text || busy || restoring || blockedByTask) return;
    setInput("");
    setQueuedCount(0);
    queueTailTaskIdRef.current = "";
    queuedTaskIdsRef.current = [];
    const userMsg: Msg = {
      id: uid(),
      role: "user",
      text,
      attachments: attachments.map((attachment) => ({
        name: attachment.name,
        mediaType: attachment.mediaType,
        bytes: attachment.bytes,
        dataUrl: attachment.dataUrl,
      })),
      tools: [],
      reasoning: [],
      warnings: [],
      status: "done",
    };
    const asstMsg: Msg = {
      id: uid(),
      role: "assistant",
      text: "",
      attachments: [],
      tools: [],
      reasoning: [],
      warnings: [],
      status: "streaming",
    };
    const submittedAttachments = attachments;
    setMessages((m) => [...m, userMsg, asstMsg]);
    setAttachments([]);
    setAttachmentError(null);
    setBusy(true);
    const ac = new AbortController();
    abortRef.current = ac;

    try {
      await streamDurableTask({
        path: "/api/chat",
        body: {
          prompt: text,
          session_id: sessionId || undefined,
          ...(submittedAttachments.length
            ? {
                attachments: submittedAttachments.map((attachment) => ({
                  name: attachment.name,
                  media_type: attachment.mediaType,
                  data: attachment.data,
                })),
              }
            : {}),
        },
        signal: ac.signal,
        onReplay: (task) => replayTask(task, ac.signal),
        onRecovering: recoveringNotice,
        onFrame: (event, data) => {
          if (ac.signal.aborted) return;
          setReconnecting(null);
          if (event === "task" && data?.task_id) {
            const taskId = String(data.task_id);
            taskIdRef.current = taskId;
            if (!queueTailTaskIdRef.current) {
              queueTailTaskIdRef.current = taskId;
            }
            if (stopRequestedRef.current) {
              void cancelTask(taskId, ac);
            }
          }
          setMessages((m) => {
            const copy = m.slice();
            const last = copy[copy.length - 1];
            if (!last || last.role !== "assistant") return m;
            applyFrame(last, event, data);
            if ((event === "session" || event === "done") && data?.session_id) {
              const sid = String(data.session_id);
              setSessionId(sid);
              // Reflect the new session in the URL so navigating away
              // and back (or hitting refresh) lands on the same chat.
              // Without this, the route stays at `/chat` and the next
              // mount restarts from scratch — losing all the messages
              // that just streamed in.
              if (route !== `/chat/${sid}`) {
                skipReloadFor.current = sid;
                navigate(`/chat/${sid}`);
              }
              // Tell the sidebar to refresh its session list so the
              // brand-new chat shows up immediately.
              window.dispatchEvent(new CustomEvent("cos:sessions-changed"));
            }
            return copy;
          });
        },
      });
    } catch (e: any) {
      if (abortRef.current !== ac) return;
      if (e instanceof TaskStreamError && e.task) setBlockedByTask(e.task.taskId);
      if (!taskIdRef.current) {
        setAttachments(submittedAttachments);
      }
      setMessages((m) => {
        const copy = m.slice();
        const last = copy[copy.length - 1];
        if (last && last.role === "assistant") {
          last.status = ac.signal.aborted ? "interrupted" : "error";
          markToolsUnconfirmed(last);
          last.error = ac.signal.aborted ? "stopped" : e?.message || "stream error";
        }
        return copy;
      });
    } finally {
      if (abortRef.current === ac) {
        setBusy(false);
        setReconnecting(null);
        abortRef.current = null;
        taskIdRef.current = "";
        stopRequestedRef.current = false;
      }
    }
  }, [
    input,
    attachments,
    busy,
    restoring,
    blockedByTask,
    sessionId,
    route,
    cancelTask,
    replayTask,
    recoveringNotice,
  ]);

  const watchPredecessor = useCallback((predecessorId: string, expectedSessionId: string) => {
    if (queueWatchersRef.current.has(predecessorId)) return;
    const controller = new AbortController();
    queueWatchersRef.current.set(predecessorId, controller);
    void (async () => {
      try {
        while (!controller.signal.aborted) {
          if (
            window.location.hash !==
            `#/chat/${encodeURIComponent(expectedSessionId)}`
          ) {
            return;
          }
          const task = await api.get<{ status?: string }>(
            `/api/tasks/${encodeURIComponent(predecessorId)}`,
            { signal: controller.signal },
          );
          if (["ok", "error", "cancelled"].includes(String(task.status || ""))) {
            if (
              window.location.hash ===
              `#/chat/${encodeURIComponent(expectedSessionId)}`
            ) {
              if (skipReloadFor.current === expectedSessionId) {
                skipReloadFor.current = "";
              }
              setRestoreVersion((version) => version + 1);
            }
            return;
          }
          await new Promise((resolve) => window.setTimeout(resolve, 1_000));
        }
      } catch (error: any) {
        if (!controller.signal.aborted) {
          setMessages((current) => {
            const copy = current.slice();
            const last = copy[copy.length - 1];
            if (last?.role === "assistant") {
              last.warnings.push(
                `Follow-up is queued, but automatic monitoring failed: ${
                  error?.message || "unknown error"
                }`,
              );
            }
            return copy;
          });
        }
      } finally {
        queueWatchersRef.current.delete(predecessorId);
      }
    })();
  }, []);

  const queueFollowUp = useCallback(async () => {
    const text = input.trim();
    const predecessorId =
      queueTailTaskIdRef.current || taskIdRef.current;
    if (!text || !busy || !predecessorId || queueing) return;

    setQueueing(true);
    try {
      const queuedAttachments = attachments;
      const task = await api.post<{ id?: string; session_id?: string }>(
        `/api/tasks/${encodeURIComponent(predecessorId)}/follow-up`,
        {
          prompt: text,
          ...(queuedAttachments.length
            ? {
                attachments: queuedAttachments.map((attachment) => ({
                  name: attachment.name,
                  media_type: attachment.mediaType,
                  data: attachment.data,
                })),
              }
            : {}),
        },
      );
      const taskId = String(task.id || "");
      if (!taskId) throw new Error("Queued task response omitted its id");
      const queuedSessionId = String(task.session_id || sessionId);
      queueTailTaskIdRef.current = taskId;
      queuedTaskIdsRef.current.push(taskId);
      setQueuedCount(queuedTaskIdsRef.current.length);
      setInput("");
      setAttachments([]);
      setAttachmentError(null);
      watchPredecessor(predecessorId, queuedSessionId);
    } catch (error: any) {
      setMessages((current) => {
        const copy = current.slice();
        const last = copy[copy.length - 1];
        if (last?.role === "assistant") {
          last.warnings.push(error?.message || "Failed to queue follow-up");
        }
        return copy;
      });
    } finally {
      setQueueing(false);
    }
  }, [attachments, busy, input, queueing, sessionId, watchPredecessor]);

  const stop = useCallback(() => {
    const controller = abortRef.current;
    if (!controller) return;
    const taskId = taskIdRef.current;
    if (!taskId) {
      stopRequestedRef.current = true;
      return;
    }
    void cancelTask(taskId, controller);
  }, [cancelTask]);

  const placeholder = useMemo(() => {
    if (!meta) return "Ask cos anything…";
    return `Ask ${meta.model || meta.provider || "cos"}…`;
  }, [meta]);

  return (
    <div className="flex h-full min-h-0 flex-col">
      {reconnecting && (
        <p role="status" className="border-b px-4 py-2 text-xs text-amber-600 dark:text-amber-400">
          {reconnecting}
        </p>
      )}
      {blockedByTask && !busy && (
        <div role="alert" className="flex items-center gap-2 border-b px-4 py-2 text-xs">
          <span>The task is still available. No work was resubmitted.</span>
          <Button variant="outline" size="sm" onClick={() => setRestoreVersion((version) => version + 1)}>
            Reconnect
          </Button>
          <Button variant="ghost" size="sm" onClick={() => navigate("/tasks")}>Open Tasks</Button>
        </div>
      )}
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
        <div className="mx-auto max-w-3xl">
          {attachments.length > 0 && (
            <div className="mb-2 flex flex-wrap gap-2" aria-label="Attached images">
              {attachments.map((attachment) => (
                <div
                  key={attachment.id}
                  className="group relative overflow-hidden rounded-md border bg-muted"
                >
                  <img
                    src={attachment.dataUrl}
                    alt={attachment.name}
                    className="h-16 w-16 object-cover"
                  />
                  <button
                    type="button"
                    aria-label={`Remove ${attachment.name}`}
                    className="absolute right-0.5 top-0.5 rounded bg-background/90 p-0.5 opacity-0 shadow group-hover:opacity-100 focus:opacity-100"
                    onClick={() =>
                      setAttachments((current) =>
                        current.filter((candidate) => candidate.id !== attachment.id),
                      )
                    }
                  >
                    <X className="h-3 w-3" />
                  </button>
                  <span className="block max-w-16 truncate px-1 py-0.5 text-[9px]">
                    {formatAttachmentBytes(attachment.bytes)}
                  </span>
                </div>
              ))}
            </div>
          )}
          <div
            className="flex items-end gap-2"
            onDragOver={(event) => event.preventDefault()}
            onDrop={(event) => {
              event.preventDefault();
              void addAttachments(Array.from(event.dataTransfer.files));
            }}
          >
            <input
              ref={fileInputRef}
              type="file"
              accept="image/png,image/jpeg,image/gif,image/webp"
              multiple
              className="hidden"
              aria-label="Attach images"
              onChange={(event) => {
                void addAttachments(Array.from(event.target.files || []));
                event.target.value = "";
              }}
            />
            <Button
              size="icon"
              variant="ghost"
              onClick={() => fileInputRef.current?.click()}
              disabled={restoring || queueing}
              title="Attach images"
            >
              <Paperclip className="h-4 w-4" />
            </Button>
            <Textarea
              value={input}
              onChange={(e) => setInput(e.target.value)}
              onPaste={(event) => {
                const files = Array.from(event.clipboardData.files);
                if (files.length > 0) void addAttachments(files);
              }}
              onKeyDown={(e) => {
                if (e.key === "Enter" && !e.shiftKey) {
                  e.preventDefault();
                  if (busy) void queueFollowUp();
                  else void send();
                }
              }}
              placeholder={placeholder}
              className="min-h-[44px] resize-none"
              rows={1}
            />
            {busy ? (
              <>
                {input.trim() && taskIdRef.current && (
                  <Button
                    size="icon"
                    variant="secondary"
                    onClick={() => void queueFollowUp()}
                    disabled={queueing}
                    title="Queue follow-up"
                  >
                    {queueing ? (
                      <Loader2 className="h-4 w-4 animate-spin" />
                    ) : (
                      <ListPlus className="h-4 w-4" />
                    )}
                  </Button>
                )}
                <Button size="icon" variant="destructive" onClick={stop} title="Stop">
                  <Square className="h-4 w-4" />
                </Button>
              </>
            ) : (
              <Button
                size="icon"
                onClick={send}
                disabled={!input.trim() || restoring || blockedByTask !== null}
                title={
                  restoring
                    ? "Restoring conversation"
                    : blockedByTask
                      ? "An active task must be monitored from Tasks"
                      : "Send"
                }
              >
                <ArrowUp className="h-4 w-4" />
              </Button>
            )}
          </div>
          {attachmentError && (
            <p role="alert" className="mt-2 text-xs text-destructive">
              {attachmentError}
            </p>
          )}
        </div>
        {queuedCount > 0 && (
          <p className="mx-auto mt-2 max-w-3xl text-xs text-muted-foreground">
            {queuedCount} follow-up{queuedCount === 1 ? "" : "s"} queued in this
            conversation.
          </p>
        )}
      </div>
    </div>
  );
}

function Message({ m }: { m: Msg }) {
  if (m.role === "user") {
    return (
      <div className="flex justify-end">
        <div className="max-w-[80%] rounded-2xl bg-primary px-4 py-2 text-primary-foreground">
          {m.attachments.length > 0 && (
            <div className="mb-2 grid grid-cols-2 gap-2">
              {m.attachments.map((attachment, index) =>
                attachment.dataUrl ? (
                  <img
                    key={`${attachment.name}-${index}`}
                    src={attachment.dataUrl}
                    alt={attachment.name}
                    className="max-h-48 rounded object-contain"
                  />
                ) : (
                  <div
                    key={`${attachment.name}-${index}`}
                    className="rounded border border-primary-foreground/20 px-2 py-1 text-xs"
                  >
                    {attachment.name} · {formatAttachmentBytes(attachment.bytes)}
                  </div>
                ),
              )}
            </div>
          )}
          <p className="whitespace-pre-wrap break-words text-sm">{m.text}</p>
        </div>
      </div>
    );
  }
  return (
    <div className="flex flex-col gap-3">
      {m.reasoning.length > 0 && <ReasoningSummary summaries={m.reasoning} />}
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
          dangerouslySetInnerHTML={{ __html: renderSafeMarkdown(m.text) }}
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
      {m.usage && <UsageSummary usage={m.usage} />}
      {m.evidence && <ChatEvidence evidence={m.evidence} />}
    </div>
  );
}

function ReasoningSummary({ summaries }: { summaries: string[] }) {
  return (
    <details className="rounded-md border border-muted px-3 py-2 text-xs">
      <summary className="flex cursor-pointer list-none items-center gap-2 text-muted-foreground">
        <Brain className="h-3.5 w-3.5" />
        Reasoning summary
      </summary>
      <div className="mt-2 grid gap-2 border-t pt-2 text-foreground">
        {summaries.map((summary, index) => (
          <p key={index} className="whitespace-pre-wrap break-words">
            {summary}
          </p>
        ))}
      </div>
    </details>
  );
}

function UsageSummary({ usage }: { usage: TokenUsage }) {
  const parts = [
    `${usage.inputTokens.toLocaleString()} in`,
    `${usage.outputTokens.toLocaleString()} out`,
  ];
  if (usage.cacheReadTokens > 0) {
    parts.push(`${usage.cacheReadTokens.toLocaleString()} cache read`);
  }
  if (usage.cacheWriteTokens > 0) {
    parts.push(`${usage.cacheWriteTokens.toLocaleString()} cache write`);
  }
  return (
    <div
      className="flex items-center gap-2 text-[11px] text-muted-foreground"
      title="Total provider usage across this response"
    >
      <Gauge className="h-3.5 w-3.5" />
      <span>{parts.join(" · ")}</span>
    </div>
  );
}

function ToolCard({ t }: { t: ToolCall }) {
  const status = t.phase || (t.isError ? "failed" : t.finished ? "completed" : "running…");
  const metrics = [
    t.latencyMs === undefined ? null : formatDuration(t.latencyMs),
    t.bytesReturned === undefined ? null : formatBytes(t.bytesReturned),
  ].filter((value): value is string => value !== null);
  return (
    <Card data-tool-id={t.id} className="border-muted px-3 py-2 text-xs">
      <div className="flex w-full items-center justify-between gap-2">
        <span className="flex items-center gap-2">
          <Wrench className={cn("h-3.5 w-3.5", t.isError && "text-destructive")} />
          <span className="font-mono font-semibold">{t.name}</span>
          <span className="text-muted-foreground">{status}</span>
        </span>
        <span className="flex items-center gap-2 text-muted-foreground">
          {metrics.length > 0 && (
            <span title="Result body remains private to the Agent runtime">
              {metrics.join(" · ")}
            </span>
          )}
          {(t.phase === "preparing" || t.phase === "running") && <Loader2 className="h-3 w-3 animate-spin" />}
        </span>
      </div>
      {t.errorPreview && (
        <details className="mt-2 border-t pt-2">
          <summary className="cursor-pointer text-destructive">
            Error details
          </summary>
          <pre className="mt-2 max-h-48 overflow-auto whitespace-pre-wrap break-words rounded bg-muted p-2 text-[11px] text-foreground">
            {t.errorPreview}
          </pre>
        </details>
      )}
    </Card>
  );
}

function formatDuration(milliseconds: number): string {
  if (milliseconds < 1_000) return `${milliseconds} ms`;
  return `${(milliseconds / 1_000).toFixed(milliseconds < 10_000 ? 1 : 0)} s`;
}

function formatBytes(bytes: number): string {
  if (bytes < 1_024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KiB`;
  return `${(bytes / (1024 * 1024)).toFixed(1)} MiB`;
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
