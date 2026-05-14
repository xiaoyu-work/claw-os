// Claw Agent — background service worker.
//
// Connects a long-lived Native Messaging port to com.clawos.browser, then
// translates requests coming from the host into chrome.tabs / chrome.scripting
// calls and forwards results back.  The port stays open for the lifetime of
// the SW, which keeps the SW awake (an active native port is treated as
// "in use" by Chromium, so the 30 s idle timeout does not kill us).
//
// Wire format (both directions on the port):
//   { id: "<uuid>", verb: "<verb>", args: {...} }
//   { id: "<uuid>", ok: true,  result: {...} }
//   { id: "<uuid>", ok: false, error: "..." }

const HOST_NAME = "com.clawos.browser";
const RECONNECT_BACKOFF_MS = [500, 1000, 2000, 5000, 10000];

let port = null;
let reconnectIdx = 0;
let state = "idle"; // idle | acting | waiting-approval | error
let acting_tab_id = null;

function setState(s, tabId = null) {
  state = s;
  acting_tab_id = tabId;
  const badge = { idle: "", acting: "•", "waiting-approval": "?", error: "!" }[s] ?? "";
  const colour = {
    idle: "#666",
    acting: "#10b981",
    "waiting-approval": "#f59e0b",
    error: "#ef4444",
  }[s] ?? "#666";
  try {
    chrome.action.setBadgeBackgroundColor({ color: colour });
    chrome.action.setBadgeText({ text: badge });
  } catch (_) {}
}

function connect() {
  try {
    port = chrome.runtime.connectNative(HOST_NAME);
  } catch (e) {
    console.warn("[claw-agent] connectNative threw:", e);
    scheduleReconnect();
    return;
  }
  reconnectIdx = 0;
  port.onMessage.addListener(onHostMessage);
  port.onDisconnect.addListener(() => {
    const err = chrome.runtime.lastError;
    console.warn("[claw-agent] host disconnected:", err && err.message);
    port = null;
    setState("error");
    scheduleReconnect();
  });
  setState("idle");
}

function scheduleReconnect() {
  const delay = RECONNECT_BACKOFF_MS[
    Math.min(reconnectIdx, RECONNECT_BACKOFF_MS.length - 1)
  ];
  reconnectIdx++;
  setTimeout(connect, delay);
}

async function onHostMessage(msg) {
  const id = msg && msg.id;
  const verb = msg && msg.verb;
  const args = (msg && msg.args) || {};
  if (!id || !verb) {
    reply(id || "no-id", false, null, "request missing id or verb");
    return;
  }
  setState("acting", typeof args.id === "number" ? args.id : null);
  try {
    const handler = HANDLERS[verb];
    if (!handler) throw new Error(`unknown verb: ${verb}`);
    const result = await handler(args);
    reply(id, true, result, null);
  } catch (e) {
    reply(id, false, null, e && e.message ? e.message : String(e));
  } finally {
    setState("idle");
  }
}

function reply(id, ok, result, error) {
  if (!port) return;
  const msg = { id, ok };
  if (ok) msg.result = result;
  else msg.error = error || "unknown error";
  try {
    port.postMessage(msg);
  } catch (e) {
    console.warn("[claw-agent] postMessage failed:", e);
  }
}

// ---------------------------------------------------------------------------
// Verb handlers
// ---------------------------------------------------------------------------

