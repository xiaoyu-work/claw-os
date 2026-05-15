// Claw Mail AI — background event page.
//
// One long-lived port to the Native Messaging host `os.claw.mail_ai`,
// which Thunderbird spawns when we call `connectNative`. The port stays
// open for the lifetime of this event page; replies are routed back to
// whichever popup / content script issued the request via a per-id
// promise table maintained in `lib/native.js`.
//
// Wire format on the port (both directions):
//   { id: "<uuid>", verb: "<verb>", args: {...} }
//   { id: "<uuid>", ok: true,  result: {...} }
//   { id: "<uuid>", ok: false, error: "...", detail?: {...} }
//
// Surfaces this background script wires up:
//   - browser.action                (main toolbar): opens the Assistant space
//   - browser.messageDisplayAction  (read window):  Summarize popup
//   - browser.composeAction         (compose):      Smart Reply / Smart Compose
//   - browser.spaces                (left rail):    Claw AI Assistant
//   - browser.menus                 (context):      Summarize / Translate / Smart Reply
//   - browser.messages.onNewMailReceived           : optional auto-triage
//   - browser.runtime.onMessage                    : popup → background bridge

importScripts("lib/native.js");

const ASSISTANT_SPACE_NAME = "claw_assistant";
const ASSISTANT_PAGE = browser.runtime.getURL("ui/spaces/assistant.html");

const DEFAULT_SETTINGS = {
  features: {
    summarize: true,
    smartReply: true,
    smartCompose: true,
    translate: true,
    triage: false,            // off by default — auto-tagging touches user data
    assistant: true,
  },
  triage: {
    autoTag: true,
    autoMoveImportant: false,
    autoMoveNewsletter: false,
    tagPrefix: "claw/",
  },
  compose: {
    defaultStyle: "formal",   // formal | casual | short
  },
  translate: {
    defaultTarget: "English",
  },
};

// ---------------------------------------------------------------------------
// Settings (browser.storage.local) helpers
// ---------------------------------------------------------------------------

async function getSettings() {
  const stored = await browser.storage.local.get("settings");
  return { ...DEFAULT_SETTINGS, ...(stored.settings || {}),
    features: { ...DEFAULT_SETTINGS.features, ...(stored.settings?.features || {}) },
    triage:   { ...DEFAULT_SETTINGS.triage,   ...(stored.settings?.triage   || {}) },
    compose:  { ...DEFAULT_SETTINGS.compose,  ...(stored.settings?.compose  || {}) },
    translate:{ ...DEFAULT_SETTINGS.translate,...(stored.settings?.translate|| {}) },
  };
}

async function setSettings(patch) {
  const current = await getSettings();
  const merged = {
    ...current,
    ...patch,
    features:  { ...current.features,  ...(patch.features  || {}) },
    triage:    { ...current.triage,    ...(patch.triage    || {}) },
    compose:   { ...current.compose,   ...(patch.compose   || {}) },
    translate: { ...current.translate, ...(patch.translate || {}) },
  };
  await browser.storage.local.set({ settings: merged });
  return merged;
}

// ---------------------------------------------------------------------------
// Bootstrap
// ---------------------------------------------------------------------------

browser.runtime.onInstalled.addListener(async () => {
  // Seed defaults on first install (don't clobber existing user pref edits).
  const stored = await browser.storage.local.get("settings");
  if (!stored.settings) {
    await browser.storage.local.set({ settings: DEFAULT_SETTINGS });
  }
});

// Open the long-lived NM port as early as possible.
ClawNative.connect("os.claw.mail_ai");

// Create the Spaces entry (TB 115+). The Spaces API is idempotent on
// the name — calling create with the same name twice throws, so we
// guard with a query first.
async function ensureAssistantSpace() {
  try {
    const list = await browser.spaces.query({ name: ASSISTANT_SPACE_NAME });
    if (list && list.length) return list[0];
    return await browser.spaces.create(ASSISTANT_SPACE_NAME, ASSISTANT_PAGE, {
      title: browser.i18n.getMessage("assistant_title") || "Claw AI",
      defaultIcons: { 16: browser.runtime.getURL("icons/icon.svg") },
      badgeBackgroundColor: "#10b981",
    });
  } catch (e) {
    console.warn("[claw-mail-ai] spaces.create failed:", e);
    return null;
  }
}

ensureAssistantSpace();

