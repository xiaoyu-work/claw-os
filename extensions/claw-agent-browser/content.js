// Claw Agent — content script.
//
// Injected into every frame of every page (per manifest).  Owns a per-page
// element table so background.js can refer to specific elements by handle
// across multiple verbs without re-querying.

(() => {
  if (window.__clawAgentInstalled) return;
  window.__clawAgentInstalled = true;

  const REF_PREFIX = "el#";
  // axTree caps — protect callers from pathological pages.
  const AX_MAX_NODES = 10_000;
  const AX_MAX_BYTES = 16 * 1024 * 1024;
  // Periodic prune of dead WeakRefs so long-lived tabs don't leak the
  // `table` Map unbounded.
  const PRUNE_INTERVAL_MS = 30_000;
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
    if (!el) {
      table.delete(ref);
      return null;
    }
    if (!el.isConnected) return null;
    return el;
  }

  function pruneRefs() {
    for (const [id, wr] of table) {
      if (wr.deref() === undefined) table.delete(id);
    }
  }
  setInterval(pruneRefs, PRUNE_INTERVAL_MS);

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

  function axTree(root, depth, max, budget) {
    if (depth > max) return null;
    if (!(root instanceof Element)) return null;
    if (budget.nodes >= AX_MAX_NODES) { budget.truncated = true; return null; }
    if (budget.bytes >= AX_MAX_BYTES)  { budget.truncated = true; return null; }
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
    budget.nodes++;
    // Approximate byte cost — role/tag/name + JSON overhead. We don't
    // need exactness; we only need a hard ceiling.
    budget.bytes += (node.role || "").length + node.tag.length + node.name.length + node.ref.length + 32;
    if (root.children && root.children.length) {
      const kids = [];
      for (const c of root.children) {
        if (budget.nodes >= AX_MAX_NODES || budget.bytes >= AX_MAX_BYTES) {
          budget.truncated = true;
          break;
        }
        const n = axTree(c, depth + 1, max, budget);
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
      const budget = { nodes: 0, bytes: 0, truncated: false };
      const tree = axTree(document.body || document.documentElement, 0, 24, budget);
      return {
        kind: "ax",
        title: document.title || "",
        url: location.href,
        tree,
        truncated: budget.truncated,
        node_count: budget.nodes,
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
