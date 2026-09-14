import { afterEach, beforeEach, expect, mock, spyOn, test } from "bun:test";
import { JSDOM } from "jsdom";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";

import { ActivityContinuityPanel } from "../src/components/activity-continuity";
import { api } from "../src/lib/api";
import {
  MAX_CONTINUITY_DOCUMENT_BYTES,
  continuityApi,
  continuityFilename,
  continuityJson,
  parseContinuityFile,
  readContinuityImport,
  validateContinuityDocument,
  type ContinuityDocument,
} from "../src/lib/activity-continuity";

const encoder = new TextEncoder();

async function sha256(value: string) {
  const bytes = await crypto.subtle.digest("SHA-256", encoder.encode(value));
  return `sha256:${Array.from(new Uint8Array(bytes))
    .map((byte) => byte.toString(16).padStart(2, "0")).join("")}`;
}

async function fixture(title = '<img src="https://continuity.invalid/x" onerror="window.continuityExecuted=true">') {
  const material = `{"kind":"claw_os.activity_continuity","schema_version":1,`
    + `"lineage":{"id":"00000000-0000-4000-8000-000000000123","revision":9007199254740993},`
    + `"intent":{"title":${JSON.stringify(title)},"goal":"Publish the release",`
    + `"completion_criteria":"Reviewed and available","boundaries":"Ask before publishing"},`
    + `"references":[{"label":"Status","reference":"app://kv/entry?id=release.status&revision=v1"}],`
    + `"rules":{"execution_limits":{"enabled":false,"max_attempts":10,`
    + `"max_turns_per_attempt":5,"expires_at":"2030-01-01T00:00:00.000000000Z"},`
    + `"scheduling":{"priority":"foreground"}}}`;
  const snapshot = await sha256(material);
  return `${material.slice(0, material.indexOf('"intent"'))}`
    + `"snapshot":"${snapshot}",${material.slice(material.indexOf('"intent"'))}`;
}

test("continuity parser preserves large lineage revisions and emits deterministic canonical JSON", async () => {
  const document = await parseContinuityFile(await fixture("Release"));
  expect(document.lineage.revision).toBe("9007199254740993");
  expect(continuityFilename(document)).toBe(
    "claw-os-activity-00000000-0000-4000-8000-000000000123.json",
  );
  expect(continuityJson(document)).toBe(await fixture("Release"));
  expect(continuityJson(document)).not.toContain('"revision":"9007199254740993"');
  expect(await validateContinuityDocument(structuredClone(document))).toEqual(document);
});

test("continuity parser rejects duplicate, version, authority, identity and envelope violations", async () => {
  const valid = await fixture("Release");
  const cases: Array<[string, string]> = [
    ["Duplicate", valid.replace(
      '"kind":"claw_os.activity_continuity"',
      '"kind":"claw_os.activity_continuity","kind":"claw_os.activity_continuity"',
    )],
    ["schema version", valid.replace('"schema_version":1', '"schema_version":2')],
    ["kind", valid.replace("claw_os.activity_continuity", "other")],
    ["unknown fields", valid.replace(/}$/, ',"owner_uid":1000}')],
    ["unknown fields", valid.replace(
      '"title":"Release"',
      '"title":"Release","capability_policy":{"grant":"all"}',
    )],
    ["canonical UUID", valid.replace(
      "00000000-0000-4000-8000-000000000123",
      "00000000-0000-4000-8000-00000000012Z",
    )],
    ["canonical app://", valid.replace(
      "app://kv/entry?id=release.status&revision=v1",
      "/home/user/private.json",
    )],
    ["snapshot digest", valid.replace("Publish the release", "Publish something else")],
    ["too deeply nested", `${"[".repeat(9)}0${"]".repeat(9)}`],
    ["too many containers", `[${Array.from({ length: 128 }, () => "[]").join(",")}]`],
    ["JSON string exceeds", `{"x":"${"a".repeat(128 * 1024 + 1)}"}`],
    ["document exceeds", " ".repeat(MAX_CONTINUITY_DOCUMENT_BYTES + 1)],
  ];
  for (const [message, source] of cases) {
    await expect(parseContinuityFile(source)).rejects.toThrow(message);
  }
});

