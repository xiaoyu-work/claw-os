// Compose-action popup — Smart Reply + Smart Compose.
//
// Lives inside the compose window (Thunderbird's "Write" window). The
// tab id we operate on is whatever compose tab is currently active.

const { aiCall, showBusy, clearBusy, showError, el, localiseDom } = window.ClawUI;
const { getComposeTab, getComposeDetails, setComposeBody, plainToHtml, bodyToPlain } = window.ClawMessages;

let composeTab = null;
let mode = "reply";          // "reply" | "compose"
let lastSuggestions = null;
let lastCompose = null;

document.addEventListener("DOMContentLoaded", async () => {
  localiseDom();
  composeTab = await getComposeTab();

  // Tab switcher.
  for (const tab of document.querySelectorAll(".tab")) {
    tab.addEventListener("click", () => switchMode(tab.dataset.mode));
  }

  document.getElementById("btn-suggest").addEventListener("click", runSmartReply);
  document.getElementById("btn-compose-generate").addEventListener("click", runSmartCompose);

  // Preload style preference.
  const settings = await ClawUI.getSettings();
  document.getElementById("compose-style").value = settings.compose.defaultStyle || "formal";

  // If the compose window is a reply (has originalMessage), default to
  // the reply tab and pre-load; otherwise default to compose.
  const details = composeTab ? await getComposeDetails(composeTab.id) : null;
  if (details && details.relatedMessageId != null) {
    switchMode("reply");
    runSmartReply();
  } else {
    switchMode("compose");
  }
});

function switchMode(m) {
  mode = m;
  for (const tab of document.querySelectorAll(".tab")) {
    tab.classList.toggle("active", tab.dataset.mode === m);
  }
  document.getElementById("panel-reply").style.display   = m === "reply"   ? "" : "none";
  document.getElementById("panel-compose").style.display = m === "compose" ? "" : "none";
}

// ---------------------------------------------------------------------------
// Smart Reply
// ---------------------------------------------------------------------------

async function runSmartReply() {
  if (!composeTab) {
    showError(document.getElementById("reply-out"),
      browser.i18n.getMessage("err_no_compose_tab") || "Not inside a compose window.");
    return;
  }

  const out  = document.getElementById("reply-out");
  const meta = document.getElementById("reply-meta");
  showBusy(out);
  meta.textContent = "";
  document.getElementById("reply-usage").textContent = "";

  const details = await getComposeDetails(composeTab.id);
  if (!details) {
    clearBusy(out);
    showError(out, "Could not read compose details.");
    return;
  }

  const intent = document.getElementById("reply-intent").value.trim();
  const lang = (browser.i18n.getUILanguage() || "en").replace("-", "_");

  // If the compose is a reply (has relatedMessageId), pull the thread.
  let threadText = "";
  let subject = details.subject || "";
  let lastFrom = "";
  if (details.relatedMessageId != null) {
    const t = await browser.runtime.sendMessage({
      kind: "getThreadBodies",
      messageId: details.relatedMessageId,
    });
    if (t && !t.error) {
      threadText = t.text;
      subject = t.subject || subject;
      lastFrom = t.lastFrom || "";
    }
  }
  if (!threadText) {
    // New compose — fall back to using the current draft as "the thread".
    threadText = bodyToPlain(details.body, details.isPlainText);
    if (!threadText.trim()) {
      clearBusy(out);
      showError(out, browser.i18n.getMessage("err_nothing_to_reply") ||
        "No thread to reply to. Use Smart Compose instead.");
      return;
    }
  }

  meta.textContent = subject ? `${lastFrom}  •  ${subject}` : (lastFrom || "");

  const res = await aiCall("smart_reply", {
    thread: threadText,
    subject,
    from: lastFrom,
    "my-intent": intent,
    lang,
  });

  clearBusy(out);
  if (!res || !res.ok) { showError(out, res?.error || "AI call failed", res?.detail); return; }
  lastSuggestions = res.result;
  renderSuggestions(out, res.result);
  renderUsage("reply-usage", res.result);
}

function renderSuggestions(out, r) {
  out.innerHTML = "";
  const s = r.suggestions || {};
  for (const style of ["formal", "casual", "short"]) {
    if (!s[style]) continue;
    const card = el("div", { class: "suggestion" }, [
      el("div", { class: "style-tag", text: browser.i18n.getMessage(`style_${style}`) || style }),
      el("div", { class: "body", text: s[style] }),
      el("div", { class: "actions" }, [
        el("button", { class: "ghost", onclick: () => copySuggestion(s[style]), text: browser.i18n.getMessage("action_copy") || "Copy" }),
        el("button", { class: "primary", onclick: () => applySuggestion(s[style]), text: browser.i18n.getMessage("action_insert") || "Insert" }),
      ]),
    ]);
    out.appendChild(card);
  }
  if (r.raw) {
    out.appendChild(el("div", { class: "card muted mono", text: r.raw }));
  }
}

