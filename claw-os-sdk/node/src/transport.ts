// Subprocess transport shared by every Node SDK module.
//
// Like the Python and Rust SDKs, the Node SDK is a thin client over
// wire protocol v1: it shells out to the `cos` binary, which reads a
// request from argv and writes a JSON envelope to stdout. The
// subprocess model is intentional — identity, audit, and session
// context are inherited from process ancestry (kernel-spawned parent →
// app process → cos child). A pure in-process binding could not prove
// "App X is making this call".

import { spawnSync } from "node:child_process";

/** Default per-call timeout. Bounded so a wedged child never blocks
 * the calling app forever, but long enough for slow providers. */
export const DEFAULT_TIMEOUT_MS = 60_000;

/** Max bytes captured from a child's stdout/stderr (8 MiB). Generous
 * enough for embeddings / base64 artifact paths without unbounded RAM. */
const MAX_BUFFER = 8 * 1024 * 1024;

/**
 * Resolve the `cos` binary.
 *
 * Honors `CLAW_COS_BIN` then `COS_BIN` (both used across the SDK family
 * and dev/test setups), falling back to `cos` on `$PATH`.
 */
export function cosBinary(): string {
  return process.env.CLAW_COS_BIN || process.env.COS_BIN || "cos";
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

/** Outcome of a `cos` invocation: the parsed envelope plus the process
 * exit status (so callers can treat a non-zero exit as failure even
 * when stdout happened to be valid JSON). */
export interface CosOutcome {
  envelope: unknown;
  status: number;
}

/**
 * Run `cos <args>` synchronously and parse its stdout as JSON.
 *
 * `label` names the logical call (e.g. `"cos ai chat"`) for error
 * messages. Throws {@link Unavailable} for transport problems (binary
 * missing, timeout, non-JSON). The envelope is returned untouched;
 * callers decide what a non-zero `status` or an `error` field means in
 * their domain.
 */
export function cosCallJson(
  label: string,
  args: string[],
  timeoutMs: number = DEFAULT_TIMEOUT_MS,
): CosOutcome {
  const bin = cosBinary();
  const res = spawnSync(bin, args, {
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

  const text = (res.stdout || "").trim() || (res.stderr || "").trim();
  if (!text) {
    throw new Unavailable(`${label} returned no output (exit ${res.status})`);
  }

  let envelope: unknown;
  try {
    envelope = JSON.parse(text);
  } catch {
    throw new Unavailable(`${label} returned non-JSON output: ${truncate(text)}`);
  }

  return { envelope, status: res.status ?? 0 };
}

/** True when an envelope is an object carrying an `error` field. */
export function hasError(envelope: unknown): envelope is Record<string, unknown> {
  return (
    typeof envelope === "object" &&
    envelope !== null &&
    "error" in (envelope as Record<string, unknown>)
  );
}

/** Narrow an envelope to a plain object, or `{}` if it isn't one. */
export function asObject(envelope: unknown): Record<string, unknown> {
  return typeof envelope === "object" && envelope !== null
    ? (envelope as Record<string, unknown>)
    : {};
}
