// Shared helper: execute a cos command and return parsed JSON.

import { execFileSync } from "node:child_process";

export function cos(app: string, command: string, args: string[] = []): unknown {
  const result = execFileSync("cos", [app, command, ...args], {
    encoding: "utf-8",
    timeout: 300_000,
    maxBuffer: 10 * 1024 * 1024,
  });
  try {
    return JSON.parse(result.trim());
  } catch {
    return { raw: result.trim() };
  }
}

export function cosRaw(app: string, command: string, args: string[] = []): string {
  return execFileSync("cos", [app, command, ...args], {
    encoding: "utf-8",
    timeout: 300_000,
    maxBuffer: 10 * 1024 * 1024,
  }).trim();
}
