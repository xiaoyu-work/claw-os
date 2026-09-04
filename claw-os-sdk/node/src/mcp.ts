import { AsyncLocalStorage } from "node:async_hooks";
import {
  closeSync,
  fstatSync,
  openSync,
  readSync,
} from "node:fs";
import { Readable, Writable } from "node:stream";
import { TextDecoder } from "node:util";

import {
  McpCallContext,
  McpPrincipal,
  WireDecimal,
  WireJsonValue,
  decodeWireJson,
  stringifyWireJson,
  validateMcpCallContext,
  wireIntegerToJs,
} from "./generated";

export const PROTOCOL_VERSION = "2025-06-18";
export const JSONRPC_VERSION = "2.0";
export const CALL_CONTEXT_META_KEY = "claw-os.dev/call-context";
export const MAX_LINE_BYTES = 16 * 1024 * 1024;
export const MAX_MANIFEST_BYTES = 1024 * 1024;
export const ERR_PARSE = -32700;
export const ERR_INVALID_REQUEST = -32600;
export const ERR_METHOD_NOT_FOUND = -32601;
export const ERR_INVALID_PARAMS = -32602;
export const ERR_INTERNAL = -32603;
export const ERR_SERVER_BUSY = -32000;

const MAX_PENDING_CALLS = 64;
const EOF_CANCELLATION_GRACE_MS = 50;
const TOOL_NAME_PATTERN = /^[a-z][a-z0-9._-]*$/;
const APP_ID_PATTERN = /^[a-z][a-z0-9_-]*$/;
const ARG_KINDS = new Set([
  "path",
  "host",
  "name",
  "text",
  "number",
  "integer",
  "bool",
]);
const UTF8_DECODER = new TextDecoder("utf-8", { fatal: true });

type JsonObject = Record<string, unknown>;
type ArgKind = "path" | "host" | "name" | "text" | "number" | "integer" | "bool";
type ConditionKind = "arg-present" | "arg-equals" | "arg-not-equals";

interface ManifestCondition {
  readonly kind: ConditionKind;
  readonly arg: string;
  readonly value?: unknown;
}

interface ManifestArgument {
  readonly name: string;
  readonly kind: ArgKind;
  readonly required: boolean;
  readonly repeatable: boolean;
  readonly choices: readonly unknown[];
  readonly hasDefault: boolean;
  readonly defaultValue?: unknown;
  readonly requiredWhen?: ManifestCondition;
  readonly label?: string;
}

interface ToolDefinition {
  readonly name: string;
  readonly summary: string;
  readonly inputSchema: Readonly<JsonObject>;
  readonly args: readonly ManifestArgument[];
  handler?: ToolHandler;
}

interface CallState {
  readonly key: string;
  readonly id: WireJsonValue;
  readonly params: JsonObject;
  readonly controller: AbortController;
  cancelled: boolean;
}

interface Frame {
  readonly bytes?: Buffer;
  readonly overflowed: boolean;
}

export type ToolHandler = (
  args: Readonly<Record<string, unknown>>,
  context: CallContext,
) => unknown | Promise<unknown>;

export class ManifestError extends Error {
  constructor(message: string) {
    super(message);
    this.name = "ManifestError";
  }
}

export class CallCancelled extends Error {
  constructor(message: string) {
    super(message);
    this.name = "CallCancelled";
  }
}

export class CallContext {
  readonly callId: string;
  readonly traceId: string;
  readonly parentCallId?: string;
  readonly depth: number;
  readonly deadlineUnixMs?: number;
  readonly sessionId?: string;
  readonly taskId?: string;
  readonly caller: Readonly<McpPrincipal>;
  readonly signal: AbortSignal;

  private readonly progressToken?: WireJsonValue;
  private readonly emitNotification: (
    method: string,
    params: JsonObject,
  ) => Promise<void>;

  constructor(
    authenticated: McpCallContext,
    signal: AbortSignal,
    progressToken: WireJsonValue | undefined,
    emitNotification: (method: string, params: JsonObject) => Promise<void>,
  ) {
    this.callId = authenticated.call_id;
    this.traceId = authenticated.trace_id;
    this.parentCallId = authenticated.parent_call_id;
    this.depth = authenticated.depth;
    this.deadlineUnixMs = authenticated.deadline_unix_ms;
    this.sessionId = authenticated.session_id;
    this.taskId = authenticated.task_id;
    this.caller = Object.freeze({ ...authenticated.caller });
    this.signal = signal;
    this.progressToken = progressToken;
    this.emitNotification = emitNotification;
    Object.freeze(this);
  }

  get authenticated(): true {
    return true;
  }

  get progressRequested(): boolean {
    return this.progressToken !== undefined;
  }

  get cancelled(): boolean {
    return this.signal.aborted
      || (this.deadlineUnixMs !== undefined && Date.now() >= this.deadlineUnixMs);
  }

  throwIfCancelled(): void {
    if (!this.cancelled) return;
    const reason = this.signal.reason;
    if (reason instanceof CallCancelled) throw reason;
    throw new CallCancelled(`call \`${this.callId}\` was cancelled`);
  }

  async reportProgress(
    progress: number,
    options: { total?: number; message?: string } = {},
  ): Promise<void> {
    this.throwIfCancelled();
    if (this.progressToken === undefined) return;
    validateProgressNumber(progress, "progress");
    const params: JsonObject = {
      progressToken: this.progressToken,
      progress,
    };
    if (options.total !== undefined) {
      validateProgressNumber(options.total, "total");
      params.total = options.total;
    }
    if (options.message !== undefined) {
      if (typeof options.message !== "string") {
        throw new TypeError("progress message must be a string");
      }
      params.message = options.message;
    }
    await this.emitNotification("notifications/progress", params);
  }
}

export class ToolResult {
  readonly content: readonly Readonly<JsonObject>[];
  readonly isError: boolean;
  readonly structuredContent?: Readonly<JsonObject>;

