// AI helper for Claw OS Node apps.
//
// Every Node app that needs a model (LLM, embedding, image, TTS, STT,
// vision, video) goes through this module. It shells out to
// `cos ai chat --app <id>` — the single authoritative entry point for
// AI requests of every modality. The kernel derives the modality (and
// the underlying caps verb) from the request shape, then runs the
// capability check, prompt-origin allowlist, per-month budget, safety
// pipeline, and audit before any model sees the prompt.
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

import {
  BridgeError,
  Denied,
  Unavailable,
  asObject,
  cosCallJson,
  hasError,
} from "./transport";

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
  units: number;
}

export interface Budget {
  period: string;
  unitsUsed: number;
  unitsCap: number;
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
  input: Record<string, unknown>;
}

export interface AiResponse {
  text: string;
  model: string;
  provider: string;
  verb: string;
  embedding: number[];
  outputPath?: string;
  usage: Usage;
  budget: Budget;
  review: Review;
  toolCalls: ProposedToolCall[];
  raw: Record<string, unknown>;
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
  embed?: boolean;
  imageInput?: string;
  imageOutput?: string;
  audioInput?: string;
  audioOutput?: string;
  videoInput?: string;
  videoOutput?: string;
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

/** Embed text into a vector. Result lives at `response.embedding`. */
export function embed(
  prompt: string,
  opts: Pick<ChatOptions, "origin" | "maxUnits" | "appId"> = {},
): AiResponse {
  if (!prompt || !prompt.trim()) throw new AiError("embed: prompt must be non-empty");
  return dispatch({
    modality: "embed",
    prompt,
    origin: opts.origin ?? "trusted",
    maxUnits: opts.maxUnits,
    appId: opts.appId,
    embed: true,
  });
}

/** Generate an image from a prompt; the gate writes it to `output`. */
export function imageGenerate(
  prompt: string,
  output: string,
  opts: Pick<ChatOptions, "origin" | "maxUnits" | "appId"> = {},
): AiResponse {
  if (!prompt || !prompt.trim())
    throw new AiError("imageGenerate: prompt must be non-empty");
  return dispatch({
    modality: "image.generate",
    prompt,
    origin: opts.origin ?? "trusted",
    maxUnits: opts.maxUnits,
    appId: opts.appId,
    imageOutput: output,
  });
}

/** Caption / classify an image with no prompt. */
export function imageAnalyze(
  image: string,
  opts: Pick<ChatOptions, "origin" | "maxUnits" | "appId"> = {},
): AiResponse {
  return dispatch({
    modality: "image.analyze",
    prompt: null,
    origin: opts.origin ?? "trusted",
    maxUnits: opts.maxUnits,
    appId: opts.appId,
    imageInput: image,
  });
}

/** Answer a textual question about an image. */
export function visionAnalyze(
  prompt: string,
  image: string,
  opts: Pick<ChatOptions, "origin" | "maxUnits" | "system" | "appId"> = {},
): AiResponse {
  if (!prompt || !prompt.trim())
    throw new AiError("visionAnalyze: prompt must be non-empty");
  return dispatch({
    modality: "vision.analyze",
    prompt,
    origin: opts.origin ?? "trusted",
    maxUnits: opts.maxUnits,
    system: opts.system,
    appId: opts.appId,
    imageInput: image,
  });
}

/** Synthesize speech from text; the gate writes audio to `output`. */
export function audioTts(
  prompt: string,
  output: string,
  opts: Pick<ChatOptions, "origin" | "maxUnits" | "appId"> = {},
): AiResponse {
  if (!prompt || !prompt.trim())
    throw new AiError("audioTts: prompt must be non-empty");
  return dispatch({
    modality: "audio.tts",
    prompt,
    origin: opts.origin ?? "trusted",
    maxUnits: opts.maxUnits,
    appId: opts.appId,
    audioOutput: output,
  });
}

/** Transcribe speech to text from an audio file. */
export function audioStt(
  audio: string,
  opts: Pick<ChatOptions, "origin" | "maxUnits" | "appId"> = {},
): AiResponse {
  return dispatch({
    modality: "audio.stt",
    prompt: null,
    origin: opts.origin ?? "trusted",
    maxUnits: opts.maxUnits,
    appId: opts.appId,
    audioInput: audio,
  });
}

/** Generate a video from a prompt; the gate writes it to `output`. */
export function videoGenerate(
  prompt: string,
  output: string,
  opts: Pick<ChatOptions, "origin" | "maxUnits" | "appId"> = {},
): AiResponse {
  if (!prompt || !prompt.trim())
    throw new AiError("videoGenerate: prompt must be non-empty");
  return dispatch({
    modality: "video.generate",
    prompt,
    origin: opts.origin ?? "trusted",
    maxUnits: opts.maxUnits,
    appId: opts.appId,
    videoOutput: output,
  });
}

/** Answer a textual question about a video. */
export function videoAnalyze(
  prompt: string,
  video: string,
  opts: Pick<ChatOptions, "origin" | "maxUnits" | "appId"> = {},
): AiResponse {
  if (!prompt || !prompt.trim())
    throw new AiError("videoAnalyze: prompt must be non-empty");
  return dispatch({
    modality: "video.analyze",
    prompt,
    origin: opts.origin ?? "trusted",
    maxUnits: opts.maxUnits,
    appId: opts.appId,
    videoInput: video,
  });
}

/** Current-period budget snapshot for an app. */
export function budget(appId?: string): Budget {
  const app = appId || process.env.COS_APP_ID;
  if (!app) throw new AiError("budget: app_id is required");
  const { envelope, status } = cosCallJson("cos agent budget show", [
    "agent",
    "budget",
    "show",
    app,
  ]);
  const env = asObject(envelope);
  if (status !== 0 || hasError(envelope)) {
    throw new AiUnavailable(
      `cos agent budget show failed: ${String(env["error"] ?? status)}`,
    );
  }
  return parseBudget(env);
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
  if (a.prompt != null) argv.push("--prompt", a.prompt);
  if (a.maxUnits != null) argv.push("--max-units", String(a.maxUnits));
  if (a.system != null) argv.push("--system", a.system);
  if (a.embed) argv.push("--embed");
  if (a.imageInput != null) argv.push("--image-input", a.imageInput);
  if (a.imageOutput != null) argv.push("--image-output", a.imageOutput);
  if (a.audioInput != null) argv.push("--audio-input", a.audioInput);
  if (a.audioOutput != null) argv.push("--audio-output", a.audioOutput);
  if (a.videoInput != null) argv.push("--video-input", a.videoInput);
  if (a.videoOutput != null) argv.push("--video-output", a.videoOutput);
  if (a.tools && a.tools.length) argv.push("--tools", a.tools.join(","));

  let outcome;
  try {
    outcome = cosCallJson(`cos ai ${a.modality}`, argv);
  } catch (e) {
    if (e instanceof Unavailable) throw new AiUnavailable(e.message);
    throw e;
  }
  const env = asObject(outcome.envelope);
  if (outcome.status !== 0 || hasError(outcome.envelope)) {
    raiseForError(env);
  }
  return parseResponse(env);
}

function raiseForError(env: Record<string, unknown>): never {
  const msg = String(env["error"] ?? "").toLowerCase();
  if (msg.includes("budget") && (msg.includes("exceed") || msg.includes("over"))) {
    throw new AiBudgetExceeded(env);
  }
  if (msg.includes("safety") || msg.includes("redact") || msg.includes("injection")) {
    throw new AiSafetyViolation(env);
  }
  throw new AiDenied(env);
}

function num(v: unknown): number {
  const n = typeof v === "number" ? v : Number(v);
  return Number.isFinite(n) ? n : 0;
}

function parseBudget(blk: Record<string, unknown>): Budget {
  return {
    period: String(blk["period"] ?? ""),
    unitsUsed: num(blk["units_used"]),
    unitsCap: num(blk["units_cap"]),
  };
}

function parseResponse(env: Record<string, unknown>): AiResponse {
  const usage = asObject(env["usage"]);
  const review = asObject(env["review"]);
  const embeddingRaw = env["embedding"];
  const rawCalls = env["tool_calls"];
  const toolCalls: ProposedToolCall[] = Array.isArray(rawCalls)
    ? rawCalls
        .filter((tc): tc is Record<string, unknown> => typeof tc === "object" && tc !== null)
        .map((tc) => ({
          id: String(tc["id"] ?? ""),
          name: String(tc["name"] ?? ""),
          input: asObject(tc["input"]),
        }))
    : [];
  return {
    text: String(env["text"] ?? ""),
    model: String(env["model"] ?? ""),
    provider: String(env["provider"] ?? ""),
    verb: String(env["verb"] ?? ""),
    embedding: Array.isArray(embeddingRaw) ? embeddingRaw.map(num) : [],
    outputPath: env["output_path"] != null ? String(env["output_path"]) : undefined,
    usage: {
      inputTokens: num(usage["input_tokens"]),
      outputTokens: num(usage["output_tokens"]),
      units: num(usage["units"]),
    },
    budget: parseBudget(asObject(env["budget"])),
    review: {
      safety: String(review["safety"] ?? "strict"),
      promptRedacted: Boolean(review["prompt_redacted"] ?? false),
    },
    toolCalls,
    raw: env,
  };
}
