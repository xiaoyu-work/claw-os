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
import { useCallback, useEffect, useRef, useState } from "react";

import { api } from "@/lib/api";
import { applyFrame, markToolsUnconfirmed } from "@/lib/chat-presentation";
import { ChatEvidence } from "@/components/chat-evidence";
import { ApprovalViews } from "@/components/chat-approvals";
import { FollowUpQueuePanel } from "@/components/followup-queue";
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
  keepStoppedReplies,
  restoreForActiveTask,
  restoreHistoryMessages,
  type ChatMessage,
  type TokenUsage,
  type ToolCall,
} from "@/lib/chat-history";
import { renderSafeMarkdown } from "@/lib/safe-markdown";
import { notificationViewer, useConversationView } from "@/lib/conversation-view";
import { useRoute, navigate } from "@/lib/router";
import { workspaceId } from "@/lib/workspaces";
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
  const [workspace, setWorkspace] = useState<string | null>(null);
  const [messages, setMessages] = useState<Msg[]>([]);
  const [input, setInput] = useState("");
  const [attachments, setAttachments] = useState<PendingImageAttachment[]>([]);
  const [attachmentError, setAttachmentError] = useState<string | null>(null);
  const [readingAttachments, setReadingAttachments] = useState(false);
  const attachmentRead = useRef({ generation: 0, busy: false });
  const [restoring, setRestoring] = useState(false);
  const [blockedByTask, setBlockedByTask] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [stopping, setStopping] = useState(false);
  const [queueing, setQueueing] = useState(false);
  const [queuedCount, setQueuedCount] = useState(0);
  const [restoreVersion, setRestoreVersion] = useState(0);
  const [reconnecting, setReconnecting] = useState<string | null>(null);
  const abortRef = useRef<AbortController | null>(null);
  const fileInputRef = useRef<HTMLInputElement | null>(null);
  const taskIdRef = useRef<string>("");
  const queueTailTaskIdRef = useRef<string>("");
  const queuedTaskIdsRef = useRef<string[]>([]);
  const stopRequestedRef = useRef<"awaiting-id" | "sent" | null>(null);
  const scrollRef = useRef<HTMLDivElement>(null);
  // When the server assigns a session id mid-stream we push the URL to
  // `/chat/<id>`. That route change fires the history-load effect below
  // again, which would otherwise overwrite the in-flight conversation
  // with a stale server snapshot. Stash the just-assigned id so the
  // effect can recognise it and skip the reload.
  const skipReloadFor = useRef<string>("");
  useConversationView(sessionId, !restoring && !blockedByTask && !reconnecting);

  const replayTask = useCallback(async (task: TaskIdentity, signal: AbortSignal) => {
    const { history, job } = await taskReplay(task, signal);
    if (signal.aborted) return;
    const restored = restoreForActiveTask(history.messages || [], job, history.jobs || []);
    setMessages((current) => {
      const liveReply = current[current.length - 1];
      if (liveReply?.taskId === task.taskId && liveReply.frozenText !== undefined) {
        restored[restored.length - 1].frozenText = liveReply.frozenText;
      }
      return keepStoppedReplies(current, restored, history.jobs || []);
    });
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

  const reloadForFollowUps = useCallback(() => {
    skipReloadFor.current = "";
    setRestoring(true);
    setRestoreVersion((version) => version + 1);
  }, []);

  const finishStream = useCallback((controller: AbortController, jobEnded: boolean) => {
    if (abortRef.current !== controller) return;
    setBusy(false);
    setStopping(false);
    setReconnecting(null);
    abortRef.current = null;
    taskIdRef.current = "";
    stopRequestedRef.current = null;
    if (jobEnded && queuedTaskIdsRef.current.length > 0) reloadForFollowUps();
  }, [reloadForFollowUps]);

  useEffect(() => () => {
    attachmentRead.current.generation += 1;
    abortRef.current?.abort();
  }, []);

  useEffect(() => {
    setSessionId(sessionFromRoute);
    setBlockedByTask(null);
    if (skipReloadFor.current === sessionFromRoute && sessionFromRoute) {
      skipReloadFor.current = "";
      setRestoring(false);
      return;
    }
    attachmentRead.current = { generation: attachmentRead.current.generation + 1, busy: false };
    setReadingAttachments(false);
    abortRef.current?.abort();
    abortRef.current = null;
    taskIdRef.current = "";
    stopRequestedRef.current = null;
    setBusy(false);
    setStopping(false);
    setReconnecting(null);
    if (!sessionFromRoute) {
      setWorkspace(null);
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
    let jobEnded = false;
    const controller = new AbortController();
    const restore = async () => {
      try {
        const response = await api.get<HistoryResponse | any[]>(
          `/api/sessions/${sessionFromRoute}/history`,
          { signal: controller.signal },
        );
        if (cancelled) return;
        if (!Array.isArray(response)) setWorkspace(response.workspace_id == null ? null : workspaceId(response.workspace_id));
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
          setMessages((current) => keepStoppedReplies(current, restored, retainedJobs));
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

        const restored = restoreForActiveTask(rows, activeTask, retainedJobs);
        setMessages((current) => keepStoppedReplies(current, restored, retainedJobs));
        setBusy(true);
        abortRef.current = controller;
        taskIdRef.current = activeTask.id;
        jobEnded = await streamDurableTask({
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
        if (!cancelled) finishStream(controller, jobEnded);
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
  }, [sessionFromRoute, restoreVersion, replayTask, recoveringNotice, finishStream]);

  useEffect(() => {
    const el = scrollRef.current;
    if (!el) return;
    el.scrollTo({ top: el.scrollHeight, behavior: "smooth" });
  }, [messages]);

  const cancelTask = useCallback(
    async (taskId: string, controller: AbortController) => {
      try {
        const result = await api.post<{
          id?: string;
          status?: string;
          cancelled?: boolean;
          cancel_requested?: boolean;
        }>(`/api/tasks/${encodeURIComponent(taskId)}/stop`);
        if (result.id !== taskId || (!result.cancelled && !result.cancel_requested &&
          !["ok", "error", "cancelled"].includes(result.status ?? ""))) {
          throw new Error("Stop was not acknowledged.");
        }
      } catch (error: any) {
        if (abortRef.current !== controller || taskIdRef.current !== taskId) return;
        stopRequestedRef.current = null;
        setStopping(false);
        setMessages((current) => {
          const copy = current.slice();
          const last = copy[copy.length - 1];
          if (last?.role === "assistant") {
            copy[copy.length - 1] = {
              ...last,
              frozenText: undefined,
              warnings: [...last.warnings, error?.message || "Failed to stop task"],
            };
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
      if (attachmentRead.current.busy || queueing || restoring) {
        setAttachmentError("Wait for the current image read or submission before adding more images.");
        return;
      }
      const generation = attachmentRead.current.generation;
      attachmentRead.current.busy = true;
      setReadingAttachments(true);
      try {
        const added = await readImageAttachments(files, attachments);
        if (generation !== attachmentRead.current.generation) return;
        setAttachments((current) => [...current, ...added]);
        setAttachmentError(null);
      } catch (error: any) {
        if (generation === attachmentRead.current.generation) {
          setAttachmentError(error?.message || "Failed to attach image");
        }
      } finally {
        if (generation === attachmentRead.current.generation) {
          attachmentRead.current.busy = false;
          setReadingAttachments(false);
        }
      }
    },
    [attachments, queueing, restoring],
  );

  const send = useCallback(async () => {
    const text = input.trim();
    if (!text || busy || queueing || restoring || blockedByTask || attachmentRead.current.busy) return;
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
    let jobEnded = false;

    try {
      jobEnded = await streamDurableTask({
        path: "/api/chat",
        body: {
          prompt: text,
          session_id: sessionId || undefined,
          notification_viewer: notificationViewer(),
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
            if (stopRequestedRef.current === "awaiting-id") {
              stopRequestedRef.current = "sent";
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
      finishStream(ac, jobEnded);
    }
  }, [
    input,
    attachments,
    busy,
    queueing,
    restoring,
    blockedByTask,
    sessionId,
    route,
    cancelTask,
    replayTask,
    recoveringNotice,
    finishStream,
  ]);

  const queueFollowUp = useCallback(async () => {
    const text = input.trim();
    const predecessorId =
      queueTailTaskIdRef.current || taskIdRef.current;
    if (!text || !busy || !predecessorId || queueing || stopping || attachmentRead.current.busy) return;

    setQueueing(true);
    try {
      const queuedAttachments = attachments;
      const task = await api.post<{ id?: string; session_id?: string }>(
        `/api/tasks/${encodeURIComponent(predecessorId)}/follow-up`,
        {
          prompt: text,
          notification_viewer: notificationViewer(!restoring && !blockedByTask && !reconnecting),
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
      if (window.location.hash !== `#/chat/${encodeURIComponent(queuedSessionId)}`) return;
      queueTailTaskIdRef.current = taskId;
      queuedTaskIdsRef.current.push(taskId);
      setQueuedCount(queuedTaskIdsRef.current.length);
      setInput((current) => (current === input ? "" : current));
      setAttachments([]);
      setAttachmentError(null);
      if (!taskIdRef.current) reloadForFollowUps();
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
  }, [attachments, blockedByTask, busy, input, queueing, reconnecting, restoring, sessionId, stopping, reloadForFollowUps]);

  const stop = useCallback(() => {
    const controller = abortRef.current;
    if (!controller || stopRequestedRef.current) return;
    const taskId = taskIdRef.current;
    stopRequestedRef.current = taskId ? "sent" : "awaiting-id";
    setStopping(true);
    setMessages((current) => {
      const last = current[current.length - 1];
      return last?.role === "assistant"
        ? [...current.slice(0, -1), { ...last, frozenText: last.text }]
        : current;
    });
    if (taskId) void cancelTask(taskId, controller);
  }, [cancelTask]);

  return (
    <div className="flex h-full min-h-0 flex-col">
      <p className="border-b px-4 py-1 text-xs text-muted-foreground">Context domain: {workspace ?? "Personal / unassigned"}. Changing the Workspace picker starts another conversation; existing history does not move.</p>
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
          {busy && (messages[messages.length - 1]?.taskId || taskIdRef.current) && (
            <div className="mb-3 max-h-72 overflow-y-auto">
              <ApprovalViews compact taskId={messages[messages.length - 1]?.taskId || taskIdRef.current} />
            </div>
          )}
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
              disabled={restoring || queueing || readingAttachments}
              title="Attach up to 4 original images, 20 MiB total"
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
                if (e.nativeEvent.isComposing || e.nativeEvent.keyCode === 229) return;
                if (e.key === "Enter" && !e.shiftKey) {
                  e.preventDefault();
                  if (busy) void queueFollowUp();
                  else void send();
                }
              }}
              aria-label="Message"
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
                    disabled={queueing || stopping || readingAttachments}
                    title="Queue follow-up"
                  >
                    {queueing ? (
                      <Loader2 className="h-4 w-4 animate-spin" />
                    ) : (
                      <ListPlus className="h-4 w-4" />
                    )}
                  </Button>
                )}
                <Button
                  size="icon"
                  variant="destructive"
                  onClick={stop}
                  disabled={stopping}
                  title={stopping ? "Stopping current task" : "Stop current task (keeps queued follow-ups)"}
                  aria-label={stopping ? "Stopping current task" : "Stop current task"}
                >
                  {stopping ? <Loader2 className="h-4 w-4 animate-spin" /> : <Square className="h-4 w-4" />}
                </Button>
              </>
            ) : (
              <Button
                size="icon"
                onClick={send}
                disabled={!input.trim() || queueing || restoring || blockedByTask !== null || readingAttachments}
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
          {readingAttachments && (
            <p className="mt-2 text-[11px] text-muted-foreground">
              Reading original images...
            </p>
          )}
          {attachmentError && (
            <p role="alert" className="mt-2 text-xs text-destructive">
              {attachmentError}
            </p>
          )}
          {busy && (messages[messages.length - 1]?.taskId || taskIdRef.current) && (
            <FollowUpQueuePanel key={messages[messages.length - 1]?.taskId || taskIdRef.current} taskId={messages[messages.length - 1]?.taskId || taskIdRef.current} version={queuedCount} />
          )}
        </div>
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
  const visibleText = m.frozenText ?? m.text;
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
      {visibleText && (
        <div
          className="max-w-none text-sm leading-relaxed [&_pre]:overflow-x-auto [&_pre]:rounded [&_pre]:bg-muted [&_pre]:p-3 [&_code]:rounded [&_code]:bg-muted [&_code]:px-1 [&_code]:py-0.5 [&_code]:text-xs [&_pre>code]:bg-transparent [&_pre>code]:p-0 [&_a]:text-primary [&_a]:underline [&_h1]:my-2 [&_h1]:text-base [&_h1]:font-semibold [&_h2]:my-2 [&_h2]:text-sm [&_h2]:font-semibold [&_p]:my-2 [&_ul]:my-2 [&_ul]:list-disc [&_ul]:pl-5 [&_ol]:my-2 [&_ol]:list-decimal [&_ol]:pl-5"
          dangerouslySetInnerHTML={{ __html: renderSafeMarkdown(visibleText) }}
        />
      )}
      {m.status === "streaming" && (
        <div className="flex items-center gap-2 text-xs text-muted-foreground">
          <Loader2 className="h-3 w-3 animate-spin" />
          {m.frozenText === undefined ? "generating…" : "Stopping…"}
        </div>
      )}
      {m.status === "interrupted" && <p className="text-xs text-muted-foreground">Stopped</p>}
      {m.status === "error" && m.error && (
        <p className="text-xs text-destructive">{m.error}</p>
      )}
      {m.usage && <UsageSummary usage={m.usage} />}
      {m.evidence && <ChatEvidence evidence={m.evidence} />}
      {!!m.memorySources?.length && <details className="rounded border border-muted px-3 py-2 text-xs">
        <summary className="cursor-pointer">Memory retrieved by this task ({m.memorySources.length})</summary>
        <div className="mt-2 grid gap-2">
          {m.memorySources.map((source) => <p key={`${source.source}-${source.revision}`} className="break-all">{source.source} &middot; revision {source.revision}</p>)}
          <p className="text-muted-foreground">Retrieved references do not prove that the answer used them correctly or that remembered state is current.</p>
          <Button size="sm" variant="outline" onClick={() => navigate("/memory")}>Open Memory</Button>
        </div>
      </details>}
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
