import { afterEach, expect, spyOn, test } from "bun:test";

import { streamDurableTask, taskReplay, TaskStreamError } from "../src/lib/chat-stream";

const spies: ReturnType<typeof spyOn>[] = [];
afterEach(() => {
  for (const spy of spies.splice(0)) spy.mockRestore();
});

function sse(events: [string, unknown][], separator = "\n\n") {
  return new Response(events.map(([event, data]) =>
    `event: ${event}\ndata: ${JSON.stringify(data)}`,
  ).join(separator) + separator, { headers: { "content-type": "text/event-stream" } });
}

test("transport loss replays the same verified task without submitting again", async () => {
  const paths: string[] = [];
  const fetchSpy = spyOn(globalThis, "fetch").mockImplementation(async (input) => {
    const path = String(input);
    paths.push(path);
    if (path === "/api/chat") return sse([
      ["task", { task_id: "job-1", session_id: "session-1" }],
      ["text", { delta: "partial" }],
    ]);
    if (path.endsWith("/history")) return Response.json({
      task_bindings_complete: true, messages: [],
      jobs: [{ id: "job-1", prompt: "hello", status: "running" }],
    });
    if (path === "/api/tasks/job-1/stream") return sse([
      ["text", { delta: "complete" }],
      ["done", { session_id: "session-1" }],
    ]);
    throw new Error(`unexpected request ${path}`);
  });
  spies.push(fetchSpy);
  let text = "";
  await streamDurableTask({
    path: "/api/chat", body: { prompt: "hello" }, signal: new AbortController().signal,
    onFrame: (event, data) => { if (event === "text") text += data.delta; },
    onReplay: async (task) => {
      await taskReplay(task, new AbortController().signal);
      text = "";
    },
    onRecovering: () => {},
  });
  expect(text).toBe("complete");
  expect(paths).toEqual(["/api/chat", "/api/sessions/session-1/history", "/api/tasks/job-1/stream"]);
});

test("unknown submission and server terminal errors never resubmit", async () => {
  const fetchSpy = spyOn(globalThis, "fetch").mockResolvedValue(sse([]));
  spies.push(fetchSpy);
  const options = {
    path: "/api/chat", body: {}, signal: new AbortController().signal,
    onFrame: () => {}, onReplay: async () => {}, onRecovering: () => {},
  };
  await expect(streamDurableTask(options)).rejects.toBeInstanceOf(TaskStreamError);
  expect(fetchSpy).toHaveBeenCalledTimes(1);
  fetchSpy.mockResolvedValue(sse([["error", { error: "task failed" }]]));
  await streamDurableTask(options);
  expect(fetchSpy).toHaveBeenCalledTimes(2);
});

test("replay refuses incomplete bindings and final CRLF frames are consumed", async () => {
  const fetchSpy = spyOn(globalThis, "fetch").mockResolvedValue(Response.json({
    task_bindings_complete: false, messages: [], jobs: [],
  }));
  spies.push(fetchSpy);
  await expect(taskReplay(
    { taskId: "job-1", sessionId: "session-1" }, new AbortController().signal,
  )).rejects.toBeInstanceOf(TaskStreamError);
  fetchSpy.mockResolvedValue(new Response('event: done\r\ndata: {"session_id":"session-1"}'));
  const events: string[] = [];
  await streamDurableTask({
    path: "/api/tasks/job-1/stream", body: {}, signal: new AbortController().signal,
    task: { taskId: "job-1", sessionId: "session-1" },
    onFrame: (event) => { events.push(event); },
    onReplay: async () => { throw new Error("terminal stream must not replay"); },
    onRecovering: () => {},
  });
  expect(events).toEqual(["done"]);
});
