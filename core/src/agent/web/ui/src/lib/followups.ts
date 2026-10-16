import { oneOf, record } from "./api-shapes";

export type FollowUp = { id: string; prompt: string; status: string; after_task_id: string; created_at: string };
export type FollowUpQueue = { task_id: string; session_id: string; tasks: FollowUp[]; truncated: boolean };
export function readFollowUps(value: unknown, taskId: string): FollowUpQueue {
  if (!record(value) || value.task_id !== taskId || typeof value.session_id !== "string" ||
      !Array.isArray(value.tasks) || typeof value.truncated !== "boolean") throw new Error("Follow-up queue has an invalid identity or shape.");
  const tasks = value.tasks.map((task: unknown) => {
    if (!record(task) || typeof task.id !== "string" || task.id === taskId ||
        typeof task.prompt !== "string" || typeof task.after_task_id !== "string" ||
        typeof task.created_at !== "string" || !oneOf(["pending", "running", "waiting_approval"], task.status)) {
      throw new Error("Follow-up queue has an invalid Job.");
    }
    return { id: task.id, prompt: task.prompt, after_task_id: task.after_task_id, created_at: task.created_at, status: task.status };
  });
  return { task_id: taskId, session_id: value.session_id, tasks, truncated: value.truncated };
}