  private constructor(
    content: readonly JsonObject[],
    isError: boolean,
    structuredContent?: JsonObject,
  ) {
    this.content = Object.freeze(content.map((item) => Object.freeze({ ...item })));
    this.isError = isError;
    this.structuredContent = structuredContent === undefined
      ? undefined
      : deepFreeze(structuredContent);
    Object.freeze(this);
  }

  static text(text: string): ToolResult {
    return new ToolResult([{ type: "text", text }], false);
  }

  static error(message: string): ToolResult {
    return new ToolResult([{ type: "text", text: message }], true);
  }

  static structured(value: JsonObject, text?: string): ToolResult {
    const frozen = deepFreeze(deepClone(value));
    return new ToolResult(
      [{ type: "text", text: text ?? encodeWire(value) }],
      false,
      frozen,
    );
  }
}

const contextStorage = new AsyncLocalStorage<CallContext>();

export function currentContext(): CallContext {
  const context = contextStorage.getStore();
  if (context === undefined) {
    throw new Error("no MCP tool call is active");
  }
  return context;
}

class RpcError extends Error {
  constructor(
    readonly code: number,
    message: string,
    readonly data?: unknown,
  ) {
    super(message);
    this.name = "RpcError";
  }
}

class ToolArgumentError extends Error {}

class FrameWriter {
  private tail: Promise<void> = Promise.resolve();
  private failure: Error | undefined;
  private readonly handleOutputError = (error: Error): void => {
    this.fail(error);
  };

  constructor(
    private readonly output: Writable,
    private readonly onFailure: (error: Error) => void,
  ) {
    this.output.on("error", this.handleOutputError);
  }

  write(frame: JsonObject): Promise<void> {
    if (this.failure !== undefined) return Promise.reject(this.failure);
    let encoded: string;
    try {
      encoded = `${encodeWire(frame)}\n`;
    } catch (error) {
      const failure = asError(error);
      this.fail(failure);
      return Promise.reject(failure);
    }
    const operation = this.tail.then(() => this.writeChunk(encoded));
    this.tail = operation.catch((error: unknown) => {
      this.fail(asError(error));
    });
    return operation;
  }

  async flush(): Promise<void> {
    await this.tail;
    if (this.failure !== undefined) throw this.failure;
  }

  recordFailure(error: unknown): void {
    this.fail(asError(error));
  }

  dispose(): void {
    this.output.off("error", this.handleOutputError);
  }

  private writeChunk(chunk: string): Promise<void> {
    return new Promise((resolve, reject) => {
      this.output.write(chunk, "utf8", (error?: Error | null) => {
        if (error) reject(error);
        else resolve();
      });
    });
  }

  private fail(error: Error): void {
    if (this.failure !== undefined) return;
    this.failure = error;
    this.onFailure(error);
  }
}

export class App {
  readonly id: string;
  readonly version: string;

  private readonly tools: Map<string, ToolDefinition>;
  private initialized = false;
  private writer: FrameWriter | undefined;
  private readonly calls = new Map<string, CallState>();
  private readonly pending: CallState[] = [];
  private active: CallState | undefined;
  private readonly running = new Set<Promise<void>>();

  private constructor(id: string, version: string, tools: ToolDefinition[]) {
    this.id = id;
    this.version = version;
    this.tools = new Map(tools.map((tool) => [tool.name, tool]));
  }

  static fromManifest(path?: string): App {
    const manifestPath = path
      ?? (process.env.COS_APP_MANIFEST || undefined)
      ?? "app.json";
    const { id, version, tools } = loadManifest(manifestPath);
    return new App(id, version, tools);
  }

  tool(name: string, handler: ToolHandler): this {
    const tool = this.tools.get(name);
    if (tool === undefined) {
      throw new Error(`tool \`${name}\` is not declared in app.json.mcp.tools`);
    }
    if (tool.handler !== undefined) {
      throw new Error(`tool \`${name}\` is already bound`);
    }
    if (typeof handler !== "function") {
      throw new TypeError("tool handler must be a function");
    }
    tool.handler = handler;
    return this;
  }

  async serve(
    input: Readable = process.stdin,
    output: Writable = process.stdout,
  ): Promise<void> {
    this.validateBindings();
    if (this.writer !== undefined) {
      throw new Error("this MCP App is already serving");
    }
    const writer = new FrameWriter(output, (error) => {
      if (!input.destroyed) input.destroy(error);
    });
    this.writer = writer;
    try {
      for await (const frame of readBoundedFrames(input)) {
        if (frame.overflowed) {
          await this.sendError(
            null,
            ERR_PARSE,
            `frame exceeds ${MAX_LINE_BYTES} bytes; rejected`,
          );
          continue;
        }
        if (frame.bytes === undefined || frame.bytes.length === 0) continue;
        await this.handleFrame(frame.bytes);
      }
      await this.finishAfterEof();
      await writer.flush();
    } finally {
      writer.dispose();
      this.writer = undefined;
      this.cancelAll("MCP input closed");
    }
  }

  private validateBindings(): void {
    const missing = [...this.tools.values()]
      .filter((tool) => tool.handler === undefined)
      .map((tool) => tool.name);
    if (missing.length > 0) {
      throw new ManifestError(
        `missing handlers for manifest tools: ${missing.join(", ")}`,
      );
    }
  }

