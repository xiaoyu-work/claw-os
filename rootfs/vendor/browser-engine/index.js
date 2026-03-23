// Claw OS Browser Engine — URL to Markdown with full JS rendering.
//
// Core techniques adapted from Jina AI Reader (https://github.com/jina-ai/reader)
// Copyright 2020-2024 Jina AI Limited. Licensed under Apache License 2.0.
//
// Stripped down to the essential pipeline for agent consumption:
//   URL → Puppeteer (render JS, simulate scroll, wait for DOM stability)
//       → Readability (extract main content)
//       → Turndown (HTML → Markdown)
//       → JSON response
//
// No Firebase, no cloud functions, no billing, no search API.
// This is an OS-level service, not a web application.

const http = require("http");
const { Readability } = require("@mozilla/readability");
const { parseHTML } = require("linkedom");
const puppeteer = require("puppeteer");
const TurndownService = require("turndown");
const { gfm } = require("turndown-plugin-gfm");

// ---------------------------------------------------------------------------
// Browser pool
// ---------------------------------------------------------------------------

let browser = null;

async function getBrowser() {
  if (browser && browser.connected) return browser;
  browser = await puppeteer.launch({
    headless: true,
    // Use system-installed Chromium (no Puppeteer download needed)
    executablePath: process.env.CHROMIUM_PATH || "/usr/bin/chromium",
    args: [
      "--no-sandbox",
      "--disable-setuid-sandbox",
      "--disable-dev-shm-usage",
      "--disable-gpu",
      "--disable-blink-features=AutomationControlled",
    ],
  });
  browser.on("disconnected", () => {
    browser = null;
  });
  return browser;
}

// ---------------------------------------------------------------------------
// Page snapshot script — injected into every page
// Adapted from Jina Reader's SCRIPT_TO_INJECT_INTO_FRAME
// ---------------------------------------------------------------------------

const SNAPSHOT_SCRIPT = `
(function() {
  // Simulate scroll to trigger lazy loading (intersection observers)
  function simulateScroll() {
    const viewportHeight = window.innerHeight || document.documentElement.clientHeight;
    const totalHeight = Math.max(
      document.body.scrollHeight,
      document.documentElement.scrollHeight
    );
    const steps = Math.min(Math.ceil(totalHeight / viewportHeight), 15);
    for (let i = 0; i <= steps; i++) {
      setTimeout(() => {
        window.scrollTo(0, i * viewportHeight);
        if (i === steps) window.scrollTo(0, 0);
      }, i * 100);
    }
  }

  // Monitor DOM mutations to detect when page stabilizes
  let lastMutationAt = Date.now();
  const observer = new MutationObserver(() => {
    lastMutationAt = Date.now();
  });
  observer.observe(document.documentElement, {
    childList: true, subtree: true, attributes: true
  });

  window.__cosLastMutationAt = () => lastMutationAt;

  document.addEventListener('DOMContentLoaded', simulateScroll, { once: true });
  if (document.readyState !== 'loading') simulateScroll();
})();
`;

// ---------------------------------------------------------------------------
// Core: crawl a URL and return structured snapshot
// ---------------------------------------------------------------------------

