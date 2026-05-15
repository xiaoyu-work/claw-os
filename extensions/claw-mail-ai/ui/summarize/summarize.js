// Summarize popup.
//
// Triggered three ways:
//   1. message_display_action (toolbar button in the read window)
//   2. context menu "Summarize with Claw AI"
//   3. assistant page action
//
// Always works on the currently-displayed message. If launched from a
// background menu click, `storage.local.pending.messageIds[0]` carries
// the seed id.

const { aiCall, showBusy, clearBusy, showError, el, localiseDom, copyToClipboard } = window.ClawUI;
const { getBody, getDisplayedMessage } = window.ClawMessages;

let lastResult = null;

document.addEventListener("DOMContentLoaded", async () => {
  localiseDom();
  document.getElementById("btn-refresh").addEventListener("click", run);
  document.getElementById("btn-copy").addEventListener("click", onCopy);
  document.getElementById("btn-open-assistant").addEventListener("click", () =>
    browser.runtime.sendMessage({ kind: "openAssistant" })
  );
  await run();
});

async function pickMessageId() {
  // Prefer the pending payload from a menu click; fall back to whatever
  // message is currently displayed.
  const pending = await browser.runtime.sendMessage({ kind: "getPending" });
  if (pending && pending.kind === "summarize" && pending.messageIds?.length) {
    await browser.runtime.sendMessage({ kind: "clearPending" });
    return pending.messageIds[0];
  }
  const displayed = await getDisplayedMessage();
  return displayed?.id ?? null;
}

async function run() {
  const out = document.getElementById("out");
  const meta = document.getElementById("meta");
  showBusy(out);
  meta.textContent = "";
  document.getElementById("usage").textContent = "";

  const id = await pickMessageId();
  if (id == null) {
    clearBusy(out);
    showError(out, browser.i18n.getMessage("err_no_message") || "No message selected.");
    return;
  }

  let body;
  try {
    body = await getBody(id);
  } catch (e) {
    clearBusy(out);
    showError(out, browser.i18n.getMessage("err_fetch_body") || "Could not read this message.", String(e));
    return;
  }
  if (!body || !body.plain?.trim()) {
    clearBusy(out);
    showError(out, browser.i18n.getMessage("err_empty_body") || "Empty message body.");
    return;
  }

  meta.textContent = `${body.from || ""}  •  ${body.subject || ""}`;

  const settings = await ClawUI.getSettings();
  const res = await aiCall("summarize", {
    body: body.plain,
    subject: body.subject,
    from: body.from,
    lang: detectLang(settings),
  });

  clearBusy(out);

  if (!res || !res.ok) {
    showError(out, res?.error || "AI call failed", res?.detail);
    return;
  }

  lastResult = res.result;
  renderSummary(out, res.result);
  renderUsage(res.result);
}

function detectLang(_settings) {
  // Use the browser UI language by default. Users can override per call
  // through the assistant if they want a different output language.
  return (browser.i18n.getUILanguage() || "en").replace("-", "_");
}

function renderSummary(out, r) {
  out.innerHTML = "";

  // Summary line + sentiment badge.
  const header = el("div", { class: "card" }, [
    el("div", { class: "section-label", "data-i18n": "label_summary", text: "Summary" }),
    el("div", { class: "summary-text", text: r.summary || "(no summary)" }),
    el("div", { style: "margin-top: 6px;" }, [sentimentBadge(r.sentiment)]),
  ]);
  out.appendChild(header);

  // Key points.
  if (r.key_points && r.key_points.length) {
    const card = el("div", { class: "card" }, [
      el("div", { class: "section-label", "data-i18n": "label_key_points", text: "Key Points" }),
      el("ul", { class: "bullet-list" }, r.key_points.map(p => el("li", { text: String(p) }))),
    ]);
    out.appendChild(card);
  }

  // Action items.
  if (r.action_items && r.action_items.length) {
    const items = r.action_items.map(item =>
      el("div", { class: "action-item" }, [
        el("div", { class: "check" }),
        el("div", { class: "grow", text: String(item) }),
      ])
    );
    const card = el("div", { class: "card" }, [
      el("div", { class: "section-label", "data-i18n": "label_action_items", text: "Action Items" }),
      ...items,
    ]);
    out.appendChild(card);
  }

  // Raw fallback (only when the model didn't return parseable JSON).
  if (r.raw) {
    const card = el("div", { class: "card" }, [
      el("div", { class: "section-label", text: "Raw model output" }),
      el("pre", { class: "muted", text: r.raw }),
    ]);
    out.appendChild(card);
  }

  localiseDom(out);
}

function sentimentBadge(sentiment) {
  const map = {
    positive: { cls: "emerald", text: "positive" },
    neutral:  { cls: "muted",   text: "neutral" },
    negative: { cls: "amber",   text: "negative" },
    urgent:   { cls: "red",     text: "urgent" },
  };
  const cfg = map[sentiment] || map.neutral;
  return el("span", { class: `tag ${cfg.cls}`, text: cfg.text });
}

function renderUsage(r) {
  const usage = r.usage || {};
  const budget = r.budget || {};
  const used = budget.units_used ?? "?";
  const cap = budget.units_cap ?? "?";
  document.getElementById("usage").textContent =
    `${r.provider || "?"} / ${r.model || "?"}  •  in=${usage.input_tokens ?? "?"} out=${usage.output_tokens ?? "?"} units=${usage.units ?? "?"}  •  budget ${used}/${cap}`;
}

async function onCopy() {
  if (!lastResult) return;
  const parts = [];
  if (lastResult.summary) parts.push(lastResult.summary);
  if (lastResult.key_points?.length) {
    parts.push("");
    parts.push("Key points:");
    for (const k of lastResult.key_points) parts.push(`• ${k}`);
  }
  if (lastResult.action_items?.length) {
    parts.push("");
    parts.push("Action items:");
    for (const a of lastResult.action_items) parts.push(`☐ ${a}`);
  }
  try {
    await copyToClipboard(parts.join("\n"));
    flashCopied();
  } catch (e) {
    console.warn(e);
  }
}

function flashCopied() {
  const btn = document.getElementById("btn-copy");
  const old = btn.textContent;
  btn.textContent = browser.i18n.getMessage("copied") || "Copied!";
  setTimeout(() => { btn.textContent = old; }, 1200);
}