async function applySuggestion(body) {
  if (!composeTab) return;
  await setComposeBody(composeTab.id, plainToHtml(body));
  flashBtnSuccess(document.querySelectorAll(".suggestion .primary")[0]);
}

async function copySuggestion(body) {
  try { await ClawUI.copyToClipboard(body); } catch (_) {}
}

// ---------------------------------------------------------------------------
// Smart Compose
// ---------------------------------------------------------------------------

async function runSmartCompose() {
  if (!composeTab) {
    showError(document.getElementById("compose-out"),
      browser.i18n.getMessage("err_no_compose_tab") || "Not inside a compose window.");
    return;
  }

  const out = document.getElementById("compose-out");
  showBusy(out);
  document.getElementById("compose-usage").textContent = "";

  const intent = document.getElementById("compose-intent").value.trim();
  if (!intent) {
    clearBusy(out);
    showError(out, browser.i18n.getMessage("err_no_intent") || "Tell me what you want to say.");
    return;
  }

  const details = await getComposeDetails(composeTab.id);
  const lang = (browser.i18n.getUILanguage() || "en").replace("-", "_");
  const style = document.getElementById("compose-style").value || "formal";
  await ClawUI.setSettings({ compose: { defaultStyle: style } });

  // First recipient — the address is a string in MV3 compose details.
  const to = (details?.to || [])[0] || "";
  const draft = bodyToPlain(details?.body || "", details?.isPlainText);

  const res = await aiCall("smart_compose", {
    intent,
    to,
    subject: details?.subject || "",
    draft,
    style,
    lang,
  });

  clearBusy(out);
  if (!res || !res.ok) { showError(out, res?.error || "AI call failed", res?.detail); return; }
  lastCompose = res.result;
  renderCompose(out, res.result);
  renderUsage("compose-usage", res.result);
}

function renderCompose(out, r) {
  out.innerHTML = "";
  const card = el("div", { class: "compose-output" }, [
    r.subject ? el("div", { class: "compose-subject", text: r.subject }) : null,
    el("div", { text: r.body }),
  ]);
  out.appendChild(card);

  const actions = el("div", { class: "compose-actions" }, [
    el("button", { class: "ghost",   onclick: () => copyComposeBody(),    text: browser.i18n.getMessage("action_copy")   || "Copy" }),
    el("button", { class: "ghost",   onclick: () => insertCompose(false), text: browser.i18n.getMessage("action_insert") || "Insert" }),
    el("button", { class: "primary", onclick: () => insertCompose(true),  text: browser.i18n.getMessage("action_replace")|| "Replace draft" }),
  ]);
  out.appendChild(actions);
}

async function insertCompose(replace) {
  if (!lastCompose || !composeTab) return;
  let body = lastCompose.body || "";
  let subject = lastCompose.subject || undefined;
  if (!replace) {
    const details = await getComposeDetails(composeTab.id);
    const existing = bodyToPlain(details?.body || "", details?.isPlainText);
    if (existing.trim()) body = existing + "\n\n" + body;
    subject = undefined;            // never overwrite subject when appending
  }
  await setComposeBody(composeTab.id, plainToHtml(body), subject);
}

async function copyComposeBody() {
  if (!lastCompose) return;
  try { await ClawUI.copyToClipboard(lastCompose.body || ""); } catch (_) {}
}

// ---------------------------------------------------------------------------
// Shared
// ---------------------------------------------------------------------------

function renderUsage(elId, r) {
  const usage = r.usage || {};
  const budget = r.budget || {};
  document.getElementById(elId).textContent =
    `${r.provider || "?"} / ${r.model || "?"}  •  in=${usage.input_tokens ?? "?"} out=${usage.output_tokens ?? "?"} units=${usage.units ?? "?"}  •  budget ${budget.units_used ?? "?"}/${budget.units_cap ?? "?"}`;
}

function flashBtnSuccess(btn) {
  if (!btn) return;
  const t = btn.textContent;
  btn.textContent = browser.i18n.getMessage("inserted") || "Inserted!";
  setTimeout(() => { btn.textContent = t; }, 1200);
}
