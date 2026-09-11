import assert from "node:assert/strict";
import { spawn, spawnSync } from "node:child_process";
import { randomUUID } from "node:crypto";
import { once } from "node:events";
import { access, mkdir, readFile, rm } from "node:fs/promises";
import { createServer } from "node:http";
import path from "node:path";
import { setTimeout as delay } from "node:timers/promises";

// Uses an installed Chromium browser and Node's built-in CDP WebSocket client;
// no browser download, additional dependency, or live agent credentials.
const dist = path.resolve(process.env.ACTIVITY_UI_DIST || ".activity-validation/dist");
const profile = path.resolve(".activity-validation", `browser-${randomUUID()}`);
const bootstrap = "0".repeat(64);
const accessToken = "activity-browser-regression";
const activities = new Map();
const jobs = new Map();
const requests = [];
const fixtureErrors = [];
const browserErrors = [];
const holds = [];
const objectDescription = {
  object: { app_id: "archive", object_type: "entry", object_id: " release.status /?#& ", revision: "rev 2" },
  reference: "app://archive/entry?id=%20release.status%20%2F%3F%23%26%20&revision=rev%202",
  app_name: "Archive",
  app_version: "1.0.0",
  object_label: "Archived entry",
  object_summary: '<img src="https://objects.invalid/private" onerror="window.objectMetadataExecuted=true">',
  invocation: { app_id: "archive", operation: "get", args: ["--revision=rev 2", "--", " release.status /?#& "] },
  provenance: { publisher: "fixture-only", ignored_metadata: "Not a UI permission grant" },
};
const declaredMetadata = { status: "declared", description: objectDescription, error: null };
const unpinnedDescription = {
  ...objectDescription,
  object: { app_id: "archive", object_type: "entry", object_id: " release.status /?#& " },
  reference: "app://archive/entry?id=%20release.status%20%2F%3F%23%26%20",
  invocation: { app_id: "archive", operation: "get", args: ["--", " release.status /?#& "] },
};
const objectMetadata = new Map([
  [objectDescription.reference, declaredMetadata],
  [unpinnedDescription.reference, { status: "declared", description: unpinnedDescription, error: null }],
]);
let activityNumber = 0;
let jobNumber = 0;
let invalidDetailOnce = null;
let invalidObjectsOnce = null;
let browser;
let cdp;

const clone = (value) => structuredClone(value);
const timestamp = () => new Date().toISOString();

function holdRequest(method, pathname) {
  let entered;
  let release;
  const seen = new Promise((resolve) => { entered = resolve; });
  const gate = new Promise((resolve) => { release = resolve; });
  holds.push({ method, pathname, entered, gate });
  return { seen, release };
}

async function reply(req, res, value, status = 200) {
  const snapshot = clone(value);
  const pathname = new URL(req.url, "http://localhost").pathname;
  const index = holds.findIndex((hold) => hold.method === req.method && hold.pathname === pathname);
  if (index >= 0) {
    const [hold] = holds.splice(index, 1);
    hold.entered();
    await hold.gate;
  }
  if (res.destroyed) return;
  res.writeHead(status, { "content-type": "application/json" });
  res.end(JSON.stringify(snapshot));
}

async function requestBody(req) {
  let text = "";
  for await (const chunk of req) {
    text += chunk;
    assert.ok(text.length <= 1024 * 1024, "bounded test request");
  }
  return text ? JSON.parse(text) : {};
}

