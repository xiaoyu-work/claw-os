// Desktop GUI bootstrap for Claw OS Node apps.
//
// This is the GUI counterpart to the `ai` (model access) and `tools`
// (cross-app verbs) modules. It does NOT wrap a UI toolkit: a Claw OS
// desktop app draws its own window in whatever toolkit it likes ("World
// A"). This module only hands the app the small amount of kernel
// context it receives once kernel-spawned as a GUI, plus the one
// privileged action a GUI commonly wants — summoning the agent overlay.
//
// When an app declares a `desktop` block in app.json, `cos app install`
// writes a launcher whose Exec is `cos app <id> --gui`. Activating it
// routes the launch back through the kernel, which spawns the app with
// COS_APP_GUI=1 and COS_APP_ID set, so identity/audit/consent apply to
// the GUI exactly as to the headless path.
//
//   import { gui } from "@claw-os/sdk";
//   export function run(command, args) {
//     if (gui.isGuiLaunch(command)) {
//       const ctx = gui.context(args);
//       startMyWindow(ctx);   // your toolkit, your loop
//       return;
//     }
//     // ... handle one-shot operations here ...
//   }

import { spawn } from "node:child_process";
import { lstatSync } from "node:fs";
import { createConnection } from "node:net";

import * as ai from "./ai";
import * as tools from "./tools";

/** Command value the bridge passes (and the default `desktop.exec`)
 * when an app is launched as a GUI. */
export const GUI_COMMAND = "--gui";
const ASK_CLAW_LAUNCHER = "/usr/local/bin/cos-ask-claw-launcher";
const ASK_CLAW_PROTOCOL = 1;
const ASK_CLAW_REQUEST_LIMIT = 32 * 1024;

/**
 * Return `true` when the current invocation is a desktop GUI launch.
 *
 * Detection prefers the `COS_APP_GUI` environment variable the bridge
 * sets for the long-lived GUI process. As a fallback (so apps with a
 * custom `desktop.exec` still work) a `command` equal to
 * {@link GUI_COMMAND} is also treated as a GUI launch.
 */
export function isGuiLaunch(command?: string): boolean {
  if (process.env.COS_APP_GUI === "1") return true;
  return command !== undefined && command === GUI_COMMAND;
}

/** The kernel context handed to a desktop app at launch. */
export class GuiContext {
  readonly appId: string;
  readonly files: string[];
  /** Gated model-access module. */
  readonly ai = ai;
  /** Cross-app verb-call module. */
  readonly tools = tools;

  constructor(appId: string, files: string[]) {
    this.appId = appId;
    this.files = files;
  }

