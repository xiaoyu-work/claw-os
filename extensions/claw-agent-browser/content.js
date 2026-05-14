// Claw Agent — content script.
//
// Injected into every frame of every page (per manifest).  Owns a per-page
// element table so background.js can refer to specific elements by handle
// across multiple verbs without re-querying.

(() => {
  if (window.__clawAgentInstalled) return;
  window.__clawAgentInstalled = true;

  const REF_PREFIX = "el#";
  let nextRef = 1;
  const table = new Map(); // refId -> WeakRef<Element>

  function makeRef(el) {
    const id = REF_PREFIX + nextRef++;
    table.set(id, new WeakRef(el));
    return id;
  }

  function resolveRef(ref) {
    const wr = table.get(ref);
    if (!wr) return null;
    const el = wr.deref();
    if (!el) return null;
    if (!el.isConnected) return null;
    return el;
  }

  function isSecretField(el) {
    if (!(el instanceof HTMLInputElement)) return false;
    const t = (el.type || "").toLowerCase();
    if (t === "password") return true;
    const ac = (el.getAttribute("autocomplete") || "").toLowerCase();
    return (
      ac === "current-password" ||
      ac === "new-password" ||
      ac === "one-time-code" ||
      ac.startsWith("cc-")
    );
  }

  function visible(el) {
    if (!el.isConnected) return false;
    const cs = el.ownerDocument && el.ownerDocument.defaultView
      ? el.ownerDocument.defaultView.getComputedStyle(el)
      : null;
    if (cs && (cs.display === "none" || cs.visibility === "hidden" || +cs.opacity === 0)) return false;
    const r = el.getBoundingClientRect();
    return r.width > 0 && r.height > 0;
  }

  function summarise(el) {
    const tag = (el.tagName || "").toLowerCase();
    const text =
      (el.textContent || el.value || "").trim().replace(/\s+/g, " ").slice(0, 240);
    const attrs = {};
    for (const name of ["id", "name", "type", "role", "aria-label", "placeholder", "href", "autocomplete"]) {
      const v = el.getAttribute && el.getAttribute(name);
      if (v) attrs[name] = v;
    }
    const r = el.getBoundingClientRect();
    return {
      ref: makeRef(el),
      tag,
      text,
      attrs,
      rect: { x: r.x | 0, y: r.y | 0, w: r.width | 0, h: r.height | 0 },
      visible: visible(el),
      secret: isSecretField(el),
    };
  }

  function axTree(root, depth, max) {
    if (depth > max) return null;
    if (!(root instanceof Element)) return null;
    const tag = (root.tagName || "").toLowerCase();
    if (tag === "script" || tag === "style" || tag === "noscript" || tag === "template") return null;
    const role = root.getAttribute("role") || implicitRole(tag);
    const name =
      root.getAttribute("aria-label") ||
      root.getAttribute("alt") ||
      root.getAttribute("title") ||
      (root.matches && root.matches("input,select,textarea,button,a")
        ? (root.value || root.textContent || "").trim().slice(0, 120)
        : "");
    const node = {
      role,
      tag,
      name: name ? name.replace(/\s+/g, " ").slice(0, 240) : "",
      ref: makeRef(root),
    };
    if (root.children && root.children.length) {
      const kids = [];
      for (const c of root.children) {
        const n = axTree(c, depth + 1, max);
        if (n) kids.push(n);
      }
      if (kids.length) node.children = kids;
    }
    if (!node.role && !node.name && !node.children) return null;
    return node;
  }

  function implicitRole(tag) {
    switch (tag) {
      case "a": return "link";
      case "button": return "button";
      case "input": return "textbox";
      case "select": return "combobox";
      case "textarea": return "textbox";
      case "form": return "form";
      case "nav": return "navigation";
      case "main": return "main";
      case "header": return "banner";
      case "footer": return "contentinfo";
      case "h1": case "h2": case "h3": case "h4": case "h5": case "h6": return "heading";
      case "img": return "image";
      default: return "";
    }
  }

  // ---------------------------------------------------------------------
  // Verb dispatch
  // ---------------------------------------------------------------------

  const VERBS = {
    query({ selector }) {
      let nodes;
      try {
        nodes = document.querySelectorAll(selector);
      } catch (e) {
        throw new Error(`invalid selector: ${e.message}`);
      }
      const out = [];
      const MAX = 50;
      for (let i = 0; i < nodes.length && i < MAX; i++) out.push(summarise(nodes[i]));
      return { matches: out, total: nodes.length, truncated: nodes.length > MAX };
    },

    click({ ref }) {
      const el = resolveRef(ref);
      if (!el) throw new Error(`stale ref: ${ref}`);
      el.scrollIntoView({ behavior: "instant", block: "center" });
      if (typeof el.click === "function") el.click();
      else el.dispatchEvent(new MouseEvent("click", { bubbles: true, cancelable: true }));
      return { ref, clicked: true };
    },

    fill({ ref, value, allow_secret }) {
      const el = resolveRef(ref);
      if (!el) throw new Error(`stale ref: ${ref}`);
      if (isSecretField(el) && !allow_secret) {
        throw new Error(
          "this field is detected as secret (password/cc/otp). " +
          "Use dom.fill_secret if you have approval."
        );
      }
      if ("value" in el) {
        const proto = Object.getPrototypeOf(el);
        const desc =
          Object.getOwnPropertyDescriptor(proto, "value") ||
          Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value");
        if (desc && desc.set) desc.set.call(el, value);
        else el.value = value;
      } else if (el.isContentEditable) {
        el.textContent = value;
      } else {
        throw new Error("element is not fillable");
      }
      el.dispatchEvent(new Event("input", { bubbles: true }));
      el.dispatchEvent(new Event("change", { bubbles: true }));
      return { ref, filled: true };
    },

    snapshot({ snapshot_kind }) {
      if (snapshot_kind === "text") {
        return {
          kind: "text",
          title: document.title || "",
          url: location.href,
          text: (document.body && document.body.innerText
            ? document.body.innerText
            : "").slice(0, 50000),
        };
      }
      return {
        kind: "ax",
        title: document.title || "",
        url: location.href,
        tree: axTree(document.body || document.documentElement, 0, 24),
      };
    },
  };

  chrome.runtime.onMessage.addListener((msg, _sender, sendResponse) => {
    if (!msg || !msg.claw_agent || !msg.kind) return false;
    const handler = VERBS[msg.kind];
    if (!handler) {
      sendResponse({ error: `unknown content verb: ${msg.kind}` });
      return false;
    }
    try {
      const result = handler(msg);
      sendResponse({ result });
    } catch (e) {
      sendResponse({ error: e && e.message ? e.message : String(e) });
    }
    return false;
  });
})();
