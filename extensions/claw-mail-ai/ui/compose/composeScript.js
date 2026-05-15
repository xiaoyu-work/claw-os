// composeScript.js — runs inside the compose window (per the
// `compose_scripts` manifest section). Adds an unobtrusive keyboard
// shortcut to launch the compose-action popup.
//
// Thunderbird ≥ 102 exposes Ctrl/⌘+Alt+J via the `commands` API, but we
// stick to a content-script-driven binding here because (a) MV3
// commands need user-facing wiring, and (b) this is more discoverable
// during development. End users can rebind through the standard
// keyboard preferences once we add a commands entry.
//
// All this script does is dispatch a message to the background, which
// then opens the popup via `compose_action.openPopup()`.

(function () {
  "use strict";

  function isSmartReplyKey(e) {
    // Ctrl/⌘ + Alt + J
    const mod = e.ctrlKey || e.metaKey;
    return mod && e.altKey && (e.key === "j" || e.key === "J");
  }

  document.addEventListener("keydown", (e) => {
    if (isSmartReplyKey(e)) {
      e.preventDefault();
      try {
        // The compose_action popup is the discoverable UI; we ping the
        // background to open it.
        browser.runtime.sendMessage({ kind: "openComposePopup" }).catch(() => {});
      } catch (_) {}
    }
  }, true);
})();
