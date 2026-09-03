// Subprocess transport shared by every Node SDK module.
//
// Like the Python and Rust SDKs, the Node SDK is a thin client over
// wire protocol v1: it shells out to the `cos` binary, which reads
// non-sensitive routing flags from argv and writes a JSON envelope to stdout.
// AI prompt bodies are passed through private temporary files. The
// subprocess model is intentional — identity, audit, and session
// context are inherited from process ancestry (kernel-spawned parent →
// app process → cos child). A pure in-process binding could not prove
// "App X is making this call".

import { spawnSync } from "node:child_process";
import { WireDecodeError, decodeWireJson, validateEnvelope } from "./generated";

/** Default per-call timeout. Bounded so a wedged child never blocks
 * the calling app forever, but long enough for slow providers. */
export const DEFAULT_TIMEOUT_MS = 60_000;

/** Max bytes captured from a child's stdout/stderr (8 MiB). Generous
 * enough for embeddings / base64 artifact paths without unbounded RAM. */
const MAX_BUFFER = 8 * 1024 * 1024;
const WIRE_ARG = "--wire=1";

/**
 * Resolve the `cos` binary.
 *
 * Honors `CLAW_COS_BIN`, falling back to `cos` on `$PATH`.
 */
export function cosBinary(): string {
  return process.env.CLAW_COS_BIN || "cos";
}

/** Base class for every error the transport (or a module built on it)
 * raises. */
export class BridgeError extends Error {}

/** The `cos` binary could not be invoked, timed out, or returned
 * something that was not a JSON envelope. */
export class Unavailable extends BridgeError {}

/** A gate (capability / origin / budget / unknown verb / arg shape)
 * refused the call. `payload` holds the structured error envelope the
 * kernel returned, suitable for forwarding back to the agent. */
export class Denied extends BridgeError {
  readonly payload: Record<string, unknown>;
  constructor(payload: Record<string, unknown>) {
    super(String(payload["error"] ?? "call denied"));
    this.payload = payload;
  }
}

function truncate(value: string, limit = 200): string {
  if (value.length <= limit) return value;
  return value.slice(0, limit) + `... [${value.length - limit} more bytes elided]`;
}

/**
 * Run `cos --wire=1 <args>` synchronously and return the success data.
 *
 * `label` names the logical call (e.g. `"cos ai chat"`) for error
 * messages. Throws {@link Unavailable} for transport problems (binary
 * missing, timeout, malformed protocol). Throws {@link Denied} for a
 * valid kernel error envelope.
 */
export function cosCallJson(
  label: string,
  args: string[],
  timeoutMs: number = DEFAULT_TIMEOUT_MS,
): unknown {
  const bin = cosBinary();
  const res = spawnSync(bin, [WIRE_ARG, ...args], {
    encoding: "utf8",
    timeout: timeoutMs,
    maxBuffer: MAX_BUFFER,
  });

  if (res.error) {
    const code = (res.error as NodeJS.ErrnoException).code;
    if (code === "ETIMEDOUT") {
      throw new Unavailable(`${label} timed out after ${timeoutMs}ms`);
    }
    if (code === "ENOENT") {
      throw new Unavailable(
        `the \`${bin}\` binary is not on PATH; cannot reach the kernel ` +
          `(set CLAW_COS_BIN or install cos)`,
      );
    }
    throw new Unavailable(`could not spawn ${bin}: ${res.error.message}`);
  }

  const text = (res.stdout || "").trim();
  if (!text) {
    throw new Unavailable(`${label} returned no wire response (exit ${res.status})`);
  }

  let envelope: unknown;
  try {
    envelope = decodeWireJson(text);
  } catch {
    throw new Unavailable(`${label} returned non-JSON output: ${truncate(text)}`);
  }
  try {
    validateEnvelope(envelope);
  } catch (error) {
    if (error instanceof WireDecodeError) {
      throw new Unavailable(`${label} returned an invalid wire envelope: ${error.message}`);
    }
    throw error;
  }

  const status = res.status ?? -1;
  if (envelope.ok) {
    if (status !== 0) {
      throw new Unavailable(`${label} returned a success envelope with exit ${status}`);
    }
    return envelope.data;
  }
  if (status === 0) {
    throw new Unavailable(`${label} returned an error envelope with exit 0`);
  }
  throw new Denied(envelope);
}
