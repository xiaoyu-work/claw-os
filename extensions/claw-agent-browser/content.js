// Claw Agent — content script.
//
// Injected into every frame of every page (per manifest).  Owns a per-page
// element table so background.js can refer to specific elements by handle
// across multiple verbs without re-querying.

(() => {
  if (window !== window.top) return;
  if (window.__clawAgentInstalled) return;
  window.__clawAgentInstalled = true;

  const refNonce = new Uint8Array(16);
  crypto.getRandomValues(refNonce);
  const REF_PREFIX =
    "doc#" + Array.from(refNonce, (byte) => byte.toString(16).padStart(2, "0")).join("") + ":el#";
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

  const SECRET_HINT =
    /\b(?:password|passwd|pwd|secret|token|api key|access key|private key|auth code|authentication code|one time|otp|pin|cvv|cvc|security code|ssn|social security|credit card|card number)\b/;
  const BUTTON_INPUT_TYPES = new Set(["button", "submit", "reset", "image"]);
  const NON_TEXT_INPUT_TYPES = new Set([
    ...BUTTON_INPUT_TYPES,
    "checkbox",
    "radio",
  ]);
  const TEXT_BLOCK_TAGS = new Set([
    "address", "article", "aside", "blockquote", "div", "dl", "fieldset",
    "figcaption", "figure", "footer", "form", "h1", "h2", "h3", "h4",
    "h5", "h6", "header", "hr", "li", "main", "nav", "ol", "p", "pre",
    "section", "table", "tbody", "td", "tfoot", "th", "thead", "tr", "ul",
  ]);

  function normaliseHint(value) {
    return String(value || "")
      .toLowerCase()
      .replace(/[^a-z0-9]+/g, " ")
      .trim();
  }

  function isSecretField(el) {
    if (!(el instanceof Element)) return false;
    const tag = (el.tagName || "").toLowerCase();
    const type = tag === "input"
      ? (el.getAttribute("type") || "text").toLowerCase()
      : "";
    if (type === "password" || type === "hidden") return true;

    const autocomplete = (el.getAttribute("autocomplete") || "")
      .toLowerCase()
      .split(/\s+/)
      .filter(Boolean);
    if (autocomplete.some((value) =>
      value === "current-password" ||
      value === "new-password" ||
      value === "one-time-code" ||
      value === "webauthn" ||
      value.startsWith("cc-")
    )) {
      return true;
    }
    if (
      el.hasAttribute("data-private") ||
      el.hasAttribute("data-sensitive") ||
      el.hasAttribute("data-secret")
    ) {
      return true;
    }

    const role = (el.getAttribute("role") || "").toLowerCase();
    const fieldLike =
      ["input", "textarea", "select"].includes(tag) ||
      el.isContentEditable ||
      ["textbox", "searchbox", "combobox", "spinbutton"].includes(role);
    if (!fieldLike) return false;

    const hint = normaliseHint([
      el.id,
      el.getAttribute("name"),
      el.getAttribute("aria-label"),
      el.getAttribute("placeholder"),
      el.getAttribute("data-testid"),
    ].filter(Boolean).join(" "));
    return SECRET_HINT.test(hint);
  }

  function isEditableValueElement(el) {
    if (!(el instanceof Element)) return false;
    if (el instanceof HTMLInputElement) {
      const type = (el.type || "text").toLowerCase();
      return !NON_TEXT_INPUT_TYPES.has(type);
    }
    return (
      el instanceof HTMLTextAreaElement ||
      el instanceof HTMLSelectElement ||
      el.isContentEditable
    );
  }

  function hasEditableValue(el) {
    if (!isEditableValueElement(el)) return false;
    if (el.isContentEditable) {
      return !!(el.innerText || el.textContent || "").trim();
    }
    if (el instanceof HTMLSelectElement) {
      return el.selectedIndex >= 0;
    }
    return !!String(el.value || "").trim();
  }

  function redactCurrentValueEcho(el, text) {
    let result = String(text || "");
    if (!result || !isEditableValueElement(el)) return result;

    const values = [];
    if (el.isContentEditable) {
      values.push((el.innerText || el.textContent || "").trim());
    } else if (el instanceof HTMLSelectElement) {
      values.push(String(el.value || "").trim());
      const option = el.selectedOptions && el.selectedOptions[0];
      if (option) values.push((option.textContent || "").trim());
    } else {
      values.push(String(el.value || "").trim());
    }
    for (const value of values.filter(Boolean)) {
      if (result.trim() === value) return "[redacted input]";
      if (value.length >= 3) {
        result = result.split(value).join("[redacted input]");
      }
    }
    return result;
  }

  function explicitAccessibleName(el) {
    const direct =
      el.getAttribute("aria-label") ||
      el.getAttribute("alt") ||
      el.getAttribute("title") ||
      el.getAttribute("placeholder") ||
      "";
    if (direct) return redactCurrentValueEcho(el, direct);
    if (el.labels && el.labels.length) {
      return redactCurrentValueEcho(el, Array.from(el.labels)
        .map((label) => safeRenderedText(label, 120))
        .filter(Boolean)
        .join(" "));
    }
    return "";
  }

  function safeRenderedText(root, limit) {
    let output = "";
    let visited = 0;

    function append(value) {
      if (!value || output.length >= limit) return;
      output += String(value).slice(0, limit - output.length);
    }

    function walk(node) {
      if (!node || output.length >= limit) return;
      if (++visited > AX_MAX_NODES) return;
      if (node.nodeType === Node.TEXT_NODE) {
        append(node.nodeValue || "");
        return;
      }
      if (!(node instanceof Element)) return;

      const tag = (node.tagName || "").toLowerCase();
      if (["script", "style", "noscript", "template"].includes(tag)) return;
      if (tag === "input" && (node.type || "").toLowerCase() === "hidden") return;

      const cs = node.ownerDocument && node.ownerDocument.defaultView
        ? node.ownerDocument.defaultView.getComputedStyle(node)
        : null;
      if (cs && (cs.display === "none" || cs.visibility === "hidden" || +cs.opacity === 0)) {
        return;
      }

      if (isSecretField(node)) {
        if (
          hasEditableValue(node) ||
          (node.textContent || "").trim()
        ) {
          append(" [redacted sensitive field] ");
        }
        return;
      }
      if (isEditableValueElement(node)) {
        if (hasEditableValue(node)) append(" [redacted input] ");
        return;
      }
      if (
        node instanceof HTMLInputElement &&
        BUTTON_INPUT_TYPES.has((node.type || "").toLowerCase())
      ) {
        append(node.getAttribute("aria-label") || node.type || "button");
        return;
      }

      const block = TEXT_BLOCK_TAGS.has(tag);
      if (block || tag === "br") append("\n");
      for (const child of node.childNodes) walk(child);
      if (block) append("\n");
    }

    walk(root);
    return output
      .split("\n")
      .map((line) => line.replace(/\s+/g, " ").trim())
      .filter(Boolean)
      .join("\n")
      .slice(0, limit);
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
    const text = safeRenderedText(el, 240);
    const secret = isSecretField(el);
    const attrs = {};
    for (const name of ["id", "name", "type", "role", "aria-label", "placeholder", "href", "autocomplete"]) {
      if (secret && (name === "aria-label" || name === "placeholder")) continue;
      const v = el.getAttribute && el.getAttribute(name);
      if (v) attrs[name] = redactCurrentValueEcho(el, v);
    }
    const r = el.getBoundingClientRect();
    return {
      ref: makeRef(el),
      tag,
      text,
      attrs,
      rect: { x: r.x | 0, y: r.y | 0, w: r.width | 0, h: r.height | 0 },
      visible: visible(el),
      secret,
      value_present: hasEditableValue(el),
    };
  }

  function axTree(root, depth, max, budget) {
    if (depth > max) return null;
    if (!(root instanceof Element)) return null;
    if (budget.nodes >= AX_MAX_NODES) { budget.truncated = true; return null; }
    if (budget.bytes >= AX_MAX_BYTES)  { budget.truncated = true; return null; }
    const tag = (root.tagName || "").toLowerCase();
    if (tag === "script" || tag === "style" || tag === "noscript" || tag === "template") return null;
    if (tag === "input" && (root.type || "").toLowerCase() === "hidden") return null;
    const secret = isSecretField(root);
    const role = root.getAttribute("role") || implicitRole(tag) || (secret ? "sensitive" : "");
    const name = secret
      ? ""
      : (
        explicitAccessibleName(root) ||
        (root.matches && root.matches("button,a")
          ? safeRenderedText(root, 120)
          : "") ||
        (root instanceof HTMLInputElement &&
        BUTTON_INPUT_TYPES.has((root.type || "").toLowerCase())
          ? (root.type || "button")
          : "")
      );
    const node = {
      role,
      tag,
      name: name ? name.replace(/\s+/g, " ").slice(0, 240) : "",
      ref: makeRef(root),
    };
    if (isEditableValueElement(root)) {
      node.secret = secret;
      node.value_present = hasEditableValue(root);
    }
    budget.nodes++;
    // Approximate byte cost — role/tag/name + JSON overhead. We don't
    // need exactness; we only need a hard ceiling.
    budget.bytes += (node.role || "").length + node.tag.length + node.name.length + node.ref.length + 32;
    if (secret) return node;
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
          text: document.body ? safeRenderedText(document.body, 50000) : "",
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