test("continuity import request is owner-free and exact acknowledgement creates a new paused Activity", async () => {
  const document = await parseContinuityFile(await fixture("Release"));
  const acknowledgement = {
    activity: {
      id: "00000000-0000-4000-8000-000000000999",
      owner_uid: 1000,
      title: document.intent.title,
      goal: document.intent.goal,
      completion_criteria: document.intent.completion_criteria,
      boundaries: document.intent.boundaries,
      resources: document.references,
      state: "paused",
      completion_note: null,
      created_at: "2026-09-14T00:00:00Z",
      updated_at: "2026-09-14T00:00:00Z",
    },
    continuity_id: document.lineage.id,
    continuity_revision: document.lineage.revision,
    placement: "local",
  } as const;
  expect((await readContinuityImport(acknowledgement, document, new Set())).activity.state)
    .toBe("paused");
  for (const invalid of [
    { ...acknowledgement, placement: "remote" },
    { ...acknowledgement, continuity_revision: "9007199254740994" },
    { ...acknowledgement, authority: true },
    { ...acknowledgement, activity: { ...acknowledgement.activity, state: "active" } },
    { ...acknowledgement, activity: { ...acknowledgement.activity, owner_selector: 1000 } },
  ]) await expect(readContinuityImport(invalid, document, new Set())).rejects.toThrow();
  await expect(readContinuityImport(
    acknowledgement,
    document,
    new Set([acknowledgement.activity.id]),
  )).rejects.toThrow("new Activity");

  const post = spyOn(api, "post").mockResolvedValue(acknowledgement);
  expect((await continuityApi.import(document, new Set())).activity.id)
    .toBe(acknowledgement.activity.id);
  expect(post).toHaveBeenCalledWith("/api/activities/continuity/import", {
    placement: "local",
    document,
  }, { signal: undefined });
  const request = JSON.stringify(post.mock.calls);
  expect(request).not.toContain("owner_uid");
  expect(request).not.toContain("server_path");
  expect(request).not.toContain("device");
});

let dom: JSDOM;
let root: Root;
let container: HTMLElement;
let previous: PropertyDescriptorMap;

beforeEach(() => {
  previous = Object.getOwnPropertyDescriptors(globalThis);
  dom = new JSDOM('<div id="root"></div>', { url: "http://localhost/" });
  Object.defineProperties(globalThis, {
    window: { configurable: true, value: dom.window },
    document: { configurable: true, value: dom.window.document },
    IS_REACT_ACT_ENVIRONMENT: { configurable: true, value: true },
  });
  container = dom.window.document.getElementById("root")!;
  root = createRoot(container);
});

afterEach(async () => {
  if (root) await act(async () => root.unmount());
  if (dom) dom.window.close();
  for (const key of ["window", "document", "IS_REACT_ACT_ENVIRONMENT"]) {
    if (previous?.[key]) Object.defineProperty(globalThis, key, previous[key]);
    else Reflect.deleteProperty(globalThis, key);
  }
  mock.restore();
});

function button(text: string) {
  const found = Array.from(container.querySelectorAll("button"))
    .find((item) => item.textContent?.trim() === text);
  if (!found) throw new Error(`Missing button: ${text}`);
  return found as HTMLButtonElement;
}

test("continuity component requires selected export and explicit local placement confirmation", async () => {
  await act(async () => root.render(
    <ActivityContinuityPanel selectedId={null} existingIds={new Set()}
      onImported={async () => {}} onSelectImported={() => {}} />,
  ));
  expect(button("Export selected Activity").disabled).toBe(true);
  expect(button("Import paused Activity").disabled).toBe(true);
  expect(container.querySelector<HTMLSelectElement>('[aria-label="Execution placement"]')?.value)
    .toBe("");
  expect(container.textContent).toContain("not live sync or backup/restore");
  expect(container.textContent).toContain("capability policy, monetary settings");
  expect(container.textContent).toContain("audit evidence, and execution proof do not migrate");
  expect(container.textContent).toContain("No server path is accepted or sent.");
});

test("continuity upload renders imported strings inertly and selects only after exact success", async () => {
  const source = await fixture();
  const document = await parseContinuityFile(source);
  const acknowledgement = {
    activity: {
      id: "00000000-0000-4000-8000-000000000999",
      owner_uid: 1000,
      ...document.intent,
      resources: document.references,
      state: "paused",
      completion_note: null,
      created_at: "2026-09-14T00:00:00Z",
      updated_at: "2026-09-14T00:00:00Z",
    },
    continuity_id: document.lineage.id,
    continuity_revision: document.lineage.revision,
    placement: "local",
  } as const;
  spyOn(continuityApi, "import").mockResolvedValue(acknowledgement);
  const selected: string[] = [];
  await act(async () => root.render(
    <ActivityContinuityPanel selectedId="source" existingIds={new Set(["source"])}
      onImported={async () => {}} onSelectImported={(id) => { selected.push(id); }} />,
  ));
  const input = container.querySelector<HTMLInputElement>('[aria-label="Continuity JSON file"]')!;
  Object.defineProperty(input, "files", {
    configurable: true,
    value: [{ name: "portable.json", size: encoder.encode(source).byteLength, text: async () => source }],
  });
  await act(async () => input.dispatchEvent(new dom.window.Event("change", { bubbles: true })));
  expect(container.textContent).toContain(document.intent.title);
  expect(container.querySelectorAll("img,script,a").length).toBe(0);
  expect((dom.window as unknown as { continuityExecuted?: boolean }).continuityExecuted).toBeUndefined();
  const placement = container.querySelector<HTMLSelectElement>('[aria-label="Execution placement"]')!;
  await act(async () => {
    placement.value = "local";
    placement.dispatchEvent(new dom.window.Event("change", { bubbles: true }));
  });
  expect(button("Import paused Activity").disabled).toBe(true);
  await act(async () => container.querySelector<HTMLInputElement>('input[type="checkbox"]')!.click());
  await act(async () => button("Import paused Activity").click());
  expect(selected).toEqual([acknowledgement.activity.id]);
});