  private async handleFrame(bytes: Buffer): Promise<void> {
    let message: WireJsonValue;
    try {
      message = decodeWireJson(UTF8_DECODER.decode(bytes));
    } catch (error) {
      await this.sendError(null, ERR_PARSE, `parse error: ${asError(error).message}`);
      return;
    }
    if (!isObject(message)) {
      await this.sendError(null, ERR_INVALID_REQUEST, "request not an object");
      return;
    }

    const hasId = Object.prototype.hasOwnProperty.call(message, "id");
    const rawId = message.id;
    if (hasId && !isRequestId(rawId)) {
      await this.sendError(
        null,
        ERR_INVALID_REQUEST,
        "request id must be a string, number, or null",
      );
      return;
    }
    const id = (hasId ? rawId : null) as WireJsonValue;
    if (message.jsonrpc !== JSONRPC_VERSION) {
      await this.sendError(id, ERR_INVALID_REQUEST, "missing jsonrpc 2.0 envelope");
      return;
    }
    if (typeof message.method !== "string") {
      await this.sendError(id, ERR_INVALID_REQUEST, "request method must be a string");
      return;
    }
    if (
      !hasId
      && Object.prototype.hasOwnProperty.call(message, "params")
      && !isObject(message.params)
      && !Array.isArray(message.params)
    ) {
      await this.sendError(
        null,
        ERR_INVALID_REQUEST,
        "request params must be an object or array",
      );
      return;
    }

    if (!hasId) {
      this.handleNotification(message.method, message.params);
      return;
    }
    if (message.method === "tools/call") {
      await this.queueToolCall(id, message.params);
      return;
    }
    try {
      const result = this.handleRequest(
        message.method,
        message.params,
        Object.prototype.hasOwnProperty.call(message, "params"),
      );
      await this.sendResult(id, result);
    } catch (error) {
      await this.sendCaughtError(id, error);
    }
  }

  private handleNotification(method: string, params: unknown): void {
    if (method === "notifications/initialized") {
      this.initialized = true;
      return;
    }
    if (method === "notifications/progress") return;
    if (method !== "notifications/cancelled") return;
    if (!isObject(params) || !Object.prototype.hasOwnProperty.call(params, "requestId")) {
      return;
    }
    const requestId = params.requestId;
    if (!isRequestId(requestId)) return;
    const state = this.calls.get(requestKey(requestId));
    if (state !== undefined) {
      state.cancelled = true;
      state.controller.abort(new CallCancelled(`call \`${state.key}\` was cancelled`));
    }
  }

  private handleRequest(
    method: string,
    params: unknown,
    paramsPresent: boolean,
  ): unknown {
    if (method === "initialize") {
      if (!isObject(params)) {
        throw new RpcError(ERR_INVALID_PARAMS, "initialize params must be an object");
      }
      return this.initialize(params);
    }
    if (method === "ping") {
      if (paramsPresent && !isObject(params)) {
        throw new RpcError(ERR_INVALID_PARAMS, "ping params must be an object");
      }
      return {};
    }
    if (method === "tools/list") {
      if (paramsPresent) {
        if (!isObject(params)) {
          throw new RpcError(ERR_INVALID_PARAMS, "tools/list params must be an object");
        }
        if (params.cursor !== undefined && typeof params.cursor !== "string") {
          throw new RpcError(
            ERR_INVALID_PARAMS,
            "tools/list cursor must be a string",
          );
        }
      }
      return {
        tools: [...this.tools.values()].map((tool) => ({
          name: tool.name,
          description: tool.summary,
          inputSchema: tool.inputSchema,
        })),
      };
    }
    throw new RpcError(ERR_METHOD_NOT_FOUND, `unknown method \`${method}\``);
  }

  private initialize(params: JsonObject): JsonObject {
    if (typeof params.protocolVersion !== "string") {
      throw new RpcError(ERR_INVALID_PARAMS, "missing `protocolVersion`");
    }
    if (!isObject(params.capabilities)) {
      throw new RpcError(ERR_INVALID_PARAMS, "missing `capabilities`");
    }
    for (const name of ["experimental", "sampling", "elicitation"]) {
      if (
        params.capabilities[name] !== undefined
        && !isObject(params.capabilities[name])
      ) {
        throw new RpcError(
          ERR_INVALID_PARAMS,
          `\`capabilities.${name}\` must be an object`,
        );
      }
    }
    if (params.capabilities.roots !== undefined) {
      if (!isObject(params.capabilities.roots)) {
        throw new RpcError(
          ERR_INVALID_PARAMS,
          "`capabilities.roots` must be an object",
        );
      }
      if (
        params.capabilities.roots.listChanged !== undefined
        && typeof params.capabilities.roots.listChanged !== "boolean"
      ) {
        throw new RpcError(
          ERR_INVALID_PARAMS,
          "`capabilities.roots.listChanged` must be a boolean",
        );
      }
    }
    if (
      !isObject(params.clientInfo)
      || typeof params.clientInfo.name !== "string"
      || typeof params.clientInfo.version !== "string"
    ) {
      throw new RpcError(ERR_INVALID_PARAMS, "missing or invalid `clientInfo`");
    }
    return {
      protocolVersion: PROTOCOL_VERSION,
      capabilities: { tools: { listChanged: false } },
      serverInfo: { name: this.id, version: this.version },
    };
  }

  private async queueToolCall(id: WireJsonValue, params: unknown): Promise<void> {
    if (!isObject(params)) {
      await this.sendError(id, ERR_INVALID_PARAMS, "tools/call params must be an object");
      return;
    }
    if (this.calls.size >= MAX_PENDING_CALLS) {
      await this.sendError(id, ERR_SERVER_BUSY, "too many pending MCP tool calls");
      return;
    }
    const key = requestKey(id);
    if (this.calls.has(key)) {
      await this.sendError(id, ERR_INVALID_REQUEST, "duplicate active request id");
      return;
    }
    const state: CallState = {
      key,
      id,
      params,
      controller: new AbortController(),
      cancelled: false,
    };
    this.calls.set(key, state);
    this.pending.push(state);
    this.startNextCall();
  }

  private startNextCall(): void {
    if (this.active !== undefined) return;
    const state = this.pending.shift();
    if (state === undefined) return;
    this.active = state;
    const operation = this.executeToolCall(state)
      .catch(async (error: unknown) => {
        if (!state.cancelled) await this.sendCaughtError(state.id, error);
      })
      .finally(() => {
        this.calls.delete(state.key);
        if (this.active === state) this.active = undefined;
        this.running.delete(operation);
        this.startNextCall();
      });
    this.running.add(operation);
    void operation.catch((error: unknown) => {
      this.writer?.recordFailure(error);
    });
  }