async function fixture(req, res) {
  const url = new URL(req.url, "http://localhost");
  if (!url.pathname.startsWith("/api/")) {
    const parts = decodeURIComponent(url.pathname).split("/").filter(Boolean);
    assert.ok(!parts.includes(".."), "static path must stay in the built UI");
    const file = path.join(dist, ...(parts.length ? parts : ["index.html"]));
    const types = { ".html": "text/html", ".js": "application/javascript", ".css": "text/css", ".png": "image/png" };
    const content = await readFile(file);
    res.writeHead(200, { "content-type": types[path.extname(file)] || "application/octet-stream" });
    res.end(content);
    return;
  }
  const body = await requestBody(req);
  requests.push({ method: req.method, path: url.pathname, body });
  if (url.pathname === "/api/auth/token") {
    assert.equal(req.headers.authorization, `Bootstrap ${bootstrap}`);
    return reply(req, res, { access_token: accessToken, token_type: "Bearer", expires_in: 3600 });
  }
  assert.equal(req.headers.authorization, `Bearer ${accessToken}`, "every adapter call must be authenticated");
  assert.ok(!Object.hasOwn(body, "owner_uid"), "Web must not submit owner identity");
  if (url.pathname === "/api/meta") {
    return reply(req, res, { hostname: "activity-test", provider: "test", model: "fixture", ready: true });
  }
  if (url.pathname === "/api/notifications/stream") {
    res.writeHead(200, { "content-type": "text/event-stream", "cache-control": "no-cache" });
    res.write(": ready\n\n");
    const ping = setInterval(() => res.write(": ping\n\n"), 1000);
    res.on("close", () => clearInterval(ping));
    return;
  }
  if (url.pathname === "/api/inbox") return reply(req, res, { cursor: 0, notifications: [] });
  if (url.pathname === "/api/notifications/preferences") {
    return reply(req, res, { web_enabled: false, desktop_enabled: false, ntfy_enabled: false });
  }
  if (url.pathname === "/api/notifications/delivery/claim") return reply(req, res, { deliveries: [] });
  const sessionIds = [...new Set([...jobs.values()].map((job) => job.session_id).filter(Boolean))];
  if (url.pathname === "/api/sessions") {
    return reply(req, res, { sessions: sessionIds.map((id) => ({ id, title: `Session ${id}` })) });
  }
  if (/^\/api\/sessions\/[^/]+\/history$/.test(url.pathname)) {
    return reply(req, res, {
      messages: [{ id: 1, role: "assistant", text: "Saved Activity session" }],
    });
  }
  if (url.pathname === "/api/tasks") return reply(req, res, { tasks: [...jobs.values()] });
  if (/^\/api\/tasks\/[^/]+\/stop$/.test(url.pathname)) {
    const job = jobs.get(url.pathname.split("/")[3]);
    assert.ok(job, "cancel addresses an existing task");
    Object.assign(job, { status: "cancelled", finished_at: timestamp() });
    return reply(req, res, { cancelled: true });
  }
  if (url.pathname === "/api/approvals/pending") {
    return reply(req, res, { requests: [...jobs.values()]
      .filter((job) => job.status === "waiting_approval")
      .flatMap((job) => job.waiting_on.map((id) => ({
        id, verb: "fs.read", reason: "Review the release notes", requested_at: timestamp(),
      }))) });
  }
  if (url.pathname === "/api/approvals/recent") return reply(req, res, { entries: [] });
  if (/^\/api\/approvals\/[^/]+\/approve$/.test(url.pathname)) {
    const id = url.pathname.split("/")[3];
    assert.equal(body.duration, "once");
    for (const job of jobs.values()) {
      if (job.waiting_on.includes(id)) Object.assign(job, {
        status: "ok", waiting_on: [], finished_at: timestamp(), response: "Release draft reviewed.",
      });
    }
    return reply(req, res, { approved: true });
  }
  if (url.pathname === "/api/activities") {
    if (req.method === "GET") {
      assert.equal(url.searchParams.get("limit"), "100");
      const state = url.searchParams.get("state");
      return reply(req, res, {
        schema: 1, activities: [...activities.values()].filter((item) => !state || item.state === state),
      });
    }
    assert.equal(req.method, "POST");
    assert.ok(body.title.trim() && body.goal.trim(), "creating a goal requires title and goal");
    const item = {
      id: `activity-${++activityNumber}`, owner_uid: 1000,
      title: body.title, goal: body.goal, completion_criteria: body.completion_criteria || "",
      boundaries: body.boundaries || "", resources: body.resources || [],
      state: "active", completion_note: null, created_at: timestamp(), updated_at: timestamp(),
    };
    activities.set(item.id, item);
    return reply(req, res, item);
  }
  const match = /^\/api\/activities\/([^/]+)(?:\/(update|transition|run|objects))?$/.exec(url.pathname);
  if (match) {
    const item = activities.get(decodeURIComponent(match[1]));
    assert.ok(item, "the requested Activity exists");
    const action = match[2];
    if (action === "objects") {
      if (req.method === "GET") {
        if (invalidObjectsOnce?.id === item.id) {
          const invalid = invalidObjectsOnce;
          invalidObjectsOnce = null;
          return reply(req, res, { schema: 1, activity_id: item.id, objects: [invalid.entry] });
        }
        return reply(req, res, {
          schema: 1, activity_id: item.id,
          objects: item.resources.filter((resource) => objectMetadata.has(resource.reference))
            .map((resource) => ({ ...resource, ...objectMetadata.get(resource.reference) })),
        });
      }
      assert.equal(req.method, "POST");
      assert.deepEqual(Object.keys(body).sort(), ["label", "object"], "attachments carry no URI, resources, or owner");
      const description = Object.hasOwn(body.object, "revision") ? objectDescription : unpinnedDescription;
      assert.deepEqual(body.object, description.object, "opaque identity and optional revision are sent unchanged");
      assert.ok(item.state === "active" || item.state === "paused");
      const resource = { label: body.label, reference: description.reference };
      const index = item.resources.findIndex((entry) => entry.reference === resource.reference);
      if (index < 0) item.resources.push(resource);
      else item.resources[index] = resource;
      item.updated_at = timestamp();
      return reply(req, res, item);
    }
    if (!action) {
      assert.equal(req.method, "GET");
      if (invalidDetailOnce === item.id) {
        invalidDetailOnce = null;
        return reply(req, res, { schema: 1, error: "Invalid upstream projection" });
      }
      const associated = [...jobs.values()].filter((job) => job.activity_id === item.id);
      return reply(req, res, {
        schema: 1, activity: item, jobs: associated,
        sessions: [...new Set(associated.map((job) => job.session_id).filter(Boolean))],
      });
    }
    assert.equal(req.method, "POST");
    if (action === "update") {
      assert.ok(item.state === "active" || item.state === "paused");
      for (const key of ["title", "goal", "completion_criteria", "boundaries", "resources"]) {
        if (Object.hasOwn(body, key)) item[key] = body[key];
      }
    } else if (action === "transition") {
      assert.ok(["active", "paused", "completed", "cancelled"].includes(body.state));
      if (body.state === "completed") assert.ok(body.completion_note?.trim(), "completion is explicit");
      item.state = body.state;
      item.completion_note = body.state === "completed" ? body.completion_note : null;
    } else {
      assert.equal(item.state, "active", "paused and terminal goals cannot submit work");
      const number = ++jobNumber;
      const job = {
        id: `job-${number}`, activity_id: item.id,
        title: body.prompt || item.title, status: "pending",
        session_id: body.session_id || `session-${number}`,
        created_at: timestamp(), finished_at: null, response: null, error: null, waiting_on: [],
      };
      jobs.set(job.id, job);
      return reply(req, res, job);
    }
    item.updated_at = timestamp();
    return reply(req, res, item);
  }
  throw new Error(`Unimplemented fixture request: ${req.method} ${url.pathname}`);
}

