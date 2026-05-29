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

import * as ai from "./ai";
import * as tools from "./tools";

/** Command value the bridge passes (and the default `desktop.exec`)
 * when an app is launched as a GUI. */
export const GUI_COMMAND = "--gui";

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
   * Summon the system "Ask Claw" agent overlay — the same
   * `cos-agent-ui --overlay` window the global hotkey raises. Pass
   * `hint` to ground the agent's first response in the app's current
   * state without polluting the visible chat transcript.
   *
   * The overlay is detached: it outlives this call and is not tied to
   * the app's stdio or event loop. Resolves once the child has been
   * spawned; rejects if the overlay binary is missing (e.g. a headless
   * box with no desktop shell).
   */
  openAgentOverlay(hint?: string): Promise<void> {
    const bin = process.env.COS_AGENT_UI_BIN || "cos-agent-ui";
    const argv = ["--overlay"];
    if (hint) argv.push("--context", hint);
    return new Promise<void>((resolve, reject) => {
      const child = spawn(bin, argv, {
        stdio: "ignore",
        detached: true,
      });
      child.once("error", (err) => reject(err));
      child.once("spawn", () => {
        child.unref();
        resolve();
      });
    });
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