// ---------------------------------------------------------------------------
// Toolbar action — opens the assistant tab.
// ---------------------------------------------------------------------------

browser.action.onClicked.addListener(async () => {
  const settings = await getSettings();
  if (!settings.features.assistant) {
    notify(browser.i18n.getMessage("assistant_disabled") || "Assistant disabled in options");
    return;
  }
  const space = await ensureAssistantSpace();
  if (space) {
    try { await browser.spaces.open(space.id); } catch (e) { console.warn(e); }
  } else {
    // Fallback: open a regular tab.
    browser.tabs.create({ url: ASSISTANT_PAGE });
  }
});

// ---------------------------------------------------------------------------
// Context menus — summarise / translate / smart reply
// ---------------------------------------------------------------------------

const MENU_IDS = {
  summarize: "claw_mail_ai_summarize",
  translate: "claw_mail_ai_translate",
  reply: "claw_mail_ai_smart_reply",
};

async function rebuildMenus() {
  try { await browser.menus.removeAll(); } catch (_) {}

  const s = await getSettings();
  if (s.features.summarize) {
    browser.menus.create({
      id: MENU_IDS.summarize,
      title: browser.i18n.getMessage("ctx_summarize") || "Summarize with Claw AI",
      contexts: ["message_list", "message_display_action_menu"],
    });
  }
  if (s.features.translate) {
    browser.menus.create({
      id: MENU_IDS.translate,
      title: browser.i18n.getMessage("ctx_translate") || "Translate with Claw AI",
      contexts: ["message_list", "message_display_action_menu", "selection"],
    });
  }
  if (s.features.smartReply) {
    browser.menus.create({
      id: MENU_IDS.reply,
      title: browser.i18n.getMessage("ctx_smart_reply") || "Smart Reply with Claw AI",
      contexts: ["message_list", "message_display_action_menu"],
    });
  }
}
rebuildMenus();

browser.storage.onChanged.addListener((_changes, area) => {
  if (area === "local") rebuildMenus();
});

browser.menus.onClicked.addListener(async (info, tab) => {
  switch (info.menuItemId) {
    case MENU_IDS.summarize:
      return openPopupForSelection("summarize", info, tab);
    case MENU_IDS.translate:
      return openPopupForSelection("translate", info, tab);
    case MENU_IDS.reply:
      return openPopupForSelection("smart_reply", info, tab);
  }
});

async function openPopupForSelection(kind, info, _tab) {
  // For message_list contexts, info.selectedMessages.messages holds the
  // current selection. For selection context, info.selectionText has the
  // highlighted text.
  const url = browser.runtime.getURL(`ui/${
    kind === "summarize" ? "summarize/summarize.html" :
    kind === "translate" ? "translate/translate.html" :
    "compose/composeAction.html"
  }?from=menu`);
  // Persist the seed payload so the popup can pick it up.
  await browser.storage.local.set({
    pending: {
      kind,
      messageIds: (info.selectedMessages?.messages || []).map(m => m.id),
      selectionText: info.selectionText || "",
      ts: Date.now(),
    },
  });
  browser.windows.create({ url, type: "popup", width: 480, height: 600 });
}

// ---------------------------------------------------------------------------
// Auto-triage on new mail
// ---------------------------------------------------------------------------

browser.messages.onNewMailReceived.addListener(async (folder, messageList) => {
  const s = await getSettings();
  if (!s.features.triage || !s.triage.autoTag) return;

  // Skip drafts/sent/templates folders — only triage inbound mail.
  const skipTypes = new Set(["drafts", "sent", "templates", "outbox", "junk", "trash"]);
  if (skipTypes.has(folder?.type)) return;

  for (const header of (messageList?.messages || [])) {
    try {
      await triageMessage(header, s);
    } catch (e) {
      console.warn("[claw-mail-ai] triage failed for", header.id, e);
    }
  }
});

async function triageMessage(header, settings) {
  const args = {
    subject: header.subject || "",
    from: (header.author || "").toString(),
    snippet: "",
    has_attachments: false,
  };

  // Pull a small body snippet — we cap at ~1000 chars in the verb anyway.
  try {
    const full = await browser.messages.getFull(header.id);
    args.snippet = extractPlainText(full).slice(0, 800);
    args.has_attachments = hasRealAttachments(full);
  } catch (_) { /* skip body fetch failures */ }

  const result = await ClawNative.call("triage", args);
  if (!result || !result.ok) return;
  const r = result.result;

  if (settings.triage.autoTag) {
    const tag = `${settings.triage.tagPrefix}${r.category}`;
    await ensureTag(tag, categoryColor(r.category));
    await browser.messages.update(header.id, {
      tags: Array.from(new Set([...(header.tags || []), tag])),
    });
  }

  // Optional auto-move (off by default).
  if (settings.triage.autoMoveImportant && r.priority === "high") {
    /* future: move to a specific folder configured in options */
  }
}

