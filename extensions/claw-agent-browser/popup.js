// Claw Agent — popup.  Reads SW state, exposes a STOP button.

async function refresh() {
  try {
    const bg = await chrome.runtime.getBackgroundPage?.();
    // MV3: no getBackgroundPage; use messaging to fetch state instead.
  } catch (_) {}

  let state = "idle";
  let tab = null;
  try {
    const reply = await chrome.runtime.sendMessage({ claw_agent_popup: "status" });
    if (reply) {
      state = reply.state || "idle";
      tab = reply.tab;
    }
  } catch (_) {
    state = "error";
  }
  const el = document.getElementById("state");
  el.className = "pill " + state;
  el.textContent = state;
  document.getElementById("tab").textContent = tab == null ? "—" : String(tab);
}

document.getElementById("stop").addEventListener("click", async () => {
  try {
    await chrome.runtime.sendMessage({ claw_agent_popup: "stop" });
  } catch (_) {}
  refresh();
});

refresh();
setInterval(refresh, 500);