  private async executeToolCall(state: CallState): Promise<void> {
    const params = state.params;
    const name = params.name;
    if (typeof name !== "string") {
      throw new RpcError(ERR_INVALID_PARAMS, "missing `name`");
    }
    const tool = this.tools.get(name);
    if (tool === undefined) {
      throw new RpcError(ERR_INVALID_PARAMS, `unknown tool \`${name}\``);
    }
    const supplied = params.arguments === undefined ? {} : params.arguments;
    if (!isObject(supplied)) {
      throw new RpcError(ERR_INVALID_PARAMS, "`arguments` must be an object");
    }
    const context = this.makeContext(params, state);
    let deadlineTimer: NodeJS.Timeout | undefined;
    if (context.deadlineUnixMs !== undefined) {
      const delay = context.deadlineUnixMs - Date.now();
      if (delay <= 0) {
        state.controller.abort(
          new CallCancelled(`call \`${context.callId}\` exceeded its deadline`),
        );
      } else {
        deadlineTimer = setTimeout(() => {
          state.controller.abort(
            new CallCancelled(`call \`${context.callId}\` exceeded its deadline`),
          );
        }, Math.min(delay, 2_147_483_647));
        deadlineTimer.unref();
      }
    }

    try {
      let args: Readonly<Record<string, unknown>>;
      try {
        args = Object.freeze(resolveArguments(tool, supplied));
      } catch (error) {
        if (!(error instanceof ToolArgumentError)) throw error;
        if (!state.cancelled) {
          await this.sendResult(
            state.id,
            toolError(`bad arguments for \`${name}\`: ${error.message}`),
          );
        }
        return;
      }
      const handler = tool.handler;
      if (handler === undefined) {
        throw new RpcError(ERR_INTERNAL, `tool \`${name}\` has no handler`);
      }
      let value: unknown;
      try {
        context.throwIfCancelled();
        value = await contextStorage.run(context, () => handler(args, context));
        context.throwIfCancelled();
      } catch (error) {
        if (state.cancelled) return;
        const message = error instanceof CallCancelled
          ? error.message
          : `${asError(error).name}: ${asError(error).message}`;
        await this.sendResult(state.id, toolError(message));
        return;
      }
      if (!state.cancelled) {
        let result: JsonObject;
        try {
          result = coerceToolResult(value);
        } catch (error) {
          result = toolError(`invalid tool result: ${asError(error).message}`);
        }
        await this.sendResult(state.id, result);
      }
    } finally {
      if (deadlineTimer !== undefined) clearTimeout(deadlineTimer);
    }
  }

  private makeContext(params: JsonObject, state: CallState): CallContext {
    const meta = params._meta;
    if (!isObject(meta)) {
      throw new RpcError(ERR_INVALID_PARAMS, "`_meta` must be an object");
    }
    const progressToken = meta.progressToken;
    if (progressToken !== undefined && !isProgressToken(progressToken)) {
      throw new RpcError(
        ERR_INVALID_PARAMS,
        "`_meta.progressToken` must be a string or integer",
      );
    }
    const rawContext = meta[CALL_CONTEXT_META_KEY];
    if (rawContext === undefined) {
      throw new RpcError(
        ERR_INVALID_PARAMS,
        `missing authenticated \`${CALL_CONTEXT_META_KEY}\``,
      );
    }
    try {
      validateMcpCallContext(rawContext);
    } catch (error) {
      throw new RpcError(
        ERR_INVALID_PARAMS,
        `invalid authenticated call context: ${asError(error).message}`,
      );
    }
    const snapshot = deepFreeze(deepClone(rawContext)) as McpCallContext;
    return new CallContext(
      snapshot,
      state.controller.signal,
      progressToken as WireJsonValue | undefined,
      (method, notificationParams) => this.sendNotification(method, notificationParams),
    );
  }

  private async sendCaughtError(id: WireJsonValue, error: unknown): Promise<void> {
    if (error instanceof RpcError) {
      await this.sendError(id, error.code, error.message, error.data);
      return;
    }
    await this.sendError(
      id,
      ERR_INTERNAL,
      `internal error: ${asError(error).message}`,
    );
  }

  private sendResult(id: WireJsonValue, result: unknown): Promise<void> {
    return this.requireWriter().write({
      jsonrpc: JSONRPC_VERSION,
      id,
      result,
    });
  }

  private sendError(
    id: WireJsonValue,
    code: number,
    message: string,
    data?: unknown,
  ): Promise<void> {
    const error: JsonObject = { code, message };
    if (data !== undefined) error.data = data;
    return this.requireWriter().write({
      jsonrpc: JSONRPC_VERSION,
      id,
      error,
    });
  }

  private sendNotification(method: string, params: JsonObject): Promise<void> {
    return this.requireWriter().write({
      jsonrpc: JSONRPC_VERSION,
      method,
      params,
    });
  }

  private requireWriter(): FrameWriter {
    if (this.writer === undefined) throw new Error("MCP App is not serving");
    return this.writer;
  }

  private async finishAfterEof(): Promise<void> {
    if (this.calls.size === 0) return;
    await Promise.race([
      Promise.all([...this.running]),
      new Promise<void>((resolve) => setTimeout(resolve, EOF_CANCELLATION_GRACE_MS)),
    ]);
    if (this.calls.size > 0) {
      this.cancelAll("MCP input closed");
      await Promise.all([...this.running]);
    }
  }

  private cancelAll(reason: string): void {
    for (const state of this.calls.values()) {
      state.cancelled = true;
      state.controller.abort(new CallCancelled(reason));
    }
    this.pending.length = 0;
    for (const [key, state] of this.calls) {
      if (state !== this.active) this.calls.delete(key);
    }
  }
}

function rejectUnknownFields(
  value: JsonObject,
  context: string,
  allowed: readonly string[],
): void {
  const unknown = Object.keys(value)
    .filter((field) => !allowed.includes(field))
    .sort();
  if (unknown.length > 0) {
    throw new ManifestError(
      `${context} contains unknown field \`${unknown[0]}\``,
    );
  }
}

