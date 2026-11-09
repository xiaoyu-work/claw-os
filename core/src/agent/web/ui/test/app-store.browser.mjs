import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { once } from "node:events";
import { mkdir, readFile, rm } from "node:fs/promises";
import { createServer } from "node:http";
import path from "node:path";
import { setTimeout as delay } from "node:timers/promises";

const dist = path.resolve(process.env.APP_STORE_UI_DIST || ".activity-validation/store-dist");
const product = path.resolve(process.env.APP_STORE_PRODUCT_FIXTURE || ".activity-validation/store-product");
const profile = path.resolve(".activity-validation/store-browser");
const browser = process.env.APP_STORE_BROWSER || "C:\\Program Files\\Google\\Chrome\\Application\\chrome.exe";
const nonce = "a".repeat(64);
const digest = "b".repeat(64);
const requestId = "c".repeat(32);
const reviewId = "rv-browser-fixture";
const requests = [];
const browserIssues = [];
const entries = new Map();
let installed = false;
let catalogAvailable = true;
const descriptor = JSON.parse(await readFile(path.join(product, "web-ui.json"), "utf8"));
const manifest = JSON.parse(await readFile(path.join(product, "app.json"), "utf8"));
const tools = manifest.mcp.tools.map((tool) => ({ ...tool, command: tool.name.split(".").at(-1) }));
const app = { id: "kv", name: "Key-Value Store", version: manifest.version, state: "installed",
  web_ready: true, source_name: "storage-sdk", source_kind: "capability",
  content_digest: `sha256:${digest}`, protocol: "claw.web-app/v1", tools };
const review = {
  schema_version: 1, id: reviewId, revision: "9007199254740993", kind: "install", status: "pending",
  subject: { app_id: "kv", name: app.name, version: app.version, publisher: "synthetic public fixture" },
  permissions: [{ id: "kv-write", label: "Write named key", blurb: "Store the requested value.",
    risk: "low", scope: { description: "Exact requested key" }, condition: null, uses: [] }],
  disclosures: [{ label: "Execution", value: "Authenticated MCP service in the normal App Host" }],
};
const authorization = { id: "d".repeat(64), review_id: reviewId, revision: review.revision,
  label: app.name, target: "kv", choice: null, action: "confirm_install",
  phase: "finished", prompt: null, message: null };
const mime = (filename) => ({ ".html": "text/html", ".js": "text/javascript", ".css": "text/css",
  ".json": "application/json", ".png": "image/png", ".svg": "image/svg+xml" }[path.extname(filename)] || "application/octet-stream");