async function crawl(url, options = {}) {
  const timeout = options.timeout || 30000;
  const waitStable = options.waitStable || 1500;
  const b = await getBrowser();

  const context = await b.createBrowserContext();
  const page = await context.newPage();

  try {
    // Anti-detection: realistic user agent
    const ua = await b.userAgent();
    await page.setUserAgent(
      ua
        .replace(/Headless/i, "")
        .replace(
          "Mozilla/5.0 (X11; Linux x86_64)",
          "Mozilla/5.0 (Windows NT 10.0; Win64; x64)"
        )
    );
    await page.setBypassCSP(true);
    await page.setViewport({ width: 1024, height: 1024 });

    // Request filtering — block known tracking/ad domains, limit total requests
    let reqCount = 0;
    const MAX_REQUESTS = 500;
    await page.setRequestInterception(true);
    page.on("request", (req) => {
      reqCount++;
      const reqUrl = req.url();

      // Block non-http protocols
      if (
        !reqUrl.startsWith("http:") &&
        !reqUrl.startsWith("https:") &&
        reqUrl !== "about:blank"
      ) {
        return req.abort("blockedbyclient");
      }

      // Block localhost/internal requests (security)
      try {
        const parsed = new URL(reqUrl);
        if (
          parsed.hostname === "localhost" ||
          parsed.hostname.startsWith("127.")
        ) {
          return req.abort("blockedbyclient");
        }
      } catch (_) {}

      // Rate limit
      if (reqCount > MAX_REQUESTS) {
        return req.abort("blockedbyclient");
      }

      return req.continue();
    });

    // Inject DOM stability monitoring before page loads
    await page.evaluateOnNewDocument(SNAPSHOT_SCRIPT);

    // Navigate
    await page.goto(url, {
      waitUntil: ["load", "domcontentloaded", "networkidle0"],
      timeout,
    });

    // Wait for DOM to stabilize (no mutations for waitStable ms)
    const stableDeadline = Date.now() + timeout;
    while (Date.now() < stableDeadline) {
      const lastMutation = await page
        .evaluate(() => window.__cosLastMutationAt())
        .catch(() => 0);
      if (Date.now() - lastMutation > waitStable) break;
      await new Promise((r) => setTimeout(r, 200));
    }

    // Extract page data
    const snapshot = await page.evaluate(() => {
      return {
        title: document.title || "",
        description:
          document.head
            ?.querySelector('meta[name="description"]')
            ?.getAttribute("content") || "",
        href: document.location.href,
        html: document.documentElement.outerHTML,
        text: document.body?.innerText || "",
      };
    });

    // Take screenshot if requested
    let screenshot = null;
    if (options.screenshot) {
      screenshot = await page.screenshot({ encoding: "base64" });
    }

    return { ...snapshot, screenshot };
  } finally {
    await context.close().catch(() => {});
  }
}

// ---------------------------------------------------------------------------
// Content extraction: HTML → clean Markdown
// Adapted from Jina Reader's snapshot-formatter.ts + jsdom.ts
// ---------------------------------------------------------------------------

function htmlToMarkdown(html, href) {
  const dom = parseHTML(html);
  const doc = dom.window.document;

  // Clean noise elements
  doc.querySelectorAll("svg").forEach((el) => (el.innerHTML = ""));

  // Run Readability to extract main content
  let parsed = null;
  try {
    parsed = new Readability(doc.cloneNode(true)).parse();
  } catch (_) {}

  // Determine source: Readability result or full page
  let sourceHtml;
  if (parsed && parsed.content) {
    // Validate Readability output isn't too short (Jina Reader's heuristic)
    const fullText = doc.body?.innerText || "";
    const parsedText = parsed.textContent || "";
    if (parsedText.length >= 0.3 * fullText.length) {
      sourceHtml = parsed.content;
    } else {
      sourceHtml = html;
    }
  } else {
    sourceHtml = html;
  }

  // Parse source for Turndown
  const sourceDom = parseHTML(sourceHtml);
  const sourceDoc = sourceDom.window.document;

  // Clean for LLM consumption
  sourceDoc
    .querySelectorAll("script, style, link, svg, noscript")
    .forEach((el) => el.remove());
  sourceDoc.querySelectorAll("[style]").forEach((el) => {
    const style = (el.getAttribute("style") || "").toLowerCase();
    if (!style.startsWith("display: none")) {
      el.removeAttribute("style");
    }
  });
  sourceDoc.querySelectorAll("[class]").forEach((el) => {
    el.removeAttribute("class");
  });
  sourceDoc.querySelectorAll("*").forEach((el) => {
    for (const attr of el.getAttributeNames()) {
      if (attr.startsWith("data-") || attr.startsWith("aria-")) {
        el.removeAttribute(attr);
      }
    }
  });

  // Hack for turndown GFM table plugin
  sourceDoc.querySelectorAll("table").forEach((t) => {
    Object.defineProperty(t, "rows", {
      value: Array.from(t.querySelectorAll("tr")),
      enumerable: true,
    });
  });
  Object.defineProperty(sourceDoc.documentElement, "cloneNode", {
    value: function () {
      return this;
    },
  });

  // Convert to Markdown
  const turndown = new TurndownService({
    headingStyle: "atx",
    codeBlockStyle: "fenced",
    bulletListMarker: "-",
  });
  turndown.use(gfm);

  // Image handling: resolve relative URLs, clean data URIs
  turndown.addRule("img-clean", {
    filter: "img",
    replacement: (_content, node) => {
      let src = node.getAttribute("src") || "";
      if (src.startsWith("data:")) {
        const dataSrc = node.getAttribute("data-src") || "";
        if (dataSrc && !dataSrc.startsWith("data:")) {
          src = dataSrc;
        } else {
          return "";
        }
      }
      try {
        src = new URL(src, href).toString();
      } catch (_) {}
      const alt = (node.getAttribute("alt") || "").trim();
      return alt ? `![${alt}](${src})` : `![](${src})`;
    },
  });

  // Link handling: resolve relative URLs
  turndown.addRule("link-resolve", {
    filter: "a",
    replacement: (content, node) => {
      let linkHref = node.getAttribute("href") || "";
      if (!linkHref || linkHref.startsWith("#") || linkHref.startsWith("javascript:")) {
        return content;
      }
      try {
        linkHref = new URL(linkHref, href).toString();
      } catch (_) {}
      const title = (node.getAttribute("title") || "").trim();
      return title
        ? `[${content}](${linkHref} "${title}")`
        : `[${content}](${linkHref})`;
    },
  });

  let markdown = "";
  try {
    markdown = turndown.turndown(sourceDoc.documentElement);
  } catch (_) {
    // Fallback to plain text
    markdown = sourceDoc.body?.innerText || "";
  }

  // Clean redundant empty lines
  markdown = markdown
    .split(/\r?\n/)
    .filter((line, i, arr) => line.trim() || (arr[i - 1] && arr[i - 1].trim()))
    .join("\n")
    .trim();

  // Extract links
  const linkDom = parseHTML(html);
  const links = Array.from(linkDom.window.document.querySelectorAll("a[href]"))
    .map((a) => {
      const text = (a.textContent || "").replace(/\s+/g, " ").trim();
      let linkUrl = a.getAttribute("href") || "";
      try {
        linkUrl = new URL(linkUrl, href).toString();
      } catch (_) {}
      return { text, href: linkUrl };
    })
    .filter((l) => l.text && l.href.startsWith("http"));

  return {
    title: parsed?.title || "",
    content: markdown,
    links,
    byline: parsed?.byline || "",
    excerpt: parsed?.excerpt || "",
  };
}

