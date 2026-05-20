/**
 * Tiny theme manager. cos UI defaults to dark (the open-agents look),
 * but lets the user switch to light or "system" (follows the OS prefs
 * via the `prefers-color-scheme` media query). Persisted in
 * localStorage.
 *
 * The actual switching is done by toggling the `dark` class on the
 * <html> element — Tailwind v4's `@variant dark (&:where(.dark, .dark *))`
 * is wired in `app/globals.css`.
 */

import { useEffect, useSyncExternalStore } from "react";

export type Theme = "dark" | "light" | "system";
const KEY = "cos.theme";

function read(): Theme {
  if (typeof window === "undefined") return "dark";
  try {
    const v = localStorage.getItem(KEY);
    if (v === "dark" || v === "light" || v === "system") return v;
  } catch {}
  return "dark";
}

function effective(theme: Theme): "dark" | "light" {
  if (theme !== "system") return theme;
  if (typeof window === "undefined") return "dark";
  return window.matchMedia("(prefers-color-scheme: dark)").matches ? "dark" : "light";
}

function apply(theme: Theme) {
  const root = document.documentElement;
  const eff = effective(theme);
  root.classList.toggle("dark", eff === "dark");
  // Mirror the resolved theme on a data attribute too so any CSS or
  // hand-written component that prefers `[data-theme]` selectors stays
  // in sync. Belt-and-suspenders alongside the Tailwind `.dark` class.
  root.setAttribute("data-theme", eff);
  root.style.colorScheme = eff;
}

const listeners = new Set<() => void>();

function subscribe(cb: () => void) {
  listeners.add(cb);
  const mq = window.matchMedia("(prefers-color-scheme: dark)");
  const onMq = () => {
    if (read() === "system") apply("system");
    cb();
  };
  mq.addEventListener("change", onMq);
  return () => {
    listeners.delete(cb);
    mq.removeEventListener("change", onMq);
  };
}

export function useTheme(): Theme {
  return useSyncExternalStore(subscribe, read, () => "dark");
}

export function setTheme(theme: Theme) {
  try {
    localStorage.setItem(KEY, theme);
  } catch {}
  apply(theme);
  for (const l of listeners) l();
}

export function useApplyTheme() {
  const theme = useTheme();
  useEffect(() => {
    apply(theme);
  }, [theme]);
}