const server = createServer((req, res) => {
  void fixture(req, res).catch((error) => {
    fixtureErrors.push(error.message);
    if (!res.destroyed && !res.headersSent) {
      res.writeHead(500, { "content-type": "application/json" });
      res.end(JSON.stringify({ error: error.message }));
    }
  });
});

async function installedBrowser() {
  if (process.env.ACTIVITY_BROWSER) {
    await access(process.env.ACTIVITY_BROWSER);
    return process.env.ACTIVITY_BROWSER;
  }
  if (process.platform === "win32") {
    for (const candidate of [
      "C:\\Program Files\\Google\\Chrome\\Application\\chrome.exe",
      "C:\\Program Files (x86)\\Microsoft\\Edge\\Application\\msedge.exe",
    ]) {
      try { await access(candidate); return candidate; } catch (error) {
        if (error.code !== "ENOENT") throw error;
      }
    }
  } else {
    for (const name of ["chromium", "chromium-browser", "google-chrome"]) {
      const result = spawnSync("which", [name], { encoding: "utf8" });
      if (result.status === 0) return result.stdout.trim();
    }
  }
  throw new Error("No installed Chromium browser found. Set ACTIVITY_BROWSER to its executable.");
}

async function connect(endpoint) {
  const socket = new WebSocket(endpoint);
  await once(socket, "open");
  let nextId = 0;
  const pending = new Map();
  const listeners = [];
  socket.addEventListener("message", ({ data }) => {
    const message = JSON.parse(data);
    if (message.id) {
      const item = pending.get(message.id);
      if (!item) return;
      pending.delete(message.id);
      clearTimeout(item.timeout);
      if (message.error) item.reject(new Error(message.error.message));
      else item.resolve(message.result);
    } else {
      for (const listen of listeners) listen(message);
    }
  });
  return {
    on: (listener) => listeners.push(listener),
    send: (method, params = {}, sessionId) => new Promise((resolve, reject) => {
      const id = ++nextId;
      const timeout = setTimeout(() => {
        pending.delete(id);
        reject(new Error(`CDP command timed out: ${method}`));
      }, 15000);
      pending.set(id, { resolve, reject, timeout });
      socket.send(JSON.stringify({ id, method, params, ...(sessionId ? { sessionId } : {}) }));
    }),
    close: () => socket.close(),
  };
}

