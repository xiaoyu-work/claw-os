// @claw-os/sdk — public App, AI, tool, and GUI SDK for Claw OS, Node edition.
//
// This package is the AI-facing surface a third-party Linux developer
// uses when their Node app touches the kernel's AI features or runs as
// a desktop GUI. Every call is a thin client over wire protocol v1: it
// shells out to the `cos` binary, which enforces capabilities, budget,
// safety, and audit. See ../README.md and ../wire/v1/README.md.
//
//   import { ai, tools, gui, mcp } from "@claw-os/sdk";
//
//   export function run(command: string, args: Record<string, unknown>) {
//     if (gui.isGuiLaunch()) {
//       return startWindow(gui.context());           // desktop surface
//     }
//     const res = ai.chat(String(args.body), { origin: "external-content" });
//     return { summary: res.text, usage: res.usage };
//   }
//
// Capability gating helpers used by claw-os's own bundled apps are
// deliberately NOT here — they live in the internal cos-runtime tree.

export * as ai from "./ai";
export * as tools from "./tools";
export * as gui from "./gui";
export * as mcp from "./mcp";
export * as transport from "./transport";
export * from "./generated";

/** Wire protocol version this SDK targets. */
export const WIRE_VERSION = 1 as const;