  /**
   * Summon the system "Ask Claw" agent overlay through the fixed packaged
   * launcher's authenticated Unix-socket protocol. Pass
   * `hint` to ground the agent's first response in the app's current
   * state without polluting the visible chat transcript.
   *
   * The overlay is detached: it outlives this call and is not tied to
   * the app's stdio or event loop. Resolves once the helper has
   * authenticated and accepted the request; rejects if the
   * overlay binary is missing (e.g. a headless
   * box with no desktop shell).
   */
  async openAgentOverlay(hint?: string): Promise<void> {
    validateAskClawLauncher();
    const child = spawn(ASK_CLAW_LAUNCHER, ["--protocol", String(ASK_CLAW_PROTOCOL)], {
      stdio: ["ignore", "pipe", "ignore"],
    });
    try {
      const announcement = await new Promise<string>((resolve, reject) => {
        let settled = false;
        const timer = setTimeout(() => {
          settled = true;
          reject(new Error("Ask Claw launcher announcement timed out"));
        }, 5000);
        let line = "";
        child.once("error", (err) => {
          if (settled) return;
          settled = true;
          clearTimeout(timer);
          reject(err);
        });
        child.stdout.on("data", (chunk: Buffer) => {
          if (settled) return;
          line += chunk.toString("utf8");
          if (line.length > 256) {
            settled = true;
            clearTimeout(timer);
            reject(new Error("Ask Claw socket announcement is too long"));
            return;
          }
          const newline = line.indexOf("\n");
          if (newline < 0) return;
          settled = true;
          clearTimeout(timer);
          child.stdout.destroy();
          resolve(line.slice(0, newline + 1));
        });
      });
      const match = announcement.match(/^SOCKET 1 @([^\n]+)\n$/);
      if (!match) throw new Error("invalid Ask Claw socket announcement");

      const payload = Buffer.from(JSON.stringify({
        protocol: ASK_CLAW_PROTOCOL,
        app: this.appId,
        hint: hint ?? null,
      }), "utf8");
      if (payload.length > ASK_CLAW_REQUEST_LIMIT) {
        throw new Error("Ask Claw request exceeds the protocol limit");
      }
      await new Promise<void>((resolve, reject) => {
        const socket = createConnection({ path: `\0${match[1]}` });
        let settled = false;
        const timer = setTimeout(() => {
          settled = true;
          socket.destroy();
          reject(new Error("Ask Claw launcher readiness timed out"));
        }, 5000);
        let phase: "ready" | "accepted" = "ready";
        let input = Buffer.alloc(0);
        socket.once("error", (err) => {
          if (settled) return;
          settled = true;
          clearTimeout(timer);
          reject(err);
        });
        socket.on("data", (chunk: Buffer) => {
          if (settled) return;
          input = Buffer.concat([input, chunk]);
          if (phase === "ready" && input.length >= 8) {
            if (!input.subarray(0, 8).equals(Buffer.from("READY 1\n"))) {
              settled = true;
              clearTimeout(timer);
              socket.destroy();
              reject(new Error("unexpected Ask Claw launcher handshake"));
              return;
            }
            input = input.subarray(8);
            phase = "accepted";
            const header = Buffer.allocUnsafe(4);
            header.writeUInt32BE(payload.length);
            socket.write(Buffer.concat([header, payload]));
          }
          if (phase === "accepted" && input.length >= 11) {
            if (!input.subarray(0, 11).equals(Buffer.from("ACCEPTED 1\n"))) {
              settled = true;
              clearTimeout(timer);
              socket.destroy();
              reject(new Error("unexpected Ask Claw acceptance response"));
              return;
            }
            settled = true;
            clearTimeout(timer);
            socket.end();
            resolve();
          }
        });
        socket.once("close", () => {
          if (settled) return;
          settled = true;
          clearTimeout(timer);
          reject(new Error("Ask Claw launcher closed the socket"));
        });
      });
      child.unref();
    } catch (error) {
      child.kill();
      throw error;
    }
  }
}

function validateAskClawLauncher(): void {
  for (const path of ["/usr", "/usr/local", "/usr/local/bin"]) {
    const info = lstatSync(path);
    if (info.isSymbolicLink() || !info.isDirectory() || info.uid !== 0 || (info.mode & 0o022) !== 0) {
      throw new Error(`untrusted Ask Claw launcher parent: ${path}`);
    }
  }
  const info = lstatSync(ASK_CLAW_LAUNCHER);
  if (
    info.isSymbolicLink() ||
    !info.isFile() ||
    info.uid !== 0 ||
    (info.mode & 0o111) === 0 ||
    (info.mode & 0o022) !== 0
  ) {
    throw new Error("untrusted Ask Claw launcher");
  }
}

/**
 * Build the {@link GuiContext} for the current GUI launch.
 *
 * `appId` is read from `COS_APP_ID` (set by the kernel when it spawns
 * the GUI). `files` defaults to the launcher's file arguments, decoded
 * from `COS_ARGS_JSON` when not supplied explicitly.
 */
export function context(files?: string[]): GuiContext {
  const appId = process.env.COS_APP_ID || "unknown";
  const resolved = files ?? filesFromEnv();
  return new GuiContext(appId, [...resolved]);
}

function filesFromEnv(): string[] {
  const raw = process.env.COS_ARGS_JSON;
  if (!raw) return [];
  try {
    const parsed = JSON.parse(raw);
    if (Array.isArray(parsed)) return parsed.map((x) => String(x));
  } catch {
    return [];
  }
  return [];
}
