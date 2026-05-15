// Mail Assistant — chat interface backed by the cos `chat` verb of
// apps/mail-ai. History is stored in storage.local.assistantHistory.
// Each "send" passes the existing history + a fresh snapshot of the
// user's recent messages so the model has context.

const { aiCall, showError, el, localiseDom } = window.ClawUI;

const HISTORY_KEY = "assistantHistory";
const MAX_HISTORY = 30;

document.addEventListener("DOMContentLoaded", async () => {
  localiseDom();

  document.getElementById("btn-send").addEventListener("click", onSend);
  document.getElementById("input").addEventListener("keydown", (e) => {
    if (e.key === "Enter" && (e.ctrlKey || e.metaKey)) {
      e.preventDefault();
      onSend();
    }
  });
  document.getElementById("btn-clear").addEventListener("click", clearHistory);
  document.getElementById("btn-options").addEventListener("click", () => {
    browser.runtime.openOptionsPage().catch(() => {});
  });

  await refresh();
});

async function refresh() {
  const history = await loadHistory();
  const mc = document.getElementById("messages");
  mc.innerHTML = "";
  if (history.length === 0) {
    mc.appendChild(el("div", { class: "msg system", text: browser.i18n.getMessage("assistant_welcome") ||
      "Hi! Ask me about your inbox. I can summarize threads, find emails, draft replies." }));
  } else {
    for (const m of history) renderMessage(m);
  }
  scrollBottom();
}

function renderMessage(m) {
  const mc = document.getElementById("messages");
  mc.appendChild(el("div", { class: `msg ${m.role}`, text: m.content }));
}

async function onSend() {
  const ta = document.getElementById("input");
  const text = ta.value.trim();
  if (!text) return;
  ta.value = "";

  const history = await loadHistory();
  history.push({ role: "user", content: text, ts: Date.now() });
  await saveHistory(history);
  renderMessage(history[history.length - 1]);
  scrollBottom();

  // Pull a snapshot of recent messages for the model.
  const recent = await browser.runtime.sendMessage({ kind: "listRecentMessages", limit: 25 });

  // Show a "thinking" placeholder.
  const mc = document.getElementById("messages");
  const ph = el("div", { class: "msg assistant", text: browser.i18n.getMessage("busy") || "Working..." });
  mc.appendChild(ph);
  scrollBottom();

  const res = await aiCall("chat", {
    history: history.slice(-MAX_HISTORY).map(({ role, content }) => ({ role, content })),
    recent: recent && recent.ok ? recent.messages : [],
    query: text,
    lang: (browser.i18n.getUILanguage() || "en").replace("-", "_"),
  });

  ph.remove();

  if (!res || !res.ok) {
    showError(mc, res?.error || "AI call failed", res?.detail);
    renderUsage(null);
    return;
  }

  const reply = (res.result.reply || "").trim() || "(empty reply)";
  history.push({ role: "assistant", content: reply, ts: Date.now() });
  await saveHistory(history);
  renderMessage(history[history.length - 1]);
  renderUsage(res.result);
  scrollBottom();
}

async function clearHistory() {
  await browser.storage.local.remove(HISTORY_KEY);
  await refresh();
}

function renderUsage(r) {
  const u = document.getElementById("usage");
  if (!r) { u.textContent = ""; return; }
  const usage = r.usage || {};
  const budget = r.budget || {};
  u.textContent =
    `${r.provider || "?"} / ${r.model || "?"}  •  in=${usage.input_tokens ?? "?"} out=${usage.output_tokens ?? "?"} units=${usage.units ?? "?"}  •  budget ${budget.units_used ?? "?"}/${budget.units_cap ?? "?"}`;
}

function scrollBottom() {
  const mc = document.getElementById("messages");
  mc.scrollTop = mc.scrollHeight;
}

async function loadHistory() {
  const obj = await browser.storage.local.get(HISTORY_KEY);
  return Array.isArray(obj[HISTORY_KEY]) ? obj[HISTORY_KEY] : [];
}

async function saveHistory(history) {
  // Trim so storage doesn't grow unbounded.
  const trimmed = history.slice(-MAX_HISTORY);
  await browser.storage.local.set({ [HISTORY_KEY]: trimmed });
}
