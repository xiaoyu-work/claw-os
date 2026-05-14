import { promises as fs } from "node:fs";
import os from "node:os";
import path from "node:path";

/**
 * The agent web UI talks to a local Rust daemon (`cos-agent-bridge`) over
 * HTTP on 127.0.0.1. In dev (`next dev`) these route handlers proxy to it.
 * In production the bridge serves the exported SPA + /api/* itself, so
 * these handlers are bypassed.
 */
export async function getBridgePort(): Promise<number> {
  const envPort = process.env.COS_AGENT_BRIDGE_PORT;
  if (envPort) {
    const parsed = Number.parseInt(envPort, 10);
    if (Number.isFinite(parsed) && parsed > 0) {
      return parsed;
    }
  }

  const runtimeDir =
    process.env.XDG_RUNTIME_DIR && process.env.XDG_RUNTIME_DIR.length > 0
      ? process.env.XDG_RUNTIME_DIR
      : os.tmpdir();
  const portFile = path.join(runtimeDir, "cos-agent-bridge.port");
  const content = await fs.readFile(portFile, "utf8");
  const parsed = Number.parseInt(content.trim(), 10);
  if (!Number.isFinite(parsed) || parsed <= 0) {
    throw new Error(
      `cos-agent-bridge port file at ${portFile} did not contain a valid port`,
    );
  }
  return parsed;
}

export async function bridgeUrl(pathname: string): Promise<string> {
  const port = await getBridgePort();
  const normalized = pathname.startsWith("/") ? pathname : `/${pathname}`;
  return `http://127.0.0.1:${port}${normalized}`;
}
