// lib/ui.js — small helpers shared by every popup / page.
// Loaded as a classic <script src=…> so it just attaches to `window`.

(function () {
  "use strict";

  // Send a verb call through the background → NM bridge.
  // Resolves to the NM envelope: { ok, result|error, detail? }
  async function aiCall(verb, args) {
    return await browser.runtime.sendMessage({
      kind: "ai",
      verb,
      args: args || {},
    });
  }

  async function getSettings() {
    return await browser.runtime.sendMessage({ kind: "getSettings" });
  }

  async function setSettings(patch) {
    return await browser.runtime.sendMessage({ kind: "setSettings", patch });
  }

  function showBusy(el, label) {
    if (!el) return;
    el.classList.add("busy");
    el.innerHTML = "";
    const dot = document.createElement("span");
    dot.className = "spinner";
    el.appendChild(dot);
    const txt = document.createElement("span");
    txt.className = "busy-label";
    txt.textContent = label || browser.i18n.getMessage("busy") || "Working…";
    el.appendChild(txt);
  }

  function clearBusy(el) {
    if (!el) return;
    el.classList.remove("busy");
    el.innerHTML = "";
  }

  function showError(el, msg, detail) {
    if (!el) return;
    el.innerHTML = "";
    const wrap = document.createElement("div");
    wrap.className = "error";
    const h = document.createElement("strong");
    h.textContent = browser.i18n.getMessage("error") || "Error";
    wrap.appendChild(h);
    const body = document.createElement("p");
    body.textContent = msg || "Unknown error";
    wrap.appendChild(body);
    if (detail) {
      const pre = document.createElement("pre");
      pre.textContent = typeof detail === "string" ? detail : JSON.stringify(detail, null, 2);
      wrap.appendChild(pre);
    }
    el.appendChild(wrap);
  }

  function el(tag, attrs, children) {
    const node = document.createElement(tag);
    if (attrs) {
      for (const [k, v] of Object.entries(attrs)) {
        if (k === "class") node.className = v;
        else if (k === "text") node.textContent = v;
        else if (k.startsWith("on") && typeof v === "function") {
          node.addEventListener(k.slice(2).toLowerCase(), v);
        } else {
          node.setAttribute(k, v);
        }
      }
    }
    for (const c of (children || [])) {
      if (c == null) continue;
      node.appendChild(typeof c === "string" ? document.createTextNode(c) : c);
    }
    return node;
  }

  function localiseDom(root) {
    // Replace any [data-i18n] textContent with the corresponding _locales
    // message. Lets us write static HTML without hard-coded English.
    const scope = root || document;
    for (const node of scope.querySelectorAll("[data-i18n]")) {
      const key = node.getAttribute("data-i18n");
      const msg = browser.i18n.getMessage(key);
      if (msg) node.textContent = msg;
    }
    for (const node of scope.querySelectorAll("[data-i18n-placeholder]")) {
      const key = node.getAttribute("data-i18n-placeholder");
      const msg = browser.i18n.getMessage(key);
      if (msg) node.setAttribute("placeholder", msg);
    }
    for (const node of scope.querySelectorAll("[data-i18n-title]")) {
      const key = node.getAttribute("data-i18n-title");
      const msg = browser.i18n.getMessage(key);
      if (msg) node.title = msg;
    }
  }

  function copyToClipboard(text) {
    return navigator.clipboard.writeText(text);
  }

  // Wait for `runtime.onMessage` from background; useful in popups that
  // want to receive push-side updates from triage / assistant.
  function onBackgroundMessage(kind, fn) {
    browser.runtime.onMessage.addListener((msg) => {
      if (msg && msg.kind === kind) fn(msg);
    });
  }

  window.ClawUI = {
    aiCall,
    getSettings,
    setSettings,
    showBusy,
    clearBusy,
    showError,
    el,
    localiseDom,
    copyToClipboard,
    onBackgroundMessage,
  };
})();
