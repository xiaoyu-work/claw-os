// Test-only helper: installs a fake `cos` binary so transport tests
// run without a real kernel. Not part of the published package
// (excluded from the build tsconfig).

import { mkdtempSync, writeFileSync, chmodSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

export interface FakeCos {
  /** Absolute path to the fake binary; assign to CLAW_COS_BIN. */
  bin: string;
  /** Path the fake writes its received argv to (one arg per line). */
  argvOut: string;
}

export function wireSuccess(data: unknown): string {
  return JSON.stringify({ ok: true, wire_version: 1, data });
}

export function wireSuccessJson(dataJson: string): string {
  return `{"ok":true,"wire_version":1,"data":${dataJson}}`;
}

export function wireError(error: string, code: string): string {
  return JSON.stringify({ ok: false, wire_version: 1, error, code });
}

/**
 * Write a fake `cos` executable that prints `stdout` and exits with
 * `exitCode`, recording the argv it received to a sidecar file. The
 * script is a Node program with a shebang so spawnSync can exec it
 * directly on Linux.
 */
export function installFakeCos(stdout: string, exitCode = 0): FakeCos {
  const dir = mkdtempSync(join(tmpdir(), "claw-fakecos-"));
  const bin = join(dir, "cos");
  const argvOut = join(dir, "argv.txt");
  const script = `#!/usr/bin/env node
const fs = require("fs");
fs.writeFileSync(${JSON.stringify(argvOut)}, process.argv.slice(2).join("\\n"));
process.stdout.write(${JSON.stringify(stdout)});
process.exit(${exitCode});
`;
  writeFileSync(bin, script);
  chmodSync(bin, 0o755);
  return { bin, argvOut };
}

/** Run `fn` with CLAW_COS_BIN (and optionally COS_APP_ID) set, then
 * restore the previous environment. */
export function withCos<T>(
  fake: FakeCos,
  env: Record<string, string | undefined>,
  fn: () => T,
): T {
  const saved: Record<string, string | undefined> = {
    CLAW_COS_BIN: process.env.CLAW_COS_BIN,
    COS_APP_ID: process.env.COS_APP_ID,
  };
  process.env.CLAW_COS_BIN = fake.bin;
  for (const [k, v] of Object.entries(env)) {
    if (v === undefined) delete process.env[k];
    else process.env[k] = v;
  }
  try {
    return fn();
  } finally {
    for (const [k, v] of Object.entries(saved)) {
      if (v === undefined) delete process.env[k];
      else process.env[k] = v;
    }
  }
}