test("late post-import refresh cannot replace a newer Activity selection or discard the import draft", async () => {
  const source = await fixture("Portable release");
  const document = await parseContinuityFile(source);
  const acknowledgement = {
    activity: {
      id: "00000000-0000-4000-8000-000000000999",
      owner_uid: 1000,
      ...document.intent,
      resources: document.references,
      state: "paused",
      completion_note: null,
      created_at: "2026-09-14T00:00:00Z",
      updated_at: "2026-09-14T00:00:00Z",
    },
    continuity_id: document.lineage.id,
    continuity_revision: document.lineage.revision,
    placement: "local",
  } as const;
  spyOn(continuityApi, "import").mockResolvedValue(acknowledgement);
  let resolveRefresh!: () => void;
  const refresh = mock(() => new Promise<void>((resolve) => { resolveRefresh = resolve; }));
  const selected: string[] = [];
  const selectImported = (id: string) => { selected.push(id); };

  await act(async () => root.render(
    <ActivityContinuityPanel selectedId="source" existingIds={new Set(["source"])}
      onImported={refresh} onSelectImported={selectImported} />,
  ));
  const input = container.querySelector<HTMLInputElement>('[aria-label="Continuity JSON file"]')!;
  Object.defineProperty(input, "files", {
    configurable: true,
    value: [{ name: "portable.json", size: encoder.encode(source).byteLength, text: async () => source }],
  });
  await act(async () => input.dispatchEvent(new dom.window.Event("change", { bubbles: true })));
  const placement = container.querySelector<HTMLSelectElement>('[aria-label="Execution placement"]')!;
  await act(async () => {
    placement.value = "local";
    placement.dispatchEvent(new dom.window.Event("change", { bubbles: true }));
  });
  await act(async () => container.querySelector<HTMLInputElement>('input[type="checkbox"]')!.click());
  await act(async () => {
    button("Import paused Activity").click();
    await Promise.resolve();
  });
  expect(refresh).toHaveBeenCalledTimes(1);

  await act(async () => root.render(
    <ActivityContinuityPanel selectedId="other" existingIds={new Set(["source", "other"])}
      onImported={refresh} onSelectImported={selectImported} />,
  ));
  expect(selected).toEqual([]);
  expect(container.textContent).toContain("Portable release");
  expect(placement.value).toBe("local");
  expect(container.querySelector<HTMLInputElement>('input[type="checkbox"]')?.checked).toBe(true);

  await act(async () => resolveRefresh());
  expect(selected).toEqual([]);
  expect(refresh).toHaveBeenCalledTimes(1);
  expect(container.textContent).toContain("Portable release");
  expect(button("Import paused Activity").disabled).toBe(false);
  expect(container.textContent).not.toContain("as a new paused Activity");
});

test("late export response cannot download after Activity selection changes", async () => {
  let resolve!: (value: ContinuityDocument) => void;
  const pending = new Promise<ContinuityDocument>((done) => { resolve = done; });
  spyOn(continuityApi, "export").mockReturnValue(pending);
  const createObjectURL = spyOn(URL, "createObjectURL").mockReturnValue("blob:continuity");
  await act(async () => root.render(
    <ActivityContinuityPanel selectedId="first" existingIds={new Set()}
      onImported={async () => {}} onSelectImported={() => {}} />,
  ));
  await act(async () => button("Export selected Activity").click());
  await act(async () => root.render(
    <ActivityContinuityPanel selectedId="second" existingIds={new Set()}
      onImported={async () => {}} onSelectImported={() => {}} />,
  ));
  await act(async () => resolve(await parseContinuityFile(await fixture("Release"))));
  expect(createObjectURL).not.toHaveBeenCalled();
});