function categoryColor(category) {
  return {
    important:    "#ef4444",
    personal:     "#10b981",
    work:         "#3b82f6",
    newsletter:   "#a855f7",
    promo:        "#f59e0b",
    receipt:      "#64748b",
    calendar:     "#06b6d4",
    notification: "#94a3b8",
    other:        "#9ca3af",
  }[category] || "#9ca3af";
}

async function ensureTag(key, color) {
  try {
    const tags = await browser.messages.tags.list();
    if (tags.some(t => t.key === key)) return;
    await browser.messages.tags.create(key, key.split("/").pop() || key, color);
  } catch (e) {
    // Older TB versions exposed this API differently — degrade silently.
    console.warn("[claw-mail-ai] could not ensure tag", key, e);
  }
}

// ---------------------------------------------------------------------------
// Plain-text body extraction (very basic — good enough for prompts)
// ---------------------------------------------------------------------------

function extractPlainText(part) {
  if (!part) return "";
  const queue = [part];
  const plain = [];
  const html = [];
  while (queue.length) {
    const p = queue.shift();
    if (!p) continue;
    if (Array.isArray(p.parts)) queue.push(...p.parts);
    if (typeof p.body !== "string") continue;
    if (p.contentType?.startsWith("text/plain")) plain.push(p.body);
    else if (p.contentType?.startsWith("text/html")) html.push(p.body);
  }
  if (plain.length) return plain.join("\n\n");
  if (html.length) return stripHtml(html.join("\n\n"));
  return "";
}

