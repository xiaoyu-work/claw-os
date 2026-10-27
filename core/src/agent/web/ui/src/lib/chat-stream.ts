import { abortableDelay, api, ApiError, ApiTransportError, streamSse } from "./api";
import type { RetainedTask } from "./chat-history";

export type TaskIdentity = { taskId: string; sessionId: string };
export type ConversationTask = RetainedTask & { prompt: string };
export type ChatHistoryResponse = {
  session_id?: string;
  messages?: unknown[];
  jobs?: ConversationTask[];
  jobs_truncated?: boolean;
  task_bindings_complete?: boolean;
  task_bindings_error?: string | null;
  presentation_error?: string | null;
};

export class TaskStreamError extends Error {
  constructor(message: string, public readonly task: TaskIdentity | null) {
    super(message);
  }
}

export async function taskReplay(task: TaskIdentity, signal: AbortSignal) {
  const history = await api.get<ChatHistoryResponse>(
    `/api/sessions/${encodeURIComponent(task.sessionId)}/history`,
    { signal },
  );
  if (
    history.task_bindings_complete !== true ||
    history.jobs_truncated === true ||
    !Array.isArray(history.messages) ||
    !Array.isArray(history.jobs) ||
    (history.session_id !== undefined && history.session_id !== task.sessionId)
  ) {
    throw new TaskStreamError(
      history.task_bindings_error || "Task history cannot be safely reconstructed. Open Tasks to monitor it.",
      task,
    );
  }
  const job = history.jobs.find((job) => job.id === task.taskId);
  if (!job) throw new TaskStreamError("The retained history omitted this task. Open Tasks to monitor it.", task);
  return { history, job };
}

type StreamOptions = {
  path: string;
  body: unknown;
  task?: TaskIdentity;
  signal: AbortSignal;
  onFrame: Parameters<typeof streamSse>[2];
  onReplay: (task: TaskIdentity) => Promise<void>;
  onRecovering: (message: string) => void;
};

function transportFailure(error: unknown): boolean {
  return error instanceof ApiTransportError ||
    (error instanceof ApiError && [408, 429, 502, 503, 504].includes(error.status));
}

export async function streamDurableTask(options: StreamOptions): Promise<void> {
  let task = options.task || null;
  let taskId = task?.taskId || "";
  let sessionId = task?.sessionId || "";
  let attempts = 0;
  let recovering = false;
  while (!options.signal.aborted) {
    let terminal = false;
    try {
      if (recovering && task) await options.onReplay(task);
      await streamSse(
        recovering && task
          ? `/api/tasks/${encodeURIComponent(task.taskId)}/stream`
          : options.path,
        recovering ? { cursor: 0 } : options.body,
        (event, data) => {
          if (options.signal.aborted || terminal) return;
          if (task && event === "task" && data?.task_id !== task.taskId) {
            throw new TaskStreamError("The stream returned a different task identity.", task);
          }
          if (task && typeof data?.session_id === "string" && data.session_id !== task.sessionId) {
            throw new TaskStreamError("The stream returned a different conversation identity.", task);
          }
          if (event === "done" && (!task || data?.session_id !== task.sessionId)) {
            throw new TaskStreamError("Task completion omitted its verified conversation identity.", task);
          }
          if (event === "task" && typeof data?.task_id === "string") taskId = data.task_id;
          if (typeof data?.session_id === "string") sessionId = data.session_id;
          if (taskId && sessionId) task = { taskId, sessionId };
          options.onFrame(event, data);
          terminal = event === "done" || event === "error";
        },
        options.signal,
      );
      if (terminal) return;
      throw new ApiTransportError("The stream disconnected before task completion was confirmed.");
    } catch (error: unknown) {
      if (terminal) return;
      if (options.signal.aborted) throw error;
      if (!task || !transportFailure(error) || attempts >= 5) {
        throw new TaskStreamError(
          `${error instanceof Error ? error.message : "Stream failed"}${
            task ? " The task was not resubmitted." : " Submission was not retried; its outcome may be unknown. Check Tasks."
          }`,
          task,
        );
      }
      attempts += 1;
      options.onRecovering(`Connection lost. Reconnecting to the same task (${attempts}/5)...`);
      await abortableDelay(Math.min(500 * 2 ** (attempts - 1), 8_000), options.signal);
      recovering = true;
    }
  }
  throw options.signal.reason || new DOMException("Aborted", "AbortError");
}
