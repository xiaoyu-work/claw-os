// lib/messages.js — popup-side helpers for fetching Thunderbird message
// content via the background script (popups can't talk to the messages
// API directly in MV3 without `messagesRead` host_permissions
// indirection — we just round-trip through runtime.onMessage).
//
// All exports attach to the global `ClawMessages` namespace.

(function () {
  "use strict";

  async function getBody(messageId) {
    return await browser.runtime.sendMessage({
      kind: "getMessageBody",
      messageId,
    });
  }

  async function getThread(messageId) {
    return await browser.runtime.sendMessage({
      kind: "getThreadBodies",
      messageId,
    });
  }

  async function getDisplayedMessage() {
    // First try the standard read-pane API.
    try {
      const msg = await browser.messageDisplay.getDisplayedMessage();
      if (msg && typeof msg.id === "number") return msg;
    } catch (_) {}
    // Fallback for the "stand-alone message window" case: pick the
    // currently active message from any message-display tab.
    try {
      const tabs = await browser.tabs.query({ active: true });
      for (const t of tabs) {
        try {
          const m = await browser.messageDisplay.getDisplayedMessage(t.id);
          if (m && typeof m.id === "number") return m;
        } catch (_) {}
      }
    } catch (_) {}
    return null;
  }

  async function getComposeTab() {
    // The compose popup lives inside the compose window — return its tab.
    try {
      const tabs = await browser.tabs.query({ active: true, currentWindow: true });
      for (const t of tabs) {
        if (t.type === "messageCompose") return t;
      }
    } catch (_) {}
    return null;
  }

  async function getComposeDetails(tabId) {
    return await browser.runtime.sendMessage({
      kind: "getComposeDetails",
      tabId,
    });
  }

  async function setComposeBody(tabId, body, subject) {
    return await browser.runtime.sendMessage({
      kind: "setComposeBody",
      tabId,
      body,
      subject,
    });
  }

  function bodyToPlain(body, isPlain) {
    if (!body) return "";
    if (isPlain) return body;
    // Compose details body is HTML when isPlainText=false. Use DOMParser
    // for proper entity decoding (named + numeric + hex) — the previous
    // regex-based stripper missed everything except a handful of common
    // entities and would leave &mdash;, &rsquo;, &#8211; etc. in output.
    try {
      const doc = new DOMParser().parseFromString(body, "text/html");
      for (const el of doc.querySelectorAll("script,style,noscript,template")) {
        el.remove();
      }
      for (const br of doc.querySelectorAll("br")) {
        br.replaceWith("\n");
      }
      for (const p of doc.querySelectorAll("p,div,li,tr")) {
        p.appendChild(doc.createTextNode("\n"));
      }
      const text = (doc.body?.textContent || "").replace(/\r\n?/g, "\n");
      return text
        .replace(/[ \t]+\n/g, "\n")
        .replace(/\n{3,}/g, "\n\n")
        .trim();
    } catch (_) {
      return body.replace(/<[^>]+>/g, " ").replace(/\s+/g, " ").trim();
    }
  }

  function plainToHtml(text) {
    const escaped = (text || "")
      .replace(/&/g, "&amp;")
      .replace(/</g, "&lt;")
      .replace(/>/g, "&gt;");
    return escaped
      .split(/\n{2,}/)
      .map(p => `<p>${p.replace(/\n/g, "<br>")}</p>`)
      .join("");
  }

  window.ClawMessages = {
    getBody,
    getThread,
    getDisplayedMessage,
    getComposeTab,
    getComposeDetails,
    setComposeBody,
    bodyToPlain,
    plainToHtml,
  };
})();