function stripHtml(s) {
  return s
    .replace(/<style[\s\S]*?<\/style>/gi, " ")
    .replace(/<script[\s\S]*?<\/script>/gi, " ")
    .replace(/<br\s*\/?>/gi, "\n")
    .replace(/<\/p>/gi, "\n\n")
    .replace(/<[^>]+>/g, " ")
    .replace(/&nbsp;/g, " ")
    .replace(/&amp;/g, "&")
    .replace(/&lt;/g, "<")
    .replace(/&gt;/g, ">")
    .replace(/&quot;/g, '"')
    .replace(/&#39;/g, "'")
    .replace(/[ \t]+\n/g, "\n")
    .replace(/\n{3,}/g, "\n\n")
    .trim();
}

function hasRealAttachments(part) {
  if (!part) return false;
  const queue = [part];
  while (queue.length) {
    const p = queue.shift();
    if (!p) continue;
    if (Array.isArray(p.parts)) queue.push(...p.parts);
    if (p.name && p.contentType && !p.contentType.startsWith("multipart/")) return true;
  }
  return false;
}

// ---------------------------------------------------------------------------
// Notifications
// ---------------------------------------------------------------------------

function notify(message, title) {
  try {
    browser.notifications.create({
      type: "basic",
      iconUrl: browser.runtime.getURL("icons/icon.svg"),
      title: title || "Claw Mail AI",
      message,
    });
  } catch (_) { /* notifications may be disabled */ }
}

// ---------------------------------------------------------------------------
// Popup / content-script ↔ background bridge
// ---------------------------------------------------------------------------

browser.runtime.onMessage.addListener((msg, _sender) => {
  if (!msg || typeof msg !== "object") return;

  switch (msg.kind) {

    case "ai":
      // { kind: "ai", verb: "<verb>", args: { … } } → forwards through NM
      return ClawNative.call(msg.verb, msg.args || {});

    case "getSettings":
      return getSettings();

    case "setSettings":
      return setSettings(msg.patch || {});

    case "getMessageBody":
      // { kind:"getMessageBody", messageId } → { plain, html, subject, from, to }
      return browser.messages.getFull(msg.messageId).then(full => {
        const headers = full?.headers || {};
        return {
          plain: extractPlainText(full),
          subject: (headers.subject?.[0]) || "",
          from: (headers.from?.[0]) || "",
          to: (headers.to?.[0]) || "",
          date: (headers.date?.[0]) || "",
        };
      });

    case "getThreadBodies":
      // { kind:"getThreadBodies", messageId } → bundled thread text
      return getThreadText(msg.messageId);

    case "getComposeDetails":
      return browser.compose.getComposeDetails(msg.tabId);

    case "setComposeBody":
      // { kind:"setComposeBody", tabId, body, subject? }
      return browser.compose.setComposeDetails(msg.tabId, {
        body: msg.body,
        ...(msg.subject ? { subject: msg.subject } : {}),
        isPlainText: false,
      });

    case "openAssistant":
      return ensureAssistantSpace().then(s =>
        s ? browser.spaces.open(s.id) : browser.tabs.create({ url: ASSISTANT_PAGE })
      );

    case "getPending":
      return browser.storage.local.get("pending").then(r => r.pending || null);

    case "clearPending":
      return browser.storage.local.remove("pending");

    case "listRecentMessages":
      return listRecentMessages(msg.limit || 30).then(messages => ({ ok: true, messages }));

    case "openComposePopup":
      // The compose-action popup must be opened by the user from the
      // compose toolbar — extensions can't programmatically pop it
      // open in Thunderbird. Fall back to a notification so the user
      // knows the shortcut was received.
      notify(browser.i18n.getMessage("hint_open_compose_popup")
        || "Click the Claw AI button in the compose toolbar to open Smart Reply.");
      return Promise.resolve({ ok: true });

    case "nativeStatus":
      return Promise.resolve({
        ok: true,
        status: (ClawNative.getStatus && ClawNative.getStatus().status) || "unknown",
        hostName: "os.claw.mail_ai",
      });

    case "refreshSpaces":
      // Settings change may have toggled assistant visibility. We can
      // only add — there's no API to remove a Space once created, so
      // we just ensure it exists when enabled.
      getSettings().then(s => { if (s.features.assistant) ensureAssistantSpace(); });
      return Promise.resolve({ ok: true });

    default:
      return Promise.resolve({ error: `unknown bridge kind: ${msg.kind}` });
  }
});

async function getThreadText(messageId) {
  try {
    const header = await browser.messages.get(messageId);
    let thread;
    try {
      thread = await browser.messages.getRelated(messageId, { type: "thread" });
    } catch (_) {
      thread = { messages: [header] };
    }
    const messages = (thread?.messages || [header]).sort(
      (a, b) => new Date(a.date) - new Date(b.date)
    );

    const blocks = [];
    for (const m of messages) {
      try {
        const full = await browser.messages.getFull(m.id);
        const body = extractPlainText(full);
        blocks.push(
          `From: ${m.author || "(unknown)"}\n` +
          `Date: ${m.date?.toISOString?.() || m.date || ""}\n` +
          `Subject: ${m.subject || ""}\n\n${body}`
        );
      } catch (_) { /* skip */ }
    }
    return {
      subject: header.subject || "",
      lastFrom: messages[messages.length - 1]?.author || "",
      text: blocks.join("\n\n--- next message ---\n\n"),
    };
  } catch (e) {
    return { error: String(e) };
  }
}

async function listRecentMessages(limit) {
  try {
    const accounts = await browser.accounts.list();
    const inboxes = [];
    for (const acct of accounts) {
      for (const folder of (acct.folders || [])) {
        if (folder.type === "inbox") inboxes.push(folder);
      }
    }
    const out = [];
    for (const folder of inboxes) {
      let page;
      try {
        page = await browser.messages.list(folder);
      } catch (_) { continue; }
      for (const m of (page?.messages || [])) {
        out.push({
          id: m.id,
          from: m.author || "",
          subject: m.subject || "",
          date: m.date?.toISOString?.() || String(m.date || ""),
        });
        if (out.length >= limit * 3) break;
      }
      if (out.length >= limit * 3) break;
    }
    out.sort((a, b) => (b.date || "").localeCompare(a.date || ""));
    return out.slice(0, limit);
  } catch (e) {
    return { error: String(e) };
  }
}

// ---------------------------------------------------------------------------
// Expose helpers globally for popups that load lib/native.js (via
// browser.runtime.getBackgroundPage is not available in MV3; popups
// use runtime.sendMessage instead).
// ---------------------------------------------------------------------------
self.ClawApp = {
  getSettings,
  setSettings,
  extractPlainText,
};