function loadManifest(path: string): {
  id: string;
  version: string;
  tools: ToolDefinition[];
} {
  const raw = readManifest(path);
  let manifest: WireJsonValue;
  try {
    manifest = decodeWireJson(raw.toString("utf8"));
  } catch (error) {
    throw new ManifestError(`invalid App manifest \`${path}\`: ${asError(error).message}`);
  }
  if (!isObject(manifest)) throw new ManifestError("App manifest must be a JSON object");
  rejectUnknownFields(manifest, "App manifest", [
    "id",
    "version",
    "schema_version",
    "name",
    "summary",
    "icon",
    "runtime",
    "entry",
    "operations",
    "ai",
    "mcp",
    "desktop",
    "dependencies",
  ]);
  if (manifest.schema_version !== 2) {
    throw new ManifestError("MCP Apps require `schema_version: 2`");
  }
  if (
    typeof manifest.id !== "string"
    || !APP_ID_PATTERN.test(manifest.id)
  ) {
    throw new ManifestError("App manifest has no valid `id`");
  }
  if (
    typeof manifest.version !== "string"
    || manifest.version.trim() === ""
    || hasUnpairedSurrogate(manifest.version)
  ) {
    throw new ManifestError("App manifest has no valid `version`");
  }
  if (!isObject(manifest.mcp)) {
    throw new ManifestError("App manifest has no `mcp` service");
  }
  localizedEnglish(manifest.name, "name");
  rejectUnknownFields(manifest.mcp, "`mcp`", [
    "entry",
    "transport",
    "lifecycle",
    "access",
    "tools",
  ]);
  if (
    manifest.mcp.transport !== undefined
    && manifest.mcp.transport !== "stdio"
  ) {
    throw new ManifestError("`mcp.transport` must be `stdio`");
  }
  if (
    manifest.mcp.lifecycle !== undefined
    && !["lazy", "always-on", "while-app-running"].includes(
      String(manifest.mcp.lifecycle),
    )
  ) {
    throw new ManifestError("`mcp.lifecycle` is invalid");
  }
  if (manifest.mcp.access !== undefined) {
    if (!isObject(manifest.mcp.access)) {
      throw new ManifestError("`mcp.access` must be an object");
    }
    rejectUnknownFields(manifest.mcp.access, "`mcp.access`", [
      "system_agent",
      "apps",
      "external_agents",
    ]);
  }
  const rawTools = manifest.mcp.tools;
  if (!Array.isArray(rawTools)) {
    throw new ManifestError("`mcp.tools` must be an array");
  }
  if (rawTools.length === 0) {
    throw new ManifestError("`mcp.tools` must contain at least one tool");
  }
  const names = new Set<string>();
  const tools = rawTools.map((rawTool, index) => {
    if (!isObject(rawTool)) {
      throw new ManifestError(`\`mcp.tools[${index}]\` must be an object`);
    }
    rejectUnknownFields(rawTool, `\`mcp.tools[${index}]\``, [
      "name",
      "summary",
      "args",
      "needs",
    ]);
    if (typeof rawTool.name !== "string" || !TOOL_NAME_PATTERN.test(rawTool.name)) {
      throw new ManifestError(`\`mcp.tools[${index}].name\` is invalid`);
    }
    if (names.has(rawTool.name)) {
      throw new ManifestError(`tool \`${rawTool.name}\` is declared twice`);
    }
    names.add(rawTool.name);
    const summary = localizedEnglish(
      rawTool.summary,
      `mcp.tools[${index}].summary`,
    );
    const rawArgs = rawTool.args ?? [];
    if (!Array.isArray(rawArgs)) {
      throw new ManifestError(`tool \`${rawTool.name}\` args must be an array`);
    }
    const args = parseArguments(rawTool.name, rawArgs);
    const inputSchema = deepFreeze(buildInputSchema(args));
    return {
      name: rawTool.name,
      summary,
      args: Object.freeze(args),
      inputSchema,
    };
  });
  return {
    id: manifest.id,
    version: manifest.version,
    tools,
  };
}

function readManifest(path: string): Buffer {
  let descriptor: number;
  try {
    descriptor = openSync(path, "r");
  } catch (error) {
    throw new ManifestError(`cannot read App manifest \`${path}\`: ${asError(error).message}`);
  }
  try {
    const size = fstatSync(descriptor).size;
    if (size > MAX_MANIFEST_BYTES) {
      throw new ManifestError(
        `App manifest \`${path}\` exceeds ${MAX_MANIFEST_BYTES} bytes`,
      );
    }
    const chunks: Buffer[] = [];
    let total = 0;
    while (total <= MAX_MANIFEST_BYTES) {
      const chunk = Buffer.allocUnsafe(
        Math.min(64 * 1024, MAX_MANIFEST_BYTES + 1 - total),
      );
      const count = readSync(descriptor, chunk, 0, chunk.length, null);
      if (count === 0) break;
      chunks.push(chunk.subarray(0, count));
      total += count;
    }
    if (total > MAX_MANIFEST_BYTES) {
      throw new ManifestError(
        `App manifest \`${path}\` exceeds ${MAX_MANIFEST_BYTES} bytes`,
      );
    }
    return Buffer.concat(chunks, total);
  } catch (error) {
    if (error instanceof ManifestError) throw error;
    throw new ManifestError(`cannot read App manifest \`${path}\`: ${asError(error).message}`);
  } finally {
    closeSync(descriptor);
  }
}

