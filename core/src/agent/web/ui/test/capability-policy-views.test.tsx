import { afterEach, beforeEach, expect, mock, spyOn, test } from "bun:test";
import { JSDOM } from "jsdom";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";

import { ActivityCapabilityPolicyPanel } from "../src/components/activity-capability-policy";
import { useActivityCapabilityPolicy } from "../src/hooks/use-activities";
import { capabilityPolicyApi, type CapabilityPolicyData } from "../src/lib/capability-policy";
import { capabilityPolicyData, capabilityPolicyFixture } from "./capability-policy-fixtures";

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
  await act(async () => root.unmount());
  dom.window.close();
  for (const key of ["window", "document", "IS_REACT_ACT_ENVIRONMENT"]) {
    if (previous[key]) Object.defineProperty(globalThis, key, previous[key]);
    else Reflect.deleteProperty(globalThis, key);
  }
  mock.restore();
});

function Panel({ id = "activity-1", editable = true, disabled = false }: { id?: string; editable?: boolean; disabled?: boolean }) {
  const view = useActivityCapabilityPolicy(id);
  return <ActivityCapabilityPolicyPanel key={id} activityId={id} ownerUid={1000} editable={editable} disabled={disabled}
    view={view} mutate={async (action) => {
      try {
        await action();
        return (await view.refresh()) !== null;
      } catch {
        return false;
      }
    }} />;
}

function button(text: string): HTMLButtonElement {
  const element = Array.from(container.querySelectorAll("button")).find((item) => item.textContent?.trim() === text);
  if (!element) throw new Error(`Missing button: ${text}`);
  return element;
}

function select(label: string): HTMLSelectElement {
  const element = Array.from(container.querySelectorAll("label"))
    .find((item) => item.textContent?.trim().startsWith(label))?.querySelector("select");
  if (!element) throw new Error(`Missing select: ${label}`);
  return element;
}

async function choose(label: string, value: string) {
  await act(async () => {
    const field = select(label);
    field.value = value;
    field.dispatchEvent(new dom.window.Event("change", { bubbles: true }));
  });
}

test("saved rule rows are inert and distinguish capability constraints from permissions", async () => {
  const policy = capabilityPolicyFixture();
  policy.rules[1].scopes = [{ kind: "path", value: '/data/<img src="https://policy.invalid/private">/**' }];
  spyOn(capabilityPolicyApi, "get").mockResolvedValue(capabilityPolicyData(policy));
  const save = spyOn(capabilityPolicyApi, "set");
  await act(async () => root.render(<Panel />));
  expect(container.textContent).toContain("Normal keeps existing permission");
  expect(container.textContent).toContain("Ask requires exact, single-use approval");
  expect(container.textContent).toContain("once even if consent said session or forever");
  expect(container.textContent).toContain("immutable root-grant policy binding without asking again");
  expect(container.textContent).toContain("A first policy also invalidates existing attempts");
  expect(container.textContent).toContain("compromised same-UID Agent");
  expect(container.textContent).toContain("Deny cannot be overridden");
  expect(container.textContent).toContain("fs.write or proc.exec");
  expect(container.querySelectorAll("article")).toHaveLength(2);
  expect(container.querySelectorAll("a,img,script,textarea")).toHaveLength(0);
  expect(save).not.toHaveBeenCalled();
  await act(async () => button("Edit capability policy").click());
  expect(select("Rule 2 capability").value).toBe("fs.read");
  expect(Array.from(select("Rule 2 capability").options).find((option) => option.value === "fs.delete")?.disabled).toBe(true);
  expect(select("Rule 2 mode").value).toBe("normal");
});