const server = createServer(async (req, res) => {
  try {
    const url = new URL(req.url, "http://fixture");
    const chunks = []; for await (const chunk of req) chunks.push(chunk);
    const body = chunks.length ? JSON.parse(Buffer.concat(chunks).toString()) : null;
    if (url.pathname.startsWith("/api/")) requests.push({ path: url.pathname, body, auth: req.headers.authorization });
    const json = (value, status = 200) => { res.writeHead(status, { "Content-Type": "application/json" }); res.end(JSON.stringify(value)); };
    if (url.pathname === "/api/auth/token") return json({ access_token: "store-browser-token", token_type: "Bearer" });
    if (url.pathname === "/api/meta") return json({ version: "test-only", auth_required: false });
    if (url.pathname === "/api/sessions") return json({ sessions: [] });
    if (url.pathname.includes("/notifications") || url.pathname === "/api/inbox") {
      if (url.pathname.endsWith("/stream")) { res.writeHead(200, { "Content-Type": "text/event-stream" }); res.end(); return; }
      return json({ notifications: [], unread_count: 0, preferences: {} });
    }
    if (url.pathname === "/api/apps") return json({ architecture: "amd64", apps: installed ? [app] : [] });
    if (url.pathname === "/api/app-installs") return json({ requests: [] });
    if (url.pathname === "/api/app-store") {
      if (!catalogAvailable) return json({ error: "Official release catalog unavailable" }, 503);
      return json({ architecture: "amd64", installed: installed ? [app] : [], catalog: {
        schema: "claw.app-store/v1", repository: "xiaoyu-work/clawos-app", runtime_abi: 1,
        generation: "1", expires_at: new Date(Date.now() + 86400000).toISOString(),
        apps: [{ app_id: "kv", app_version: app.version, release_version: "1.0.0", architecture: "all",
          artifact: { file: `claw-app-snapshot-${digest}.tar.gz`, size: 4096, sha256: digest, media_type: "application/gzip" },
          provenance: { content_digest: app.content_digest },
          display: { name: app.name, description: "Persistent string key-value storage",
            source_name: "storage-sdk", source_kind: "capability" } }],
      } });
    }
    if (url.pathname === "/api/apps/kv/install") {
      assert.deepEqual(body, { sha256: digest });
      return json({ request_id: requestId, app_id: "kv", status: "review_required", review });
    }
    if (url.pathname === `/api/app-reviews/${reviewId}/authorization`) {
      assert.equal(body.revision, review.revision); return json({ ...authorization, phase: "starting" });
    }
    if (url.pathname === `/api/authorizations/${authorization.id}`) return json(authorization);
    if (url.pathname === `/api/app-installs/${requestId}/commit`) { installed = true; return json({ status: "installed", app }); }
    if (url.pathname === "/api/apps/kv/open") return json({ status: "ready", view_id: nonce, nonce,
      src: `/app-ui/${nonce}/index.html`, app });
    if (url.pathname.startsWith("/api/app-views/")) return json({ closed: true });
    if (url.pathname === "/api/apps/kv/call") {
      assert.equal(body.view_id, nonce);
      assert.ok(tools.some((tool) => tool.name === body.tool));
      const args = body.arguments;
      const command = body.tool.split(".").at(-1);
      let result;
      if (command === "set") { entries.set(args.key, args.value); result = "ok"; }
      else if (command === "get") result = entries.get(args.key) || "";
      else if (command === "del") { entries.delete(args.key); result = "ok"; }
      else result = [...entries.keys()];
      return json({ ok: true, result: { content: [{ type: "text", text: JSON.stringify(result) }], structuredContent: result } });
    }
    if (url.pathname.startsWith("/api/")) return json({ error: "Unexpected fixture API" }, 404);
    if (url.pathname.startsWith(`/app-ui/${nonce}/`)) {
      const filename = url.pathname.slice(`/app-ui/${nonce}/`.length);
      assert.ok(!filename.includes(".."));
      const content = await readFile(path.join(product, "web", filename));
      res.writeHead(200, { "Content-Type": mime(filename),
        "Content-Security-Policy": "default-src 'none'; script-src 'self'; style-src 'self' 'unsafe-inline'; connect-src 'none'; frame-ancestors 'self'",
        "Referrer-Policy": "no-referrer" }); res.end(content); return;
    }
    const filename = url.pathname === "/" ? "index.html" : url.pathname.slice(1);
    res.writeHead(200, { "Content-Type": mime(filename) }); res.end(await readFile(path.join(dist, filename)));
  } catch (error) { res.writeHead(500); res.end(String(error)); }
});
await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
const origin = `http://127.0.0.1:${server.address().port}`;
await mkdir(profile, { recursive: true });
const chrome = spawn(browser, ["--headless=new", "--disable-gpu", "--no-first-run", "--no-default-browser-check",
  "--disable-extensions",
  `--user-data-dir=${profile}`, "--remote-debugging-port=0", "--window-size=1440,1000", "about:blank"], { stdio: "ignore" });