function parseArguments(
  toolName: string,
  rawArgs: WireJsonValue[],
): ManifestArgument[] {
  const args: ManifestArgument[] = [];
  const names = new Set<string>();
  for (let index = 0; index < rawArgs.length; index += 1) {
    const raw = rawArgs[index];
    if (!isObject(raw)) {
      throw new ManifestError(`tool \`${toolName}\` arg ${index} must be an object`);
    }
    if (typeof raw.name !== "string" || raw.name.trim() === "") {
      throw new ManifestError(`tool \`${toolName}\` arg ${index} has no valid name`);
    }
    if (names.has(raw.name)) {
      throw new ManifestError(`tool \`${toolName}\` arg \`${raw.name}\` is duplicated`);
    }
    if (typeof raw.kind !== "string" || !ARG_KINDS.has(raw.kind)) {
      throw new ManifestError(
        `tool \`${toolName}\` arg \`${raw.name}\` has invalid kind`,
      );
    }
    rejectUnknownFields(raw, `tool \`${toolName}\` arg \`${raw.name}\``, [
      "name",
      "kind",
      "required",
      "required_when",
      "repeatable",
      "choices",
      "default",
      "label",
    ]);
    if (raw.required !== undefined && typeof raw.required !== "boolean") {
      throw new ManifestError(`tool \`${toolName}\` arg \`${raw.name}\` required must be boolean`);
    }
    if (raw.repeatable !== undefined && typeof raw.repeatable !== "boolean") {
      throw new ManifestError(`tool \`${toolName}\` arg \`${raw.name}\` repeatable must be boolean`);
    }
    const kind = raw.kind as ArgKind;
    const required = raw.required === true;
    const repeatable = raw.repeatable === true;
    if (repeatable && kind === "bool") {
      throw new ManifestError(`tool \`${toolName}\` arg \`${raw.name}\` cannot repeat booleans`);
    }

    const choices = raw.choices ?? [];
    if (!Array.isArray(choices)) {
      throw new ManifestError(`tool \`${toolName}\` arg \`${raw.name}\` choices must be an array`);
    }
    const normalizedChoices = choices.map((choice) =>
      validateScalar(raw.name as string, kind, choice, "choice"));
    const choiceKeys = new Set(normalizedChoices.map(choiceKey));
    if (choiceKeys.size !== normalizedChoices.length) {
      throw new ManifestError(`tool \`${toolName}\` arg \`${raw.name}\` choices must be unique`);
    }

    const hasDefault = Object.prototype.hasOwnProperty.call(raw, "default");
    if (required && hasDefault) {
      throw new ManifestError(`tool \`${toolName}\` arg \`${raw.name}\` cannot be required and defaulted`);
    }
    const requiredWhen = raw.required_when === undefined
      ? undefined
      : parseCondition(toolName, raw.name, raw.required_when, names);
    if (requiredWhen !== undefined && (required || repeatable || hasDefault)) {
      throw new ManifestError(
        `tool \`${toolName}\` arg \`${raw.name}\` has an incompatible required_when declaration`,
      );
    }
    let defaultValue: unknown;
    if (hasDefault) {
      defaultValue = repeatable
        ? validateRepeatableDefault(raw.name, kind, raw.default)
        : validateScalar(raw.name, kind, raw.default, "default");
      if (
        normalizedChoices.length > 0
        && (repeatable
          ? !(defaultValue as unknown[]).every((value) =>
            normalizedChoices.some((choice) => valuesEqual(value, choice)))
          : !normalizedChoices.some((choice) => valuesEqual(defaultValue, choice)))
      ) {
        throw new ManifestError(
          `tool \`${toolName}\` arg \`${raw.name}\` default is not an allowed choice`,
        );
      }
    }
    const label = raw.label === undefined
      ? undefined
      : localizedEnglish(raw.label, `tool \`${toolName}\` arg \`${raw.name}\` label`);
    args.push(deepFreeze({
      name: raw.name,
      kind,
      required,
      repeatable,
      choices: Object.freeze(normalizedChoices),
      hasDefault,
      defaultValue: hasDefault ? deepFreeze(deepClone(defaultValue)) : undefined,
      requiredWhen,
      label,
    }));
    names.add(raw.name);
  }
  return args;
}

function parseCondition(
  toolName: string,
  argName: string,
  raw: unknown,
  earlierArgs: ReadonlySet<string>,
): ManifestCondition {
  if (!isObject(raw)) {
    throw new ManifestError(
      `tool \`${toolName}\` arg \`${argName}\` required_when must be an object`,
    );
  }
  const allowedFields = new Set(["kind", "arg", "value"]);
  const unknownField = Object.keys(raw).find((field) => !allowedFields.has(field));
  if (unknownField !== undefined) {
    throw new ManifestError(
      `tool \`${toolName}\` arg \`${argName}\` required_when has unknown field \`${unknownField}\``,
    );
  }
  if (
    raw.kind !== "arg-present"
    && raw.kind !== "arg-equals"
    && raw.kind !== "arg-not-equals"
  ) {
    throw new ManifestError(
      `tool \`${toolName}\` arg \`${argName}\` has invalid required_when kind`,
    );
  }
  if (typeof raw.arg !== "string" || !earlierArgs.has(raw.arg)) {
    throw new ManifestError(
      `tool \`${toolName}\` arg \`${argName}\` required_when must reference an earlier arg`,
    );
  }
  const hasValue = Object.prototype.hasOwnProperty.call(raw, "value");
  if (raw.kind === "arg-present" && hasValue) {
    throw new ManifestError(
      `tool \`${toolName}\` arg \`${argName}\` arg-present cannot declare value`,
    );
  }
  if (raw.kind !== "arg-present" && (!hasValue || raw.value === null)) {
    throw new ManifestError(
      `tool \`${toolName}\` arg \`${argName}\` condition requires a non-null value`,
    );
  }
  return deepFreeze({
    kind: raw.kind,
    arg: raw.arg,
    value: hasValue ? deepFreeze(deepClone(raw.value)) : undefined,
  });
}

