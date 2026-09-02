/**
 * Hash-based routing. We can't use HTML5 history because the bundle is
 * served from a single axum route — every refresh would 404. Hash
 * routing keeps everything inside the same `/index.html` response and
 * still gives us shareable URLs.
 *
 * Routes:
 *   #/                       → chat (default)
 *   #/chat                   → chat
 *   #/tasks                  → tasks
 *   #/approvals              → approvals
 *   #/inbox                  → notification inbox
 *   #/events                 → raw context-event diagnostics
 *   #/notifications          → legacy alias for inbox
 *   #/system                 → sysinfo
 *   #/settings               → settings overview (text model)
 *   #/settings/text          → settings :: text modality
 *   #/settings/embed         → settings :: embed
 *   #/settings/tts           → settings :: tts
 *   #/settings/stt           → settings :: stt
 *   #/settings/imagegen      → settings :: imagegen
 *   #/settings/about         → settings :: about
 */

import { useSyncExternalStore } from "react";

function read(): string {
  if (typeof window === "undefined") return "/";
  const h = window.location.hash || "#/";
  return h.startsWith("#") ? h.slice(1) : h;
}

const listeners = new Set<() => void>();
if (typeof window !== "undefined") {
  window.addEventListener("hashchange", () => {
    for (const l of listeners) l();
  });
}

function subscribe(cb: () => void) {
  listeners.add(cb);
  return () => listeners.delete(cb);
}

export function useRoute(): string {
  return useSyncExternalStore(subscribe, read, () => "/");
}

export function navigate(path: string) {
  if (!path.startsWith("/")) path = "/" + path;
  if (window.location.hash !== "#" + path) {
    window.location.hash = "#" + path;
  }
}

export function isActive(prefix: string, current: string): boolean {
  if (prefix === "/" || prefix === "/chat") {
    return current === "/" || current === "/chat" || current === "";
  }
  return current === prefix || current.startsWith(prefix + "/");
}