let socket;
try {
  let port;
  for (let attempt = 0; attempt < 100; attempt++) {
    try { port = (await readFile(path.join(profile, "DevToolsActivePort"), "utf8")).split("\n")[0]; break; } catch {}
    await delay(100);
  }
  assert.ok(port, "Chromium did not become ready");
  const tabs = await (await fetch(`http://127.0.0.1:${port}/json/list`)).json();
  socket = new WebSocket(tabs.find((tab) => tab.type === "page").webSocketDebuggerUrl);
  await once(socket, "open");
  let sequence = 0; const pending = new Map();
  socket.addEventListener("message", (event) => {
    const message = JSON.parse(event.data);
    if (message.method === "Log.entryAdded" || message.method === "Runtime.exceptionThrown") browserIssues.push(message.params);
    if (message.id && pending.has(message.id)) { pending.get(message.id)(message); pending.delete(message.id); }
  });
  const send = (method, params = {}, sessionId) => new Promise((resolve, reject) => {
    const id = ++sequence; const timer = setTimeout(() => { pending.delete(id); reject(new Error(`CDP timeout: ${method}`)); }, 15000);
    pending.set(id, (message) => { clearTimeout(timer); message.error ? reject(new Error(JSON.stringify(message.error))) : resolve(message.result); });
    socket.send(JSON.stringify({ id, method, params, ...(sessionId ? { sessionId } : {}) }));
  });
  const evaluate = async (expression) => {
    const result = await send("Runtime.evaluate", { expression, returnByValue: true, awaitPromise: true });
    if (result.exceptionDetails) throw new Error(JSON.stringify(result.exceptionDetails));
    return result.result.value;
  };
  const wait = async (expression) => {
    for (let attempt = 0; attempt < 100; attempt++) { if (await evaluate(expression)) return; await delay(100); }
    throw new Error(`Browser condition failed: ${expression}\n${await evaluate("document.body.innerText")}`);
  };
  const clickText = async (text) => { await evaluate(`Array.from(document.querySelectorAll('button')).find(item => item.textContent.trim() === ${JSON.stringify(text)})?.click()`); };
  await send("Log.enable");
  await send("Runtime.enable");
  await send("Page.navigate", { url: `${origin}/#/app-store` });
  await wait("!!document.querySelector('input')");
  await evaluate("localStorage.setItem('cos.token','store-browser-token'); location.reload()");
  await wait("document.body.innerText.includes('Persistent string key-value storage')");
  await clickText("Install");
  await wait("document.body.innerText.includes('Review and install App')");
  await clickText("Confirm and install");
  await wait("document.body.innerText.includes('installed.')");
  assert.ok(installed);
  await clickText("Open");
  await wait("!!document.querySelector('iframe')");
  await wait("document.querySelector('iframe').contentDocument === null");
  let target;
  for (let attempt = 0; attempt < 100; attempt++) {
    const targets = await send("Target.getTargets");
    target = targets.targetInfos.find((item) => item.type === "iframe" && item.url.includes(`/app-ui/${nonce}/`));
    if (target) break;
    await delay(100);
  }
  assert.ok(target, JSON.stringify({
    message: "The product frame must complete its navigation", browserIssues,
    targets: await send("Target.getTargets"), source: await evaluate("document.querySelector('iframe').src"),
  }));
  const attached = await send("Target.attachToTarget", { targetId: target.targetId, flatten: true });
  const frameEvaluate = async (expression) => {
    const result = await send("Runtime.evaluate", { expression, returnByValue: true }, attached.sessionId);
    if (result.exceptionDetails) throw new Error(JSON.stringify(result.exceptionDetails));
    return result.result.value;
  };
  for (let attempt = 0; attempt < 100; attempt++) {
    if (await frameEvaluate("document.querySelector('#kv') && !document.querySelector('#kv').hidden")) break;
    await delay(100);
  }
  await frameEvaluate("document.querySelector('#key').value='browser-proof'; document.querySelector('#value').value='persisted'; document.querySelector('#set').click()");
  await wait(`document.body.innerText.includes('Isolated product UI')`);
  for (let attempt = 0; attempt < 100 && entries.get("browser-proof") !== "persisted"; attempt++) await delay(100);
  assert.equal(entries.get("browser-proof"), "persisted", "Product UI must make the owning App call");
  await frameEvaluate(`document.querySelector('details').open=true;
    [...document.querySelectorAll('details button')].find(button=>button.textContent==='set').click()`);
  for (let attempt = 0; attempt < 100; attempt++) {
    if (await frameEvaluate("!!document.querySelector('dialog')")) break;
    await delay(100);
  }
  await frameEvaluate(`document.querySelector('dialog [name=key]').value='form-proof';
    document.querySelector('dialog [name=value]').value='submitted';
    document.querySelector('dialog button[type=submit]').click()`);
  for (let attempt = 0; attempt < 100 && entries.get("form-proof") !== "submitted"; attempt++) await delay(100);
  assert.equal(entries.get("form-proof"), "submitted", "A real submit-button click must work without allow-forms");
  const before = requests.filter((item) => item.path.endsWith("/call")).length;
  await evaluate(`window.dispatchEvent(new MessageEvent('message', {source: document.querySelector('iframe').contentWindow,
    origin:'null', data:{protocol:'claw.web-app/v1',type:'call',nonce:${JSON.stringify(nonce)},id:'forged',tool:'email.send',arguments:{}}}))`);
  await delay(200);
  assert.equal(requests.filter((item) => item.path.endsWith("/call")).length, before);
  assert.equal(requests.filter((item) => item.path === "/api/chat").length, 0);
  assert.ok(requests.filter((item) => item.path === "/api/apps/kv/call").every((item) => item.auth === "Bearer store-browser-token"));
  catalogAvailable = false;
  await send("Page.navigate", { url: `${origin}/#/apps` });
  await wait("document.body.innerText.includes('Key-Value Store')");
  console.log("PASS Store fixture presentation, exact-revision install review, isolated product UI, real form clicks, same-App calls and offline installed access (not production clawd acceptance)");
} finally {
  if (socket?.readyState === WebSocket.OPEN) socket.close();
  chrome.kill();
  await once(chrome, "exit").catch(() => {});
  server.closeAllConnections();
  await new Promise((resolve) => server.close(resolve));
  await rm(profile, { recursive: true, force: true });
}