function buildInputSchema(args: readonly ManifestArgument[]): JsonObject {
  const properties = Object.create(null) as JsonObject;
  const required: string[] = [];
  const allOf: JsonObject[] = [];
  for (const arg of args) {
    const scalar: JsonObject = { type: jsonType(arg.kind) };
    if (arg.choices.length > 0) scalar.enum = arg.choices;
    const property: JsonObject = arg.repeatable
      ? { type: "array", items: scalar }
      : scalar;
    if (arg.label !== undefined) property.description = arg.label;
    if (arg.hasDefault) property.default = arg.defaultValue;
    properties[arg.name] = property;
    if (arg.required) required.push(arg.name);
    if (arg.requiredWhen !== undefined) {
      allOf.push({
        if: conditionSchema(arg.requiredWhen),
        then: { required: [arg.name] },
        else: { not: { required: [arg.name] } },
      });
    }
  }
  const schema: JsonObject = {
    type: "object",
    properties,
    additionalProperties: false,
  };
  if (required.length > 0) schema.required = required;
  if (allOf.length > 0) schema.allOf = allOf;
  return schema;
}

function conditionSchema(condition: ManifestCondition): JsonObject {
  if (condition.kind === "arg-present") return { required: [condition.arg] };
  if (condition.kind === "arg-equals") {
    return {
      properties: { [condition.arg]: { const: condition.value } },
      required: [condition.arg],
    };
  }
  return {
    required: [condition.arg],
    not: {
      properties: { [condition.arg]: { const: condition.value } },
      required: [condition.arg],
    },
  };
}

function resolveArguments(
  tool: ToolDefinition,
  supplied: JsonObject,
): Record<string, unknown> {
  const declared = new Map(tool.args.map((arg) => [arg.name, arg]));
  const unknown = Object.keys(supplied).sort().find((name) => !declared.has(name));
  if (unknown !== undefined) {
    throw new ToolArgumentError(`unknown argument \`${unknown}\``);
  }
  const resolved = Object.fromEntries(Object.entries(supplied));
  for (const arg of tool.args) {
    const active = arg.requiredWhen === undefined
      || conditionMatches(arg.requiredWhen, resolved);
    if (!active) {
      if (Object.prototype.hasOwnProperty.call(resolved, arg.name)) {
        throw new ToolArgumentError(
          `\`${arg.name}\` is not accepted when its condition is false`,
        );
      }
      continue;
    }
    if (!Object.prototype.hasOwnProperty.call(resolved, arg.name)) {
      if (arg.hasDefault) {
        resolved[arg.name] = deepClone(arg.defaultValue);
      } else if (arg.required || arg.requiredWhen !== undefined) {
        throw new ToolArgumentError(`missing required argument \`${arg.name}\``);
      } else {
        continue;
      }
    }
    resolved[arg.name] = validateArgumentValue(arg, resolved[arg.name]);
  }
  return resolved;
}

function conditionMatches(
  condition: ManifestCondition,
  values: Record<string, unknown>,
): boolean {
  const present = Object.prototype.hasOwnProperty.call(values, condition.arg);
  if (condition.kind === "arg-present") return present;
  if (!present) return false;
  const equal = valuesEqual(values[condition.arg], condition.value);
  return condition.kind === "arg-equals" ? equal : !equal;
}

function validateArgumentValue(arg: ManifestArgument, value: unknown): unknown {
  if (arg.repeatable) {
    if (!Array.isArray(value)) {
      throw new ToolArgumentError(`\`${arg.name}\` must be an array`);
    }
    return value.map((item) => validateCallScalar(arg, item));
  }
  return validateCallScalar(arg, value);
}

function validateCallScalar(arg: ManifestArgument, value: unknown): unknown {
  let normalized: unknown;
  try {
    normalized = validateScalar(arg.name, arg.kind, value, "argument");
  } catch (error) {
    throw new ToolArgumentError(asError(error).message);
  }
  if (
    arg.choices.length > 0
    && !arg.choices.some((choice) => valuesEqual(normalized, choice))
  ) {
    throw new ToolArgumentError(`\`${arg.name}\` is not one of its allowed values`);
  }
  return normalized;
}

function validateScalar(
  name: string,
  kind: ArgKind,
  value: unknown,
  source: "argument" | "choice" | "default",
): unknown {
  const prefix = source === "argument" ? `\`${name}\`` : `${source} for \`${name}\``;
  if (kind === "path" || kind === "host" || kind === "name" || kind === "text") {
    if (typeof value !== "string") throw scalarValidationError(source, `${prefix} must be a string`);
    return value;
  }
  if (kind === "bool") {
    if (typeof value !== "boolean") throw scalarValidationError(source, `${prefix} must be a boolean`);
    return value;
  }
  if (kind === "integer") {
    try {
      return wireIntegerToJs(value);
    } catch {
      throw scalarValidationError(source, `${prefix} must be an integer`);
    }
  }
  if (!isWireNumber(value)) throw scalarValidationError(source, `${prefix} must be a number`);
  return value;
}

function scalarValidationError(
  source: "argument" | "choice" | "default",
  message: string,
): Error {
  return source === "argument" ? new Error(message) : new ManifestError(message);
}

function validateRepeatableDefault(
  name: string,
  kind: ArgKind,
  value: unknown,
): unknown[] {
  if (!Array.isArray(value)) {
    throw new ManifestError(`default for \`${name}\` must be an array`);
  }
  return value.map((item) => validateScalar(name, kind, item, "default"));
}

function localizedEnglish(value: unknown, field: string): string {
  if (
    !isObject(value)
    || typeof value.en !== "string"
    || value.en.trim() === ""
    || hasUnpairedSurrogate(value.en)
  ) {
    throw new ManifestError(`\`${field}\` requires non-empty English text`);
  }
  return value.en;
}

function jsonType(kind: ArgKind): string {
  if (kind === "number") return "number";
  if (kind === "integer") return "integer";
  if (kind === "bool") return "boolean";
  return "string";
}

function coerceToolResult(value: unknown): JsonObject {
  if (value instanceof ToolResult) {
    const result: JsonObject = {
      content: value.content,
      isError: value.isError,
    };
    if (value.structuredContent !== undefined) {
      result.structuredContent = value.structuredContent;
    }
    return result;
  }
  if (isObject(value)) {
    return {
      content: [{ type: "text", text: encodeWire(value) }],
      isError: false,
      structuredContent: value,
    };
  }
  return {
    content: [{ type: "text", text: renderText(value) }],
    isError: false,
  };
}

