// AI helper for Claw OS Node apps.
//
// `chat` is the stable model API and shells out to
// `cos ai chat --app <id>`. The kernel derives `ai.chat` or
// `ai.chat.untrusted` from origin, then runs capability checks, budget,
// safety, and audit.
//
// Apps never name a verb and never pick a model. They describe what
// they want; the gate picks the verb and the machine owner configures
// the one provider/model in /etc/cos/agent.toml. Importing a provider
// SDK directly (openai, anthropic, ...) is refused at install time by
// `cos app lint` — this module is the only sanctioned route to a model.
//
//   import { ai } from "@claw-os/sdk";
//   const res = ai.chat("Summarise this", { origin: "external-content" });
//   console.log(res.text, res.usage.units);

import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

import {
  BridgeError,
  Denied,
  Unavailable,
  cosCallJson,
} from "./transport";
import {
  WireDecodeError,
  materializeWireValue,
  type WireJsonValue,
  validateAi,
  validateBudgetShow,
  type AiBudget,
} from "./generated";

/** Base class for every error this module raises. */
export class AiError extends BridgeError {}
/** The `cos` binary could not be invoked or returned garbage. */
export class AiUnavailable extends AiError {}
/** A gate (capability / origin / budget / safety) refused the call.
 * `payload` is the structured envelope the kernel returned. */
export class AiDenied extends AiError {
  readonly payload: Record<string, unknown>;
  constructor(payload: Record<string, unknown>) {
    super(String(payload["error"] ?? "AI call denied"));
    this.payload = payload;
  }
}
/** The per-app monthly budget was exhausted. */
export class AiBudgetExceeded extends AiDenied {}
/** The safety pipeline refused the request. */
export class AiSafetyViolation extends AiDenied {}

export interface Usage {
  inputTokens: number;
  outputTokens: number;
  units: number | bigint;
}

export interface Budget {
  period: string;
  unitsUsed: number | bigint;
  unitsCap: number | bigint;
}

export interface Review {
  safety: string;
  promptRedacted: boolean;
}

/** A tool call the model proposed but the gate did NOT execute. Apps
 * decide whether to fulfil any by calling `tools.call` with the same
 * name + input; `id` echoes back to the provider on the next turn. */
export interface ProposedToolCall {
  id: string;
  name: string;
  input: WireJsonValue;
}

export interface AiResponse {
  text: string;
  model: string;
  provider: string;
  verb: string;
  usage: Usage;
  budget: Budget;
  review: Review;
  toolCalls: ProposedToolCall[];
  raw: Record<string, WireJsonValue>;
}

export interface ChatOptions {
  /** Prompt provenance. Pass `"external-content"` for any third-party
   * text (emails, web pages, file contents) so the strict safety
   * pipeline runs and the gate picks `ai.chat.untrusted`. */
  origin?: string;
  maxUnits?: number;
  system?: string;
  /** App identity. Defaults to `COS_APP_ID`. */
  appId?: string;
  /** Catalog tool names the model may *propose*; never executed by the
   * gate — proposals return in `AiResponse.toolCalls`. */
  tools?: string[];
}

interface DispatchArgs {
  modality: string;
  prompt?: string | null;
  origin: string;
  maxUnits?: number;
  system?: string;
  appId?: string;
  tools?: string[];
}

/** Single-shot chat completion through the kernel's AI gate. */
export function chat(prompt: string, opts: ChatOptions = {}): AiResponse {
  if (!prompt || !prompt.trim()) throw new AiError("chat: prompt must be non-empty");
  return dispatch({
    modality: "chat",
    prompt,
    origin: opts.origin ?? "trusted",
    maxUnits: opts.maxUnits,
    system: opts.system,
    appId: opts.appId,
    tools: opts.tools,
  });
}

/** Current-period budget snapshot for an app. */
export function budget(appId?: string): Budget {
  const app = appId || process.env.COS_APP_ID;
  if (!app) throw new AiError("budget: app_id is required");
  let data: unknown;
  try {
    data = cosCallJson("cos agent budget show", ["agent", "budget", "show", app]);
  } catch (error) {
    if (error instanceof Denied || error instanceof Unavailable) {
      throw new AiUnavailable(error.message);
    }
    throw error;
  }
  try {
    validateBudgetShow(data);
  } catch (error) {
    if (error instanceof WireDecodeError) {
      throw new AiUnavailable(`budget response decode failed: ${error.message}`);
    }
    throw error;
  }
  return {
    period: data.period,
    unitsUsed: data.units_used,
    unitsCap: 0,
  };
}

function resolveApp(modality: string, appId?: string): string {
  const app = appId || process.env.COS_APP_ID;
  if (!app) {
    throw new AiError(
      `${modality}: app_id is required (pass appId or set COS_APP_ID)`,
    );
  }
  return app;
}

function dispatch(a: DispatchArgs): AiResponse {
  const app = resolveApp(a.modality, a.appId);
  const argv = ["ai", "chat", "--app", app, "--origin", a.origin];
  const privateDir = mkdtempSync(join(tmpdir(), "claw-ai-"));
  try {
    if (a.prompt != null) {
      const promptPath = join(privateDir, "prompt");
      writeFileSync(promptPath, a.prompt, { encoding: "utf8", mode: 0o600, flag: "wx" });
      argv.push("--prompt-file", promptPath);
    }
    if (a.maxUnits != null) argv.push("--max-units", String(a.maxUnits));
    if (a.system != null) {
      const systemPath = join(privateDir, "system");
      writeFileSync(systemPath, a.system, { encoding: "utf8", mode: 0o600, flag: "wx" });
      argv.push("--system-file", systemPath);
    }
    if (a.tools && a.tools.length) argv.push("--tools", a.tools.join(","));

    let data: unknown;
    try {
      data = cosCallJson(`cos ai ${a.modality}`, argv);
    } catch (e) {
      if (e instanceof Denied) raiseForError(e.payload);
      if (e instanceof Unavailable) throw new AiUnavailable(e.message);
      throw e;
    }
    return parseResponse(data);
  } finally {
    rmSync(privateDir, { recursive: true, force: true });
  }
}

function raiseForError(env: Record<string, unknown>): never {
  const code = String(env["code"]);
  if (code === "BUDGET_EXCEEDED") throw new AiBudgetExceeded(env);
  if (code === "SAFETY_VIOLATION") throw new AiSafetyViolation(env);
  throw new AiDenied(env);
}

function parseBudget(blk: AiBudget): Budget {
  return {
    period: blk.period,
    unitsUsed: blk.units_used,
    unitsCap: blk.units_cap,
  };
}

function parseResponse(env: unknown): AiResponse {
  try {
    validateAi(env);
  } catch (error) {
    if (error instanceof WireDecodeError) {
      throw new AiUnavailable(`ai response decode failed: ${error.message}`);
    }
    throw error;
  }
  const usage = env.usage;
  const review = env.review;
  const toolCalls: ProposedToolCall[] = (env.tool_calls ?? []).map((toolCall) => ({
    id: toolCall.id,
    name: toolCall.name,
    input: materializeWireValue(toolCall.input),
  }));
  return {
    text: env.text,
    model: env.model,
    provider: env.provider,
    verb: env.verb,
    usage: {
      inputTokens: usage.input_tokens,
      outputTokens: usage.output_tokens,
      units: usage.units,
    },
    budget: parseBudget(env.budget),
    review: {
      safety: review.safety,
      promptRedacted: review.prompt_redacted,
    },
    toolCalls,
    raw: materializeWireValue(env) as Record<string, WireJsonValue>,
  };
}
