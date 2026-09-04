import { after, test } from "node:test";
import assert from "node:assert/strict";
import {
  mkdtempSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import { join } from "node:path";
import { PassThrough, Writable } from "node:stream";

import {
  App,
  CALL_CONTEXT_META_KEY,
  ERR_INVALID_PARAMS,
  ERR_PARSE,
  MAX_LINE_BYTES,
  ManifestError,
  currentContext,
} from "./mcp";

type Frame = Record<string, unknown>;

const temporaryDirectories: string[] = [];

after(() => {
  for (const directory of temporaryDirectories) {
    rmSync(directory, { recursive: true, force: true });
  }
});

function writeManifest(value: unknown): string {
  const directory = mkdtempSync(join(process.cwd(), ".mcp-test-"));
  temporaryDirectories.push(directory);
  const path = join(directory, "app.json");
  writeFileSync(path, JSON.stringify(value));
  return path;
}

function manifestPath(tools: unknown[]): string {
  return writeManifest({
    schema_version: 2,
    id: "test_app",
    version: "1.2.3",
    name: { en: "Test App" },
    mcp: {
      transport: "stdio",
      tools,
    },
  });
}

function protocolOnlyApp(): App {
  const app = App.fromManifest(manifestPath([
    {
      name: "test_app.noop",
      summary: { en: "No operation." },
    },
  ]));
  app.tool("test_app.noop", () => null);
  return app;
}

function context(callId: string): Record<string, unknown> {
  return {
    wire_version: 1,
    call_id: callId,
    trace_id: `trace-${callId}`,
    depth: 1,
    session_id: "session-1",
    caller: {
      kind: "app",
      id: "caller-app",
      owner_uid: 1000,
      app_id: "caller",
    },
  };
}

function callFrame(
  id: string | number,
  name: string,
  args: Record<string, unknown> = {},
  meta: Record<string, unknown> = {},
): Frame {
  return {
    jsonrpc: "2.0",
    id,
    method: "tools/call",
    params: {
      name,
      arguments: args,
      _meta: {
        [CALL_CONTEXT_META_KEY]: context(String(id)),
        ...meta,
      },
    },
  };
}

class Harness {
  readonly input = new PassThrough();
  readonly output = new PassThrough();
  readonly done: Promise<void>;
  readonly lines: string[] = [];

  private buffered = "";
  private readonly frames: Frame[] = [];
  private readonly waiters: Array<{
    predicate: (frame: Frame) => boolean;
    resolve: (frame: Frame) => void;
    reject: (error: Error) => void;
    timer: NodeJS.Timeout;
  }> = [];

  constructor(app: App) {
    this.output.setEncoding("utf8");
    this.output.on("data", (chunk: string) => this.consume(chunk));
    this.done = app.serve(this.input, this.output);
  }

  send(frame: Frame): void {
    this.input.write(`${JSON.stringify(frame)}\n`);
  }

  sendRaw(bytes: string | Buffer): void {
    this.input.write(bytes);
  }

  waitFor(predicate: (frame: Frame) => boolean): Promise<Frame> {
    const existing = this.frames.find(predicate);
    if (existing !== undefined) return Promise.resolve(existing);
    return new Promise((resolve, reject) => {
      const timer = setTimeout(() => {
        reject(new Error(`timed out waiting for frame; received ${JSON.stringify(this.frames)}`));
      }, 2_000);
      this.waiters.push({ predicate, resolve, reject, timer });
    });
  }

  async close(): Promise<void> {
    this.input.end();
    await this.done;
  }

  private consume(chunk: string): void {
    this.buffered += chunk;
    while (true) {
      const newline = this.buffered.indexOf("\n");
      if (newline < 0) return;
      const line = this.buffered.slice(0, newline);
      this.buffered = this.buffered.slice(newline + 1);
      if (line === "") continue;
      this.lines.push(line);
      const frame = JSON.parse(line) as Frame;
      this.frames.push(frame);
      for (let index = this.waiters.length - 1; index >= 0; index -= 1) {
        const waiter = this.waiters[index];
        if (!waiter.predicate(frame)) continue;
        this.waiters.splice(index, 1);
        clearTimeout(waiter.timer);
        waiter.resolve(frame);
      }
    }
  }
}

test("manifest contract is closed and requires tools", () => {
  const tool = {
    name: "test_app.status",
    summary: { en: "Show status." },
  };
  const base = {
    schema_version: 2,
    id: "test_app",
    version: "1.2.3",
    name: { en: "Test App" },
    mcp: { tools: [tool] },
  };
  for (const manifest of [
    { ...base, unknown: true },
    { ...base, mcp: { ...base.mcp, unknown: true } },
    { ...base, mcp: { tools: [{ ...tool, unknown: true }] } },
    {
      ...base,
      mcp: {
        tools: [{
          ...tool,
          args: [{ name: "value", kind: "text", binding: "sideways" }],
        }],
      },
    },
    { ...base, mcp: { tools: [] } },
  ]) {
    assert.throws(() => App.fromManifest(writeManifest(manifest)), ManifestError);
  }
});

test("mcp tool arg binding is CLI-only metadata excluded from input schema", async () => {
  const app = App.fromManifest(manifestPath([
    {
      name: "test_app.compose",
      summary: { en: "Compose" },
      args: [
        { name: "message", kind: "text", binding: "positional" },
        { name: "loud", kind: "bool", binding: "flag" },
      ],
    },
  ]));
  app.tool("test_app.compose", ({ message }) => message);
  const harness = new Harness(app);
  harness.send({ jsonrpc: "2.0", id: 1, method: "tools/list", params: {} });
  const reply = await harness.waitFor((frame) => frame.id === 1);
  const [entry] = (reply.result as Frame).tools as Frame[];
  const schema = entry.inputSchema as Frame;
  const properties = schema.properties as Frame;
  // `binding` is one-shot CLI metadata only; it must not surface in the
  // model-facing MCP inputSchema.
  assert.ok(!JSON.stringify(schema).includes("binding"));
  assert.ok(properties.message);
  assert.ok(properties.loud);
  await harness.close();
});

test("tools/list is derived only from the manifest", async () => {
  const app = App.fromManifest(manifestPath([
    {
      name: "test_app.echo",
      summary: { en: "Echo text" },
      args: [
        {
          name: "text",
          kind: "text",
          required: true,
          label: { en: "Text to echo" },
        },
        {
          name: "mode",
          kind: "name",
          choices: ["short", "long"],
          default: "short",
        },
      ],
    },
  ]));
  app.tool("test_app.echo", ({ text }) => text);
  const harness = new Harness(app);
  harness.send({
    jsonrpc: "2.0",
    id: "initialize",
    method: "initialize",
    params: {
      protocolVersion: "2025-06-18",
      capabilities: {},
      clientInfo: { name: "test-client", version: "1.0.0" },
    },
  });
  const initialized = await harness.waitFor((frame) => frame.id === "initialize");
  assert.deepEqual(initialized.result, {
    protocolVersion: "2025-06-18",
    capabilities: { tools: { listChanged: false } },
    serverInfo: { name: "test_app", version: "1.2.3" },
  });
  harness.send({ jsonrpc: "2.0", method: "notifications/initialized" });
  harness.send({ jsonrpc: "2.0", id: 1, method: "tools/list", params: {} });
  const reply = await harness.waitFor((frame) => frame.id === 1);
  assert.deepEqual(reply.result, {
    tools: [{
      name: "test_app.echo",
      description: "Echo text",
      inputSchema: {
        type: "object",
        properties: {
          text: {
            type: "string",
            description: "Text to echo",
          },
          mode: {
            type: "string",
            enum: ["short", "long"],
            default: "short",
          },
        },
        additionalProperties: false,
        required: ["text"],
      },
    }],
  });
  await harness.close();
});

test("binding calls a handler with immutable authenticated context", async () => {
  const app = App.fromManifest(manifestPath([
    {
      name: "test_app.identity",
      summary: { en: "Show caller" },
      args: [],
    },
  ]));
  app.tool("test_app.identity", (_args, passedContext) => {
    const active = currentContext();
    assert.equal(active, passedContext);
    assert.equal(active.authenticated, true);
    assert.equal(active.callId, "call-2");
    assert.equal(active.traceId, "trace-call-2");
    assert.equal(active.caller.id, "caller-app");
    assert.ok(Object.isFrozen(active));
    assert.ok(Object.isFrozen(active.caller));
    return "caller-app";
  });
  const harness = new Harness(app);
  harness.send(callFrame("call-2", "test_app.identity"));
  const reply = await harness.waitFor((frame) => frame.id === "call-2");
  assert.equal(
    ((reply.result as Frame).content as Frame[])[0].text,
    "caller-app",
  );
  await harness.close();
});

test("defaults are applied and bad arguments are MCP tool errors", async () => {
  const app = App.fromManifest(manifestPath([
    {
      name: "test_app.format",
      summary: { en: "Format text" },
      args: [
        { name: "text", kind: "text", required: true },
        { name: "mode", kind: "name", choices: ["short", "long"], default: "short" },
        {
          name: "detail",
          kind: "text",
          required_when: { kind: "arg-equals", arg: "mode", value: "long" },
        },
      ],
    },
  ]));
  app.tool("test_app.format", (args) => args);
  const harness = new Harness(app);

  harness.send(callFrame(3, "test_app.format", { text: "hello" }));
  const success = await harness.waitFor((frame) => frame.id === 3);
  assert.deepEqual((success.result as Frame).structuredContent, {
    text: "hello",
    mode: "short",
  });

  harness.send(callFrame(4, "test_app.format", { text: "hello", extra: true }));
  const unknown = await harness.waitFor((frame) => frame.id === 4);
  assert.equal((unknown.result as Frame).isError, true);
  assert.match(
    String(((unknown.result as Frame).content as Frame[])[0].text),
    /unknown argument `extra`/,
  );

  harness.send(callFrame(5, "test_app.format", { text: "hello", mode: "long" }));
  const conditional = await harness.waitFor((frame) => frame.id === 5);
  assert.equal((conditional.result as Frame).isError, true);
  assert.match(
    String(((conditional.result as Frame).content as Frame[])[0].text),
    /missing required argument `detail`/,
  );
  await harness.close();
});

test("object results include structured content and rendered text", async () => {
  const app = App.fromManifest(manifestPath([
    {
      name: "test_app.object",
      summary: { en: "Return an object" },
      args: [],
    },
  ]));
  app.tool("test_app.object", () => ({ ok: true, count: 2 }));
  const harness = new Harness(app);
  harness.send(callFrame(6, "test_app.object"));
  const reply = await harness.waitFor((frame) => frame.id === 6);
  assert.deepEqual((reply.result as Frame).structuredContent, { ok: true, count: 2 });
  assert.equal(
    ((reply.result as Frame).content as Frame[])[0].text,
    "{\"ok\":true,\"count\":2}",
  );
  await harness.close();
});

test("progress is emitted only with a progress token", async () => {
  const app = App.fromManifest(manifestPath([
    {
      name: "test_app.progress",
      summary: { en: "Report progress" },
      args: [],
    },
  ]));
  app.tool("test_app.progress", async (_args, callContext) => {
    await callContext.reportProgress(1, { total: 2, message: "halfway" });
    return "done";
  });
  const harness = new Harness(app);
  harness.send(callFrame(7, "test_app.progress", {}, { progressToken: "token-1" }));
  const notification = await harness.waitFor(
    (frame) => frame.method === "notifications/progress",
  );
  assert.deepEqual(notification.params, {
    progressToken: "token-1",
    progress: 1,
    total: 2,
    message: "halfway",
  });
  await harness.waitFor((frame) => frame.id === 7);

  harness.send(callFrame(8, "test_app.progress"));
  await harness.waitFor((frame) => frame.id === 8);
  await harness.close();
});

test("cancellation is observable while an async handler is running", async () => {
  const app = App.fromManifest(manifestPath([
    {
      name: "test_app.wait",
      summary: { en: "Wait for cancellation" },
      args: [],
    },
  ]));
  let started: (() => void) | undefined;
  const didStart = new Promise<void>((resolve) => {
    started = resolve;
  });
  let observed = false;
  app.tool("test_app.wait", async (_args, callContext) => {
    started?.();
    await new Promise<void>((resolve) => {
      callContext.signal.addEventListener("abort", () => {
        observed = callContext.cancelled;
        resolve();
      }, { once: true });
    });
    callContext.throwIfCancelled();
  });
  const harness = new Harness(app);
  harness.send(callFrame("cancel-me", "test_app.wait"));
  await didStart;
  harness.send({
    jsonrpc: "2.0",
    method: "notifications/cancelled",
    params: { requestId: "cancel-me", reason: "test" },
  });
  harness.send({ jsonrpc: "2.0", id: "ping", method: "ping" });
  await harness.waitFor((frame) => frame.id === "ping");
  assert.equal(observed, true);
  await harness.close();
});

test("missing authenticated context is a JSON-RPC parameter error", async () => {
  const app = App.fromManifest(manifestPath([
    {
      name: "test_app.secure",
      summary: { en: "Require identity" },
      args: [],
    },
  ]));
  app.tool("test_app.secure", () => "no");
  const harness = new Harness(app);
  harness.send({
    jsonrpc: "2.0",
    id: 9,
    method: "tools/call",
    params: { name: "test_app.secure", arguments: {}, _meta: {} },
  });
  const reply = await harness.waitFor((frame) => frame.id === 9);
  assert.equal((reply.error as Frame).code, ERR_INVALID_PARAMS);
  await harness.close();
});

test("expired authenticated deadlines stop handlers before execution", async () => {
  const app = App.fromManifest(manifestPath([
    {
      name: "test_app.deadline",
      summary: { en: "Check a deadline" },
      args: [],
    },
  ]));
  let ran = false;
  app.tool("test_app.deadline", () => {
    ran = true;
    return "late";
  });
  const harness = new Harness(app);
  const frame = callFrame(11, "test_app.deadline");
  const params = frame.params as Frame;
  const meta = params._meta as Frame;
  (meta[CALL_CONTEXT_META_KEY] as Frame).deadline_unix_ms = 1;
  harness.send(frame);
  const reply = await harness.waitFor((candidate) => candidate.id === 11);
  assert.equal((reply.result as Frame).isError, true);
  assert.match(
    String(((reply.result as Frame).content as Frame[])[0].text),
    /deadline/,
  );
  assert.equal(ran, false);
  await harness.close();
});

test("malformed and oversized frames are rejected without poisoning the stream", async () => {
  const app = protocolOnlyApp();
  const harness = new Harness(app);
  harness.sendRaw("{bad json\n");
  const malformed = await harness.waitFor(
    (frame) => (frame.error as Frame | undefined)?.code === ERR_PARSE,
  );
  assert.equal(malformed.id, null);

  harness.sendRaw(Buffer.from([0x22, 0xff, 0x22, 0x0a]));
  await harness.waitFor(
    (frame) =>
      (frame.error as Frame | undefined)?.code === ERR_PARSE
      && String((frame.error as Frame).message).includes("encoded data"),
  );

  harness.sendRaw(Buffer.alloc(MAX_LINE_BYTES + 1, 0x61));
  harness.sendRaw("\n");
  await harness.waitFor(
    (frame) =>
      (frame.error as Frame | undefined)?.code === ERR_PARSE
      && String((frame.error as Frame).message).includes("exceeds"),
  );
  harness.send({ jsonrpc: "2.0", id: 10, method: "ping" });
  const ping = await harness.waitFor((frame) => frame.id === 10);
  assert.deepEqual(ping.result, {});
  await harness.close();
});

test("tool response output failures reject the App server", async () => {
  const app = App.fromManifest(manifestPath([
    {
      name: "test_app.output",
      summary: { en: "Write a result" },
      args: [],
    },
  ]));
  app.tool("test_app.output", () => "done");
  const input = new PassThrough();
  const output = new Writable({
    write(_chunk, _encoding, callback) {
      callback(new Error("output closed"));
    },
  });
  const serving = app.serve(input, output);
  input.write(`${JSON.stringify(callFrame(12, "test_app.output"))}\n`);
  await assert.rejects(serving, /output closed/);
});

test("request IDs retain their exact wire representation", async () => {
  const app = protocolOnlyApp();
  const harness = new Harness(app);
  harness.sendRaw(
    "{\"jsonrpc\":\"2.0\",\"id\":9007199254740993,\"method\":\"ping\"}\n",
  );
  await harness.waitFor((frame) => frame.result !== undefined);
  assert.ok(
    harness.lines.some((line) => line.includes("\"id\":9007199254740993")),
  );
  await harness.close();
});

test("all manifest tools must be bound before serving", async () => {
  const app = App.fromManifest(manifestPath([
    {
      name: "test_app.unbound",
      summary: { en: "Must be bound" },
      args: [],
    },
  ]));
  await assert.rejects(
    app.serve(new PassThrough(), new PassThrough()),
    ManifestError,
  );
});
