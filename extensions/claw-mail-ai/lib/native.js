// lib/native.js — Native Messaging port wrapper.
//
// Maintains a single long-lived port to the host named at connect() time.
// Each `call(verb, args)` resolves to either { ok:true, result } from the
// host, or { ok:false, error } on any failure (host disconnected, timeout,
// host crashed). The port auto-reconnects with capped exponential backoff.
//
// This module is loaded into the background event page via the
// `background.scripts` manifest list (it runs *before* background.js)
// and intentionally exposes a single namespace `ClawNative`.

(function () {
  "use strict";

  const RECONNECT_BACKOFF_MS = [500, 1000, 2000, 5000, 10000, 30000];
  const REQUEST_TIMEOUT_MS = 90_000;
  // Connection is "stable" — and the backoff index reset — only after
  // STABLE_MS of uptime OR after the first successful response. Without
  // this guard, a host that accepts the handshake then dies immediately
  // would cause a retry storm at the 500 ms floor.
  const STABLE_MS = 5_000;

  const pending = new Map();           // id → { resolve, reject, timer }
  let port = null;
  let reconnectIdx = 0;
  let hostName = null;
  let listeners = new Set();           // status listeners: ({status,error?}) ⇒ void
  let lastStatus = { status: "idle" };
  let stableTimer = null;
  let seenRoundTrip = false;

  function uuid() {
    // Service worker-safe; crypto.randomUUID is widely available.
    if (typeof crypto !== "undefined" && crypto.randomUUID) return crypto.randomUUID();
    return "id-" + Math.random().toString(16).slice(2) + "-" + Date.now();
  }

  function fireStatus(s) {
    lastStatus = s;
    for (const fn of listeners) {
      try { fn(s); } catch (_) {}
    }
  }

  function connect(name) {
    hostName = name;
    try {
      port = browser.runtime.connectNative(name);
    } catch (e) {
      console.warn("[claw-mail-ai] connectNative threw:", e);
      fireStatus({ status: "error", error: String(e) });
      scheduleReconnect();
      return;
    }
    // Do NOT reset reconnectIdx synchronously here — a host that
    // accepts the handshake then drops us immediately would otherwise
    // be hammered at the shortest backoff. Reset only after STABLE_MS
    // or after a successful round-trip (see onMessage below).
    seenRoundTrip = false;
    if (stableTimer) clearTimeout(stableTimer);
    stableTimer = setTimeout(() => {
      if (port) reconnectIdx = 0;
      stableTimer = null;
    }, STABLE_MS);
    fireStatus({ status: "connected" });

    port.onMessage.addListener(onMessage);
    port.onDisconnect.addListener(() => {
      const err = browser.runtime.lastError;
      const msg = err?.message || "host disconnected";
      console.warn("[claw-mail-ai] NM port disconnected:", msg);
      port = null;
      if (stableTimer) { clearTimeout(stableTimer); stableTimer = null; }
      // Fail every in-flight request.
      for (const [id, entry] of pending.entries()) {
        clearTimeout(entry.timer);
        entry.resolve({ ok: false, error: msg });
        pending.delete(id);
      }
      fireStatus({ status: "error", error: msg });
      scheduleReconnect();
    });
  }

  function scheduleReconnect() {
    if (!hostName) return;
    const delay = RECONNECT_BACKOFF_MS[
      Math.min(reconnectIdx, RECONNECT_BACKOFF_MS.length - 1)
    ];
    reconnectIdx++;
    setTimeout(() => connect(hostName), delay);
  }

  function onMessage(msg) {
    if (!msg || typeof msg !== "object") return;
    const id = msg.id;
    if (!id) return;
    const entry = pending.get(id);
    if (!entry) return;
    clearTimeout(entry.timer);
    pending.delete(id);
    // First completed round-trip ⇒ channel is healthy; reset the
    // reconnect backoff so the *next* disconnect starts at the
    // shortest delay again.
    if (!seenRoundTrip) {
      seenRoundTrip = true;
      reconnectIdx = 0;
    }
    entry.resolve(msg);
  }

  function call(verb, args) {
    return new Promise((resolve) => {
      if (!port) {
        // Try opening the port lazily — useful if popup arrives before
        // the background script's eager connect() has succeeded.
        if (hostName) connect(hostName);
        if (!port) {
          resolve({ ok: false, error: "native host unavailable" });
          return;
        }
      }
      const id = uuid();
      const timer = setTimeout(() => {
        pending.delete(id);
        resolve({ ok: false, error: "request timed out" });
      }, REQUEST_TIMEOUT_MS);
      pending.set(id, { resolve, timer });

      try {
        port.postMessage({ id, verb, args: args || {} });
      } catch (e) {
        clearTimeout(timer);
        pending.delete(id);
        resolve({ ok: false, error: String(e) });
      }
    });
  }

  function onStatus(fn) {
    listeners.add(fn);
    // Fire latest immediately so callers don't need a separate getter.
    try { fn(lastStatus); } catch (_) {}
    return () => listeners.delete(fn);
  }

  function getStatus() { return lastStatus; }

  self.ClawNative = { connect, call, onStatus, getStatus };
})();