const HANDLERS = {
  "tabs.list": async () => {
    const tabs = await chrome.tabs.query({});
    return {
      tabs: tabs.map((t) => ({
        id: t.id,
        windowId: t.windowId,
        title: t.title || "",
        url: t.url || "",
        active: !!t.active,
        audible: !!t.audible,
        pinned: !!t.pinned,
      })),
    };
  },

  "tabs.info": async ({ id }) => {
    const tab = await chrome.tabs.get(id);
    let host = "";
    try {
      host = new URL(tab.url || "").hostname;
    } catch (_) {}
    return {
      id: tab.id,
      title: tab.title || "",
      url: tab.url || "",
      host,
      active: !!tab.active,
    };
  },

  "tabs.activate": async ({ id }) => {
    const tab = await chrome.tabs.get(id);
    await chrome.windows.update(tab.windowId, { focused: true });
    await chrome.tabs.update(id, { active: true });
    return { id };
  },

  "nav.go": async ({ id, url }) => {
    await chrome.tabs.update(id, { url });
    return { id, url };
  },

  "dom.query": async ({ id, selector }) => {
    return sendToContent(id, { kind: "query", selector });
  },

  "dom.click": async ({ id, ref }) => {
    return sendToContent(id, { kind: "click", ref });
  },

  "dom.fill": async ({ id, ref, value, allow_secret }) => {
    return sendToContent(id, {
      kind: "fill",
      ref,
      value,
      allow_secret: !!allow_secret,
    });
  },

  "page.snapshot": async ({ id, kind }) => {
    return sendToContent(id, { kind: "snapshot", snapshot_kind: kind || "ax" });
  },

  "page.screenshot": async ({ id }) => {
    const tab = await chrome.tabs.get(id);
    if (!tab.active) {
      await chrome.windows.update(tab.windowId, { focused: true });
      await chrome.tabs.update(id, { active: true });
      await sleep(150);
    }
    const dataUrl = await chrome.tabs.captureVisibleTab(tab.windowId, {
      format: "png",
    });
    const b64 = dataUrl.replace(/^data:image\/png;base64,/, "");
    return { png_base64: b64 };
  },

  "eval": async ({ id, expr }) => {
    const [{ result }] = await chrome.scripting.executeScript({
      target: { tabId: id },
      world: "MAIN",
      func: (code) => {
        try {
          // eslint-disable-next-line no-new-func
          const v = new Function(`return (${code})`)();
          return { ok: true, value: serialise(v) };
        } catch (e) {
          return { ok: false, error: String(e && e.message ? e.message : e) };
        }
        function serialise(v) {
          if (v === null || v === undefined) return v;
          const t = typeof v;
          if (t === "string" || t === "number" || t === "boolean") return v;
          try { return JSON.parse(JSON.stringify(v)); } catch (_) { return String(v); }
        }
      },
      args: [expr],
    });
    return result;
  },
};

async function sendToContent(tabId, message) {
  try {
    const response = await chrome.tabs.sendMessage(tabId, {
      claw_agent: true,
      ...message,
    });
    if (response && response.error) throw new Error(response.error);
    return response && response.result !== undefined ? response.result : response;
  } catch (e) {
    if (
      e &&
      typeof e.message === "string" &&
      e.message.includes("Could not establish connection")
    ) {
      throw new Error(
        "content script not ready in tab (page is restricted or still loading)"
      );
    }
    throw e;
  }
}

function sleep(ms) {
  return new Promise((r) => setTimeout(r, ms));
}

// ---------------------------------------------------------------------------
// Popup messaging
// ---------------------------------------------------------------------------

chrome.runtime.onMessage.addListener((msg, _sender, sendResponse) => {
  if (!msg || !msg.claw_agent_popup) return false;
  if (msg.claw_agent_popup === "status") {
    sendResponse({ state, tab: acting_tab_id });
    return false;
  }
  if (msg.claw_agent_popup === "stop") {
    // Disconnect the port; on reconnect, the SW will be back in `idle`.
    try { if (port) port.disconnect(); } catch (_) {}
    port = null;
    setState("idle");
    sendResponse({ ok: true });
    return false;
  }
  return false;
});

// ---------------------------------------------------------------------------
// Wake-up triggers
// ---------------------------------------------------------------------------

chrome.runtime.onStartup.addListener(() => {
  if (!port) connect();
});
chrome.runtime.onInstalled.addListener(() => {
  if (!port) connect();
});

// Service workers may be torn down and reawoken on events.  Connect eagerly
// at script evaluation so we re-open the port after a wake-up too.
connect();