try {
  await access(path.join(dist, "index.html"));
  await mkdir(path.join(profile, "runtime"), { recursive: true });
  await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
  const origin = `http://127.0.0.1:${server.address().port}`;
  assert.equal((await fetch(origin)).status, 200, "the isolated build is served");
  const executable = await installedBrowser();
  browser = spawn(executable, [
    "--headless=new", "--disable-gpu", "--no-first-run", "--no-default-browser-check",
    "--disable-background-networking", "--disable-component-update", "--disable-breakpad",
    "--remote-debugging-port=0", "--window-size=1440,1100",
    `--user-data-dir=${profile}`, `--disk-cache-dir=${path.join(profile, "cache")}`,
    "about:blank",
  ], {
    stdio: ["ignore", "ignore", "pipe"],
    env: { ...process.env, TMP: path.join(profile, "runtime"), TEMP: path.join(profile, "runtime"), TMPDIR: path.join(profile, "runtime") },
  });
  const endpoint = await new Promise((resolve, reject) => {
    let output = "";
    const timeout = setTimeout(() => reject(new Error(`Browser did not start: ${output.slice(-1000)}`)), 15000);
    browser.on("error", (error) => { clearTimeout(timeout); reject(error); });
    browser.stderr.on("data", (chunk) => {
      output += chunk;
      const match = /DevTools listening on (ws:\/\/[^\s]+)/.exec(output);
      if (match) { clearTimeout(timeout); resolve(match[1]); }
    });
  });
  cdp = await connect(endpoint);
  const { targetId } = await cdp.send("Target.createTarget", { url: "about:blank" });
  const { sessionId } = await cdp.send("Target.attachToTarget", { targetId, flatten: true });
  const send = (method, params) => cdp.send(method, params, sessionId);
  cdp.on((event) => {
    if (event.sessionId !== sessionId) return;
    if (event.method === "Runtime.exceptionThrown") browserErrors.push(JSON.stringify(event.params.exceptionDetails));
    if (event.method === "Runtime.consoleAPICalled" && event.params.type === "error") {
      browserErrors.push(event.params.args.map((value) => value.description || value.value).join(" "));
    }
    if (event.method === "Log.entryAdded" && event.params.entry.level === "error") {
      browserErrors.push(event.params.entry.text);
    }
    if (event.method === "Network.requestWillBeSent") {
      const url = event.params.request.url;
      if (!url.startsWith(origin) && !url.startsWith("data:") && url !== "about:blank") {
        browserErrors.push(`Unexpected outbound request: ${url}`);
      }
    }
  });
  await Promise.all(["Runtime.enable", "Page.enable", "Log.enable", "Network.enable"].map((method) => send(method)));
  const evaluate = async (expression) => {
    const result = await send("Runtime.evaluate", { expression, returnByValue: true, awaitPromise: true });
    if (result.exceptionDetails) throw new Error(JSON.stringify(result.exceptionDetails));
    return result.result.value;
  };
  const wait = async (expression, label, timeout = 12000) => {
    const deadline = Date.now() + timeout;
    while (Date.now() < deadline) {
      if (await evaluate(expression)) return;
      await delay(100);
    }
    throw new Error(`UI did not complete: ${label}\n${await evaluate("document.body.innerText")}`);
  };
  const buttonExpression = (text) =>
    `Array.from(document.querySelectorAll('button')).find(el => el.textContent.trim() === ${JSON.stringify(text)})`;
  const click = async (expression) => {
    await wait(`(() => { const el = ${expression}; return el && !el.matches(':disabled'); })()`, "button enabled");
    const point = await evaluate(`(() => {
      const el = ${expression}; el.scrollIntoView({block: 'center'});
      const rect = el.getBoundingClientRect();
      return {x: rect.x + rect.width / 2, y: rect.y + rect.height / 2};
    })()`);
    await send("Input.dispatchMouseEvent", { type: "mousePressed", button: "left", clickCount: 1, ...point });
    await send("Input.dispatchMouseEvent", { type: "mouseReleased", button: "left", clickCount: 1, ...point });
  };
  const clickText = (text) => click(buttonExpression(text));
  const clickLabel = (label) => click(`document.querySelector('[aria-label=${JSON.stringify(label)}]')`);
  const fieldExpression = (label) => `Array.from(document.querySelectorAll('label')).find(el =>
    Array.from(el.childNodes).filter(node => node.nodeType === 3).map(node => node.textContent).join('').trim() === ${JSON.stringify(label)}
  )?.querySelector('input,textarea,select')`;
  const fill = async (label, value) => {
    await wait(`(() => { const el = ${fieldExpression(label)}; return el && !el.matches(':disabled'); })()`, `editable ${label}`);
    await evaluate(`(() => {
      const el = ${fieldExpression(label)};
      if (!el || el.matches(':disabled')) throw new Error('Field missing or disabled: ' + ${JSON.stringify(label)});
      const prototype = el.tagName === 'TEXTAREA' ? HTMLTextAreaElement.prototype
        : el.tagName === 'SELECT' ? HTMLSelectElement.prototype : HTMLInputElement.prototype;
      Object.getOwnPropertyDescriptor(prototype, 'value').set.call(el, ${JSON.stringify(value)});
      el.dispatchEvent(new Event(el.tagName === 'SELECT' ? 'change' : 'input', {bubbles: true}));
    })()`);
  };
  const detailText = `document.querySelector('[aria-label="Activity detail"]')?.innerText || ''`;
  const expectDetail = (text) => wait(`(${detailText}).includes(${JSON.stringify(text)})`, text);
  const objectPanel = `document.querySelector('[aria-label="App object references"]')`;
  const expectObjects = (text) => wait(`(${objectPanel}?.innerText || '').includes(${JSON.stringify(text)})`, text);
  const fillObject = async (label) => {
    await fill("Reference label", label);
    await fill("App ID", objectDescription.object.app_id);
    await fill("Object type", objectDescription.object.object_type);
    await fill("Opaque object ID", objectDescription.object.object_id);
    await fill("Revision (optional)", objectDescription.object.revision);
  };
  const open = async (title) => {
    await clickLabel(`Open activity: ${title}`);
    await wait(`document.querySelector('[aria-label="Activity detail"] h2')?.textContent === ${JSON.stringify(title)}`, title);
  };
  const reload = async () => {
    await send("Page.reload");
    await wait(`!!document.querySelector('[aria-label="Activity detail"] h2')`, "restored detail after reload");
  };

  await send("Page.navigate", { url: `${origin}/?t=${bootstrap}#/activities` });
  await wait(`document.body.innerText.includes('No activities yet.')`, "authenticated empty list");
  await clickText("New activity");
  await fill("Title", "Release preparation");
  await fill("Goal", "Prepare a reviewed release draft");
  await fill("Completion criteria", "I verified the release draft");
  await fill("Boundaries", "Do not publish without approval");
  await clickText("Add resource");
  await fill("Resource 1 label", "Reference only");
  await fill("Resource 1 reference", "javascript:window.activityReferenceExecuted=true");
  await clickText("Create activity");
  await expectDetail("Prepare a reviewed release draft");
  assert.equal(activities.size, 1);
  assert.equal(jobs.size, 0, "creating an Activity does not start work");
  assert.equal(await evaluate("window.activityReferenceExecuted"), undefined);
  assert.equal(await evaluate(`document.querySelector('[aria-label="Activity detail"] a[href^="javascript:"]') !== null`), false);
  await reload();
  await expectDetail("I verified the release draft");
  assert.equal(await evaluate("location.hash"), "#/activities/activity-1");
  console.log("PASS authenticated create/list/detail and reload persistence");

  await expectObjects("No App object references.");
  await fillObject("Draft object");
  await clickText("Edit activity");
  assert.equal(await evaluate(`(${fieldExpression("Reference label")}).matches(':disabled')`), true);
  await clickText("Cancel editing");
  const concurrentResource = { label: "From another client", reference: "notes:keep-this" };
  activities.get("activity-1").resources.push(concurrentResource);
  await clickText("Attach object reference");
  await expectObjects("Draft object");
  await expectObjects(objectDescription.reference);
  assert.equal(jobs.size, 0, "attaching an object does not execute work");
  assert.deepEqual(activities.get("activity-1").resources, [
    { label: "Reference only", reference: "javascript:window.activityReferenceExecuted=true" },
    concurrentResource, { label: "Draft object", reference: objectDescription.reference },
  ]);
  await fillObject("Release status");
  await clickText("Attach object reference");
  await expectObjects("Release status");
  assert.equal(activities.get("activity-1").resources.filter((entry) => entry.reference === objectDescription.reference).length, 1);
  await reload();
  await expectObjects("Declared (manifest only)");
  await expectObjects("not object existence");
  await expectObjects(objectDescription.object_summary);
  await click(`${objectPanel}.querySelector('summary')`);
  await expectObjects("Structured arguments only");
  assert.deepEqual(await evaluate(`JSON.parse(${objectPanel}.querySelector('pre').textContent)`), objectDescription.invocation);
  assert.equal(await evaluate(`${objectPanel}.querySelectorAll('a,script,img').length`), 0);
  assert.equal(await evaluate("window.objectMetadataExecuted"), undefined);
  await click(`${objectPanel}.querySelector('code')`);
  assert.equal(await evaluate("location.hash"), "#/activities/activity-1");
  objectMetadata.set(objectDescription.reference, {
    status: "unavailable", description: null, error: "App package is quarantined; review its publisher trust.",
  });
  await clickLabel("Refresh object descriptions");
  await expectObjects("Unavailable");
  await expectObjects("App package is quarantined; review its publisher trust.");
  assert.equal(await evaluate(`(${objectPanel}.innerText).includes('Archived entry')`), false);
  assert.ok(activities.get("activity-1").resources.some((entry) => entry.reference === objectDescription.reference));
  objectMetadata.set("app:invalid", { status: "invalid", description: null, error: "Stored reference is not canonical." });
  activities.get("activity-1").resources.push({ label: "Invalid stored reference", reference: "app:invalid" });
  await clickLabel("Refresh object descriptions");
  await expectObjects("Invalid reference");
  await expectObjects("Stored reference is not canonical.");
  objectMetadata.set(objectDescription.reference, declaredMetadata);
  for (const entry of [
    { label: "Untrusted metadata", reference: objectDescription.reference, status: "unavailable", description: objectDescription, error: "Revoked" },
    { label: "Malformed metadata", reference: objectDescription.reference, status: "declared",
      description: { ...objectDescription, invocation: { ...objectDescription.invocation, args: "not structured argv" } }, error: null },
  ]) {
    invalidObjectsOnce = { id: "activity-1", entry };
    await clickLabel("Refresh object descriptions");
    await expectObjects("Invalid App object descriptions from the server.");
    assert.equal(await evaluate(`${objectPanel}.querySelectorAll('[aria-label^="Object reference:"]').length`), 0);
    await clickLabel("Refresh object descriptions");
    await expectObjects("Declared (manifest only)");
  }
  assert.equal(jobs.size, 0);
  assert.equal(requests.some((request) => /\/(?:apps|resolve)(?:\/|$)/.test(request.path)), false);
  console.log("PASS typed object attachment, canonical backend upsert, declaration diagnostics and non-execution");

  await fill("Work instructions (optional)", "Review the release draft");
  await clickText("Submit work");
  await expectDetail("Queued");
  assert.equal(jobs.size, 1);
  const firstJob = jobs.get("job-1");
  firstJob.status = "running";
  await expectDetail("Running");
  Object.assign(firstJob, { status: "waiting_approval", waiting_on: ["approval-1"] });
  await expectDetail("Waiting for your approval decision.");
  await clickText("Open Approvals");
  await wait("location.hash === '#/approvals' && document.body.innerText.includes('Review the release notes')", "existing approvals page");
  await clickText("Approve");
  await wait("document.body.innerText.includes('No pending approvals.')", "existing approval action");
  assert.equal(firstJob.status, "ok");
  assert.equal(activities.get("activity-1").state, "active", "successful jobs never auto-complete a goal");
  await clickText("Activities");
  await open("Release preparation");
  await expectDetail("Release draft reviewed.");
  await clickText("Open session");
  await wait("location.hash === '#/chat/session-1' && document.body.innerText.includes('Saved Activity session')", "existing session history");
  await clickText("Activities");
  await open("Release preparation");
  console.log("PASS durable submission, live polling, existing approval and session routes");

  await fill("Work session", "session-1");
  await fill("Work instructions (optional)", "Continue the release review");
  await clickText("Continue work");
  await expectDetail("Continue the release review");
  assert.equal(jobs.get("job-2").session_id, "session-1");
  assert.equal(requests.filter((request) => request.path.endsWith("/run")).at(-1).body.session_id, "session-1");
  await clickText("Open Tasks");
  await wait("location.hash === '#/tasks' && document.body.innerText.includes('Continue the release review')", "existing tasks page");
  await click(`document.querySelector('button[title="Cancel task"]')`);
  await wait("document.body.innerText.includes('cancelled')", "existing task cancellation");
  assert.equal(jobs.get("job-2").status, "cancelled");
  assert.equal(activities.get("activity-1").state, "active");
  await clickText("Activities");
  await open("Release preparation");

  await clickText("Edit activity");
  await fill("Goal", "Prepare a reviewed and signed draft");
  await fill("Boundaries", "Planning guidance is not permission");
  await clickText("Save changes");
  await expectDetail("Prepare a reviewed and signed draft");
  assert.equal(activities.get("activity-1").boundaries, "Planning guidance is not permission");
  const slowObjects = holdRequest("GET", "/api/activities/activity-1/objects");
  await clickText("Pause activity");
  await slowObjects.seen;
  await expectDetail("Activity paused.");
  await wait(`!(${buttonExpression("Edit activity")}).matches(':disabled')`, "state controls do not wait for object metadata");
  slowObjects.release();
  await reload();
  await expectDetail("Resume activity");
  assert.equal(await evaluate(`(${buttonExpression("Submit work")}).matches(':disabled')`), true);
  await clickText("Resume activity");
  await expectDetail("Activity resumed.");
  await clickText("Mark completed");
  await fill("Completion note", "   ");
  assert.equal(await evaluate(`(${buttonExpression("Confirm completion")}).matches(':disabled')`), true);
  await fill("Completion note", "I reviewed the signed draft against the criteria.");
  await clickText("Confirm completion");
  await expectDetail("Completion confirmation");
  assert.equal(activities.get("activity-1").state, "completed");
  await reload();
  await expectDetail("I reviewed the signed draft against the criteria.");
  assert.equal(await evaluate(`(${fieldExpression("Reference label")}).matches(':disabled')`), true);
  await clickText("Reopen activity");
  await expectDetail("Activity explicitly reopened.");
  await clickText("Cancel activity");
  await expectDetail("Activity cancelled.");
  await reload();
  await expectDetail("Reopen activity");
  assert.equal(activities.get("activity-1").state, "cancelled");
  await clickText("Reopen activity");
  await expectDetail("Activity explicitly reopened.");
  console.log("PASS continuation, task cancellation, edit, pause/resume, explicit complete/reopen/cancel");

  await clickText("New activity");
  await fill("Title", "Second goal");
  await fill("Goal", "Keep this selection stable");
  await clickText("Create activity");
  await expectDetail("Keep this selection stable");
  invalidDetailOnce = "activity-2";
  await clickLabel("Refresh activity detail");
  await expectDetail("Invalid Activity detail from the server.");
  assert.equal(await evaluate(`(${buttonExpression("Edit activity")}).matches(':disabled')`), true);
  await clickLabel("Refresh activity detail");
  await wait(`!document.querySelector('[aria-label="Activity detail"] [role="alert"]')`, "read failure cleared by a successful refresh");
  await expectDetail("Keep this selection stable");
  await clickText("Activities");
  const oldRead = holdRequest("GET", "/api/activities/activity-1");
  await clickLabel("Open activity: Release preparation");
  await oldRead.seen;
  await open("Second goal");
  oldRead.release();
  await delay(300);
  assert.equal(await evaluate("location.hash"), "#/activities/activity-2");
  await expectDetail("Keep this selection stable");

  const oldObjects = holdRequest("GET", "/api/activities/activity-1/objects");
  await open("Release preparation");
  await oldObjects.seen;
  await open("Second goal");
  await expectObjects("No App object references.");
  oldObjects.release();
  await delay(300);
  assert.equal(await evaluate(`${objectPanel}.querySelectorAll('[aria-label^="Object reference:"]').length`), 0);
  assert.equal(await evaluate("location.hash"), "#/activities/activity-2");
  await open("Release preparation");
  await fillObject("Attached while looking elsewhere");
  await fill("Revision (optional)", "");
  const oldAttach = holdRequest("POST", "/api/activities/activity-1/objects");
  await clickText("Attach object reference");
  await oldAttach.seen;
  assert.equal(Object.hasOwn(requests.filter((request) => request.method === "POST" && request.path.endsWith("/objects")).at(-1).body.object, "revision"), false);
  await open("Second goal");
  await fill("Reference label", "Second Activity draft");
  oldAttach.release();
  await delay(300);
  assert.equal(await evaluate("location.hash"), "#/activities/activity-2");
  assert.equal(await evaluate(`(${fieldExpression("Reference label")}).value`), "Second Activity draft");
  assert.equal(await evaluate(`(${detailText}).includes('Object reference attached.')`), false);
  assert.equal(activities.get("activity-2").resources.length, 0);
  assert.ok(activities.get("activity-1").resources.some((entry) => entry.reference === unpinnedDescription.reference));
  console.log("PASS late object descriptions and attachments cannot overwrite a newer selection or draft");

  await open("Release preparation");
  await clickText("Edit activity");
  await fill("Goal", "Saved while another Activity is selected");
  const oldUpdate = holdRequest("POST", "/api/activities/activity-1/update");
  await clickText("Save changes");
  await oldUpdate.seen;
  await open("Second goal");
  oldUpdate.release();
  await delay(300);
  assert.equal(await evaluate("location.hash"), "#/activities/activity-2");
  assert.equal(await evaluate(`(${detailText}).includes('Activity saved.')`), false);

  await clickText("New activity");
  await fill("Title", "Background create");
  await fill("Goal", "Save without replacing a newer selection");
  const oldCreate = holdRequest("POST", "/api/activities");
  await clickText("Create activity");
  await oldCreate.seen;
  await open("Second goal");
  oldCreate.release();
  await delay(300);
  assert.equal(await evaluate("location.hash"), "#/activities/activity-2");
  assert.equal(activities.size, 3);
  await clickLabel("Refresh activity list");
  await wait(`!!document.querySelector('[aria-label="Open activity: Background create"]')`, "saved background creation in refreshed list");
  const filter = async (value) => evaluate(`(() => {
    const el = document.querySelector('[aria-label="Filter activities by state"]');
    Object.getOwnPropertyDescriptor(HTMLSelectElement.prototype, 'value').set.call(el, ${JSON.stringify(value)});
    el.dispatchEvent(new Event('change', {bubbles: true}));
  })()`);
  await filter("paused");
  await wait("document.body.innerText.includes('No activities in this state.')", "owner-backed state filter");
  await filter("");
  await wait(`document.querySelectorAll('[aria-label="Activity list"] [aria-label^="Open activity:"]').length === 3`, "all persisted records refetched");
  assert.deepEqual(await evaluate("Object.keys(localStorage).filter(key => /activit/i.test(key))"), []);
  assert.deepEqual(fixtureErrors, []);
  assert.deepEqual(browserErrors, []);
  console.log("PASS visible read errors, filters, and selection-safe late reads/mutations/creation");
  console.log(`PASS Activities browser regression (${requests.length} authenticated API interactions, no console errors)`);
} finally {
  if (cdp) {
    await cdp.send("Browser.close").catch(() => {});
    cdp.close();
  }
  if (browser && browser.exitCode === null) {
    await Promise.race([once(browser, "exit"), delay(5000)]);
    if (browser.exitCode === null) browser.kill();
  }
  server.closeAllConnections();
  server.close();
  await rm(profile, { recursive: true, force: true, maxRetries: 10, retryDelay: 200 });
}
