// Tool helper for Claw OS Node apps.
//
// Apps that want to fulfil a model-proposed tool call (returned in
// `ai.AiResponse.toolCalls` after `ai.chat(..., { tools: [...] })`)
// shell out through this module to `cos ai tool <name> --app <id>
// --args <json>`. The kernel resolves the name against the catalog,
// derives the caps verb + scope, runs caps::require under the app's own
// grants, executes the implementation, and writes an audit row.
//
//   import { ai, tools } from "@claw-os/sdk";
//   const proposal = ai.chat("Summarise /etc/hostname", {
//     tools: tools.forChat("fs.read_text"),
//   });
//   for (const c of proposal.toolCalls) {
//     const r = tools.call(c.name, c.input);
//   }

import { BridgeError, Unavailable, asObject, cosCallJson, hasError } from "./transport";
import {
  WireDecodeError,
  validateTool,
  validateToolCatalog,
} from "./generated";

/** Base class for every error this module raises. */
export class ToolError extends BridgeError {}
/** The `cos` binary could not be invoked or returned garbage. */
export class ToolUnavailable extends ToolError {}

/** A gate (capability / unknown tool / args shape) refused the call.
 * `payload` is the structured envelope the kernel returned. */
export class ToolDenied extends ToolError {
  readonly payload: Record<string, unknown>;
  constructor(payload: Record<string, unknown>) {
    super(String(payload["error"] ?? "Tool call denied"));
    this.payload = payload;
  }
}

/** The kernel-mediated result of one tool invocation. `value` is the
 * JSON the catalog implementation produced (shape is per-tool). */
export interface ToolResult {
  name: string;
  appId: string;
  status: string;
  value: unknown;
  raw: Record<string, unknown>;
}

/** One row from `cos ai tools`. */
export interface CatalogEntry {
  name: string;
  summary: string;
  verb: string;
  stability: string;
  argsSchema?: Record<string, unknown>;
  returnsSchema?: Record<string, unknown>;
  raw: Record<string, unknown>;
}

/**
 * Invoke a catalog tool through the kernel.
 *
 * Throws {@link ToolDenied} for anything the gate refused (unknown
 * tool, missing capability, malformed args) and {@link ToolUnavailable}
 * for transport problems.
 */
export function call(
  name: string,
  args: Record<string, unknown> = {},
  opts: { appId?: string } = {},
): ToolResult {
  if (!name || typeof name !== "string") {
    throw new ToolError("call: name must be a non-empty string");
  }
  const app = opts.appId || process.env.COS_APP_ID;
  if (!app) {
    throw new ToolError(`${name}: app_id is required (pass appId or set COS_APP_ID)`);
  }

  const argv = ["ai", "tool", name, "--app", app, "--args", JSON.stringify(args ?? {})];
  let outcome;
  try {
    outcome = cosCallJson(`cos ai tool ${name}`, argv);
  } catch (e) {
    if (e instanceof Unavailable) throw new ToolUnavailable(e.message);
    throw e;
  }
  if (outcome.status !== 0 || hasError(outcome.envelope)) {
    throw new ToolDenied(asObject(outcome.envelope));
  }
  try {
    validateTool(outcome.envelope);
  } catch (error) {
    if (error instanceof WireDecodeError) {
      throw new ToolUnavailable(`tool result decode failed: ${error.message}`);
    }
    throw error;
  }
  const env = outcome.envelope;
  return {
    name: env.tool,
    appId: env.app_id,
    status: env.status,
    value: env.result,
    raw: env,
  };
}

/**
 * Return the live tool catalog as exposed by `cos ai tools`.
 *
 * Apps shouldn't hard-code tool names without consulting this list; the
 * catalog evolves and a tool can be deprecated or renamed between
 * releases.
 */
export function catalog(): CatalogEntry[] {
  let outcome;
  try {
    outcome = cosCallJson("cos ai tools", ["ai", "tools"]);
  } catch (e) {
    if (e instanceof Unavailable) throw new ToolUnavailable(e.message);
    throw e;
  }
  if (outcome.status !== 0 || hasError(outcome.envelope)) {
    throw new ToolDenied(asObject(outcome.envelope));
  }
  try {
    validateToolCatalog(outcome.envelope);
  } catch (error) {
    if (error instanceof WireDecodeError) {
      throw new ToolUnavailable(`catalog decode failed: ${error.message}`);
    }
    throw error;
  }
  const env = outcome.envelope;
  return env.tools.map((row) => ({
      name: row.name,
      summary: row.summary,
      verb: row.verb,
      stability: row.stability,
      argsSchema: row.args_schema,
      returnsSchema: row.returns_schema,
      raw: { ...row },
    }));
}

/**
 * Normalise tool names for `ai.chat`'s `tools` option: trim whitespace
 * and drop empties, so `forChat("fs.read_text", " kv.get ", "")`
 * collapses to two clean entries.
 */
export function forChat(...names: string[]): string[] {
  const out: string[] = [];
  for (const n of names) {
    if (typeof n !== "string") {
      throw new ToolError(`forChat: tool names must be strings, got ${String(n)}`);
    }
    const s = n.trim();
    if (s) out.push(s);
  }
  return out;
}