// ---------------------------------------------------------------------------
// HTTP Server — the OS-level service
// ---------------------------------------------------------------------------

const PORT = parseInt(process.env.PORT || "3000");

const server = http.createServer(async (req, res) => {
  // Health check
  if (req.url === "/" || req.url === "/health") {
    res.writeHead(200, { "Content-Type": "application/json" });
    return res.end(JSON.stringify({ status: "ok", engine: "cos-browser-engine" }));
  }

  // Parse URL from path: GET /<encoded-url>
  // Or from query: GET /?url=<encoded-url>
  let targetUrl = null;
  const parsedReq = new URL(req.url, `http://localhost:${PORT}`);

  if (parsedReq.searchParams.has("url")) {
    targetUrl = parsedReq.searchParams.get("url");
  } else {
    // Path-based: everything after first /
    const pathUrl = req.url.slice(1);
    if (pathUrl && (pathUrl.startsWith("http://") || pathUrl.startsWith("https://"))) {
      targetUrl = decodeURIComponent(pathUrl);
    }
  }

  if (!targetUrl) {
    res.writeHead(400, { "Content-Type": "application/json" });
    return res.end(
      JSON.stringify({ error: "No URL provided. Use: GET /?url=<url>" })
    );
  }

  try {
    const wantScreenshot = parsedReq.searchParams.has("screenshot");
    const snapshot = await crawl(targetUrl, { screenshot: wantScreenshot });
    const result = htmlToMarkdown(snapshot.html, snapshot.href);

    const response = {
      url: snapshot.href,
      title: result.title || snapshot.title,
      description: snapshot.description,
      content: result.content,
      links: result.links,
      byline: result.byline,
      engine: "cos-browser-engine",
    };

    if (snapshot.screenshot) {
      response.screenshot = snapshot.screenshot;
    }

    res.writeHead(200, { "Content-Type": "application/json" });
    res.end(JSON.stringify(response));
  } catch (err) {
    res.writeHead(500, { "Content-Type": "application/json" });
    res.end(JSON.stringify({ error: err.message, url: targetUrl }));
  }
});

server.listen(PORT, () => {
  console.log(`cos-browser-engine listening on port ${PORT}`);
});

// Graceful shutdown
process.on("SIGTERM", async () => {
  server.close();
  if (browser) await browser.close();
  process.exit(0);
});
process.on("SIGINT", async () => {
  server.close();
  if (browser) await browser.close();
  process.exit(0);
});