function toolError(message: string): JsonObject {
  return {
    content: [{ type: "text", text: message }],
    isError: true,
  };
}

function renderText(value: unknown): string {
  if (value === null || value === undefined) return "";
  if (typeof value === "string") return value;
  return encodeWire(value);
}

async function* readBoundedFrames(input: Readable): AsyncGenerator<Frame> {
  let parts: Buffer[] = [];
  let length = 0;
  let overflowed = false;
  for await (const rawChunk of input) {
    const chunk = Buffer.isBuffer(rawChunk)
      ? rawChunk
      : Buffer.from(String(rawChunk), "utf8");
    let offset = 0;
    while (offset < chunk.length) {
      const newline = chunk.indexOf(0x0a, offset);
      const end = newline === -1 ? chunk.length : newline;
      if (!overflowed) {
        const segmentLength = end - offset;
        if (length + segmentLength > MAX_LINE_BYTES) {
          parts = [];
          length = 0;
          overflowed = true;
        } else if (segmentLength > 0) {
          const copy = Buffer.from(chunk.subarray(offset, end));
          parts.push(copy);
          length += copy.length;
        }
      }
      if (newline === -1) break;
      if (overflowed) {
        yield { overflowed: true };
      } else {
        let bytes = Buffer.concat(parts, length);
        if (bytes.length > 0 && bytes[bytes.length - 1] === 0x0d) {
          bytes = bytes.subarray(0, bytes.length - 1);
        }
        yield { bytes, overflowed: false };
      }
      parts = [];
      length = 0;
      overflowed = false;
      offset = newline + 1;
    }
  }
  if (overflowed) {
    yield { overflowed: true };
  } else if (length > 0) {
    yield { bytes: Buffer.concat(parts, length), overflowed: false };
  }
}

function validateProgressNumber(value: number, field: string): void {
  if (!Number.isFinite(value) || value < 0) {
    throw new TypeError(`${field} must be a finite non-negative number`);
  }
}

function isObject(value: unknown): value is JsonObject {
  return typeof value === "object"
    && value !== null
    && !Array.isArray(value)
    && !(value instanceof WireDecimal);
}

function isWireNumber(value: unknown): value is number | bigint | WireDecimal {
  return (
    (typeof value === "number" && Number.isFinite(value))
    || typeof value === "bigint"
    || value instanceof WireDecimal
  );
}

function isRequestId(value: unknown): value is WireJsonValue {
  return value === null
    || (typeof value === "string" && !hasUnpairedSurrogate(value))
    || isWireNumber(value);
}

function isProgressToken(value: unknown): value is WireJsonValue {
  if (typeof value === "string") return !hasUnpairedSurrogate(value);
  if (typeof value === "number") return Number.isSafeInteger(value);
  if (typeof value === "bigint") return true;
  if (value instanceof WireDecimal) {
    try {
      wireIntegerToJs(value);
      return true;
    } catch {
      return false;
    }
  }
  return false;
}

function requestKey(value: unknown): string {
  if (!isRequestId(value)) {
    throw new TypeError("request id must be a string, number, or null");
  }
  return encodeWire(value);
}

function encodeWire(value: unknown): string {
  return stringifyWireJson(value as WireJsonValue);
}

function deepClone<T>(value: T): T {
  if (value instanceof WireDecimal) {
    return new WireDecimal(value.lexeme) as T;
  }
  if (Array.isArray(value)) {
    return value.map((item) => deepClone(item)) as T;
  }
  if (isObject(value)) {
    return Object.fromEntries(
      Object.entries(value).map(([key, item]) => [key, deepClone(item)]),
    ) as T;
  }
  return value;
}

function deepFreeze<T>(value: T): T {
  if (value instanceof WireDecimal) return Object.freeze(value);
  if (Array.isArray(value)) {
    for (const item of value) deepFreeze(item);
    return Object.freeze(value);
  }
  if (isObject(value)) {
    for (const item of Object.values(value)) deepFreeze(item);
    return Object.freeze(value);
  }
  return value;
}

function valuesEqual(left: unknown, right: unknown): boolean {
  if (isWireNumber(left) && isWireNumber(right)) {
    return decimalKey(left) === decimalKey(right);
  }
  return encodeWire(left) === encodeWire(right);
}

function choiceKey(value: unknown): string {
  return isWireNumber(value) ? `number:${decimalKey(value)}` : encodeWire(value);
}

function decimalKey(value: number | bigint | WireDecimal): string {
  const lexeme = value instanceof WireDecimal
    ? value.lexeme
    : value.toString();
  const match = /^(-?)(\d+)(?:\.(\d*))?(?:[eE]([+-]?\d+))?$/.exec(lexeme);
  if (match === null) return lexeme;
  const negative = match[1] === "-";
  const fraction = match[3] ?? "";
  const exponent = Number(match[4] ?? "0");
  let digits = `${match[2]}${fraction}`.replace(/^0+/, "");
  let scale = fraction.length - exponent;
  if (digits === "") return "0";
  while (digits.endsWith("0")) {
    digits = digits.slice(0, -1);
    scale -= 1;
  }
  return `${negative ? "-" : ""}${digits}e${-scale}`;
}

function asError(value: unknown): Error {
  return value instanceof Error ? value : new Error(String(value));
}

function hasUnpairedSurrogate(value: string): boolean {
  for (let index = 0; index < value.length; index += 1) {
    const code = value.charCodeAt(index);
    if (code >= 0xd800 && code <= 0xdbff) {
      if (index + 1 >= value.length) return true;
      const next = value.charCodeAt(index + 1);
      if (next < 0xdc00 || next > 0xdfff) return true;
      index += 1;
    } else if (code >= 0xdc00 && code <= 0xdfff) {
      return true;
    }
  }
  return false;
}