test("failed and stale saves retain drafts until explicit refresh and revision review", async () => {
  let current = capabilityPolicyFixture();
  spyOn(capabilityPolicyApi, "get").mockImplementation(async () => capabilityPolicyData(current));
  const save = spyOn(capabilityPolicyApi, "set").mockRejectedValue(new Error("Capability policy revision conflict"));
  await act(async () => root.render(<Panel />));
  await act(async () => button("Edit capability policy").click());
  await choose("Rule 2 mode", "require_approval");
  await act(async () => button("Save capability policy").click());
  expect(save).toHaveBeenCalledTimes(1);
  expect(select("Rule 2 mode").value).toBe("require_approval");
  expect(container.textContent).toContain("Your draft is retained");
  expect(button("Save capability policy").disabled).toBe(true);
  current = { ...current, revision: 2, enabled: false, rules: [] };
  await act(async () => dom.window.dispatchEvent(new dom.window.Event("focus")));
  expect(button("Save capability policy").disabled).toBe(true);
  expect(button("Use refreshed revision for this draft").disabled).toBe(true);
  expect(select("Rule 2 mode").value).toBe("require_approval");
  await act(async () => container.querySelector<HTMLButtonElement>('[aria-label="Refresh capability policy"]')!.click());
  expect(button("Save capability policy").disabled).toBe(true);
  expect(button("Use refreshed revision for this draft").disabled).toBe(false);
  expect(save).toHaveBeenCalledTimes(1);
  await act(async () => button("Use refreshed revision for this draft").click());
  expect(save).toHaveBeenCalledTimes(1);
  save.mockImplementation(async (_id, expected, draft) => {
    expect(expected).toEqual({ revision: 2, enabled: false, ownerUid: 1000 });
    expect(draft.rules[1].mode).toBe("require_approval");
    current = { ...current, revision: 3, rules: draft.rules };
    return current;
  });
  await act(async () => button("Save capability policy").click());
  expect(save).toHaveBeenCalledTimes(2);
  expect(current.enabled).toBe(false);
  expect(container.querySelector('form[aria-label="Edit capability rules"]')).toBeNull();
  expect(container.textContent).toContain("Policy: Disabled");
});

test("unsafe reads and owner mismatches hide saved rules and disable policy mutations", async () => {
  const get = spyOn(capabilityPolicyApi, "get").mockResolvedValue(capabilityPolicyData());
  await act(async () => root.render(<Panel />));
  get.mockRejectedValue(new Error("Capability policy unavailable"));
  await act(async () => container.querySelector<HTMLButtonElement>('[aria-label="Refresh capability policy"]')!.click());
  expect(container.querySelectorAll("article")).toHaveLength(0);
  expect(container.textContent).toContain("Saved rules are hidden until refresh succeeds");
  get.mockResolvedValue(capabilityPolicyData({ ...capabilityPolicyFixture(), owner_uid: 2000 }));
  await act(async () => container.querySelector<HTMLButtonElement>('[aria-label="Refresh capability policy"]')!.click());
  expect(container.querySelectorAll("article")).toHaveLength(0);
  expect(container.textContent).toContain("owner does not match");
  expect(container.querySelector('form')).toBeNull();
});

test("terminal Activities can disable existing policies but cannot edit or re-enable them", async () => {
  let current = capabilityPolicyFixture();
  spyOn(capabilityPolicyApi, "get").mockImplementation(async () => capabilityPolicyData(current));
  const enable = spyOn(capabilityPolicyApi, "enable").mockImplementation(async (_id, _policy, enabled) => {
    current = { ...current, revision: current.revision + 1, enabled };
    return current;
  });
  await act(async () => root.render(<Panel editable={false} />));
  expect(button("Edit capability policy").disabled).toBe(true);
  expect(button("Disable capability policy").disabled).toBe(false);
  await act(async () => button("Disable capability policy").click());
  expect(enable).toHaveBeenCalledTimes(1);
  expect(current.enabled).toBe(false);
  expect(button("Enable capability policy").disabled).toBe(true);
  expect(container.textContent).toContain("all controlled capability checks are blocked");
});

test("drafts survive terminal state changes and late reads cannot replace another selection", async () => {
  let resolveFirst!: (data: CapabilityPolicyData) => void;
  const first = new Promise<CapabilityPolicyData>((resolve) => { resolveFirst = resolve; });
  const get = spyOn(capabilityPolicyApi, "get").mockImplementation((id) => id === "activity-1"
    ? first : Promise.resolve(capabilityPolicyData(capabilityPolicyFixture(id), id)));
  await act(async () => root.render(<Panel key="first" id="activity-1" />));
  await act(async () => root.render(<Panel key="second" id="activity-2" />));
  expect(get.mock.calls[0][1]?.aborted).toBe(true);
  await act(async () => resolveFirst(capabilityPolicyData({ ...capabilityPolicyFixture(), revision: 99 })));
  expect(container.textContent).toContain("Revision: 1");
  expect(container.textContent).not.toContain("Revision: 99");
  await act(async () => button("Edit capability policy").click());
  await choose("Rule 2 mode", "require_approval");
  await act(async () => root.render(<Panel key="second" id="activity-2" editable={false} />));
  expect(select("Rule 2 mode").value).toBe("require_approval");
  expect(select("Rule 2 mode").matches(":disabled")).toBe(true);
  expect(container.textContent).toContain("Your unsaved policy draft is retained");
  expect(button("Save capability policy").disabled).toBe(true);
});
