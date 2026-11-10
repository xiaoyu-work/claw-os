import { record, oneOf } from "./api-shapes";

export type AppTool = {
  name: string; command: string; description?: string; args?: unknown[];
};
export type InstalledApp = {
  id: string; name: string; version?: string; content_digest?: string;
  source_name?: string; source_kind?: "product" | "capability";
  state: "installed" | "quarantined"; web_ready: boolean; tools: AppTool[]; error?: string;
};
export type StoreApp = {
  app_id: string; app_version: string; architecture: string; release_version: string;
  artifact: { file: string; size: number; sha256: string; media_type: string };
  provenance: { content_digest: string };
  display: { name: string; description: string; source_name: string; source_kind: "product" | "capability" };
  delivery: { kind: "apt"; package_sha256: string };
  package: StorePackage;
};
export type StorePackage = {
  package: string; version: string; architecture: string; size: number; sha256: string; apps: string[];
};
export type StoreView = { apps: StoreApp[]; installed: InstalledApp[]; architecture: string; expires_at: string };
const APP_ID = /^[a-z][a-z0-9_-]{0,127}$/;
const SHA256 = /^[a-f0-9]{64}$/;

function tools(value: unknown): AppTool[] {
  if (!Array.isArray(value) || value.length > 1000) throw new Error("App tool metadata is invalid.");
  return value.map((tool) => {
    if (!record(tool) || typeof tool.name !== "string" || typeof tool.command !== "string") {
      throw new Error("App tool metadata is invalid.");
    }
    return { ...tool, name: tool.name, command: tool.command } as AppTool;
  });
}

export function readInstalled(value: unknown): InstalledApp[] {
  if (!record(value) || !Array.isArray(value.apps) || value.apps.length > 1000) {
    throw new Error("The OS installed-App response is invalid.");
  }
  const ids = new Set<string>();
  return value.apps.map((app) => {
    if (!record(app) || typeof app.id !== "string" || !APP_ID.test(app.id) ||
        ids.has(app.id) || typeof app.name !== "string" || typeof app.web_ready !== "boolean" ||
        !oneOf(["installed", "quarantined"] as const, app.state)) {
      throw new Error("The OS installed-App identity or status is invalid.");
    }
    ids.add(app.id);
    return { ...app, id: app.id, name: app.name, state: app.state, web_ready: app.web_ready,
      tools: app.state === "quarantined" ? [] : tools(app.tools) } as InstalledApp;
  });
}

export function readStore(value: unknown): StoreView {
  if (!record(value) || !record(value.catalog) || value.catalog.schema !== "claw.app-store/v2" ||
      value.catalog.repository !== "xiaoyu-work/clawos-app" || value.catalog.runtime_abi !== 1 ||
      typeof value.catalog.generation !== "string" || !/^[1-9][0-9]{0,19}$/.test(value.catalog.generation) ||
      typeof value.catalog.expires_at !== "string" ||
      !Number.isFinite(Date.parse(value.catalog.expires_at)) ||
      Date.parse(value.catalog.expires_at) <= Date.now() ||
      !Array.isArray(value.catalog.apps) || !Array.isArray(value.catalog.packages) ||
      value.catalog.packages.length === 0 || value.catalog.packages.length > 10_000 ||
      typeof value.architecture !== "string") {
    throw new Error("The OS did not return a fresh authenticated official App catalog.");
  }
  const packages = new Map<string, StorePackage>();
  const packageIdentities = new Set<string>();
  for (const item of value.catalog.packages) {
    if (!record(item) || typeof item.package !== "string" ||
        !/^[a-z0-9][a-z0-9+.-]{1,254}$/.test(item.package) ||
        typeof item.version !== "string" || !item.version ||
        !oneOf(["all", "amd64", "arm64"] as const, item.architecture) ||
        typeof item.size !== "number" || !Number.isSafeInteger(item.size) ||
        item.size <= 0 || item.size > 1024 ** 3 || typeof item.sha256 !== "string" ||
        !SHA256.test(item.sha256) || packages.has(item.sha256) || !Array.isArray(item.apps) ||
        item.apps.length > 1000 || item.apps.some((id, index, ids) =>
          typeof id !== "string" || !APP_ID.test(id) || index > 0 && id <= ids[index - 1])) {
      throw new Error("The authenticated APT package inventory is malformed.");
    }
    const identity = `${item.package}:${item.version}:${item.architecture}`;
    if (packageIdentities.has(identity)) throw new Error("The APT package inventory is ambiguous.");
    packageIdentities.add(identity);
    packages.set(item.sha256, item as unknown as StorePackage);
  }
  const identities = new Set<string>();
  const apps = value.catalog.apps.map((app): StoreApp => {
    if (!record(app) || typeof app.app_id !== "string" || !APP_ID.test(app.app_id) ||
        !oneOf(["all", "amd64", "arm64"] as const, app.architecture) ||
        typeof app.app_version !== "string" || typeof app.release_version !== "string" ||
        !record(app.artifact) || typeof app.artifact.sha256 !== "string" ||
        !SHA256.test(app.artifact.sha256) ||
        app.artifact.file !== `claw-app-snapshot-${app.artifact.sha256}.tar.gz` ||
        typeof app.artifact.size !== "number" || !Number.isSafeInteger(app.artifact.size) ||
        app.artifact.size <= 0 || app.artifact.media_type !== "application/gzip" ||
        !record(app.provenance) || typeof app.provenance.content_digest !== "string" ||
        !/^sha256:[a-f0-9]{64}$/.test(app.provenance.content_digest) ||
        !record(app.display) || typeof app.display.name !== "string" ||
        typeof app.display.description !== "string" || typeof app.display.source_name !== "string" ||
        !oneOf(["product", "capability"] as const, app.display.source_kind) ||
        !record(app.delivery) || app.delivery.kind !== "apt" ||
        typeof app.delivery.package_sha256 !== "string") {
      throw new Error("The official App release metadata is malformed.");
    }
    const identity = `${app.app_id}:${app.architecture}`;
    if (identities.has(identity)) throw new Error("The App catalog contains duplicate identities.");
    identities.add(identity);
    const pkg = packages.get(app.delivery.package_sha256);
    if (!pkg || pkg.architecture !== app.architecture || !pkg.apps.includes(app.app_id)) {
      throw new Error("The App is not bound to its authenticated APT package.");
    }
    return { ...app, package: pkg } as unknown as StoreApp;
  });
  return { apps, architecture: value.architecture, expires_at: value.catalog.expires_at,
    installed: readInstalled({ apps: value.installed }) };
}

export type AppReview = {
  id: string; revision: string; kind: "install" | "activation" | "update"; status: string;
  subject: { app_id: string; name: string; version: string | null; publisher: string | null };
  permissions: { id: string; label: string; blurb: string; risk: string;
    scope: { description: string }; condition: string | null; uses: unknown[] }[];
  disclosures: { label: string; value: string }[];
};

export function readAppReview(value: unknown): AppReview {
  if (!record(value) || value.schema_version !== 1 || typeof value.id !== "string" ||
      !/^rv-[A-Za-z0-9_-]+$/.test(value.id) || typeof value.revision !== "string" ||
      !/^[1-9][0-9]{0,19}$/.test(value.revision) || BigInt(value.revision) > 18446744073709551615n ||
      !oneOf(["install", "activation", "update"] as const, value.kind) ||
      typeof value.status !== "string" || !record(value.subject) ||
      typeof value.subject.app_id !== "string" || !APP_ID.test(value.subject.app_id) ||
      typeof value.subject.name !== "string" || !Array.isArray(value.permissions) ||
      !Array.isArray(value.disclosures)) {
    throw new Error("The protected OS App review is invalid.");
  }
  for (const permission of value.permissions) {
    if (!record(permission) || typeof permission.id !== "string" || typeof permission.label !== "string" ||
        typeof permission.blurb !== "string" || typeof permission.risk !== "string" ||
        !record(permission.scope) || typeof permission.scope.description !== "string" ||
        !Array.isArray(permission.uses)) throw new Error("The OS App permission disclosure is invalid.");
  }
  for (const disclosure of value.disclosures) {
    if (!record(disclosure) || typeof disclosure.label !== "string" || typeof disclosure.value !== "string") {
      throw new Error("The OS App execution disclosure is invalid.");
    }
  }
  return value as unknown as AppReview;
}

export type AppView = { view_id: string; nonce: string; src: string; app: InstalledApp };
export function readAppView(value: unknown, appId: string): AppView {
  if (!record(value) || value.status !== "ready" || typeof value.view_id !== "string" ||
      !SHA256.test(value.view_id) || value.nonce !== value.view_id ||
      value.src !== `/app-ui/${value.view_id}/index.html` || !record(value.app) ||
      value.app.id !== appId || typeof value.app.name !== "string" ||
      value.app.protocol !== "claw.web-app/v1") {
    throw new Error("The OS returned an invalid owning-App Web view.");
  }
  return { view_id: value.view_id, nonce: value.nonce, src: value.src,
    app: { ...value.app, state: "installed", web_ready: true,
      tools: tools(value.app.tools) } as InstalledApp };
}

export function appMessage(value: unknown, nonce: string, app: InstalledApp): {
  id: string; tool: string; arguments: Record<string, unknown>;
} | null {
  if (!record(value) || value.protocol !== "claw.web-app/v1" || value.type !== "call" ||
      value.nonce !== nonce || typeof value.id !== "string" || !/^[A-Za-z0-9_-]{1,128}$/.test(value.id) ||
      typeof value.tool !== "string" || !app.tools.some((tool) => tool.name === value.tool) ||
      !record(value.arguments) || JSON.stringify(value).length > 1024 * 1024) return null;
  return { id: value.id, tool: value.tool, arguments: value.arguments };
}

export type InstallHistory = {
  request_id: string; review_id: string; review_ids: string[]; app_id: string; name: string; phase: string;
};
export function readInstallHistory(value: unknown): InstallHistory[] {
  if (!record(value) || !Array.isArray(value.requests) || value.requests.length > 1000) {
    throw new Error("The OS installation history is invalid.");
  }
  return value.requests.map((item) => {
    if (!record(item) || typeof item.request_id !== "string" || !/^[a-f0-9]{32}$/.test(item.request_id) ||
        typeof item.review_id !== "string" || !/^rv-[A-Za-z0-9_-]+$/.test(item.review_id) ||
        !Array.isArray(item.review_ids) || item.review_ids.length === 0 || item.review_ids.length > 32 ||
        item.review_ids.some((id) => typeof id !== "string" || !/^rv-[A-Za-z0-9_-]+$/.test(id)) ||
        new Set(item.review_ids).size !== item.review_ids.length || !item.review_ids.includes(item.review_id) ||
        typeof item.app_id !== "string" || !APP_ID.test(item.app_id) || typeof item.name !== "string" ||
        !oneOf(["ready", "cleanup_pending", "committing", "installing", "indeterminate", "installed", "failed", "cancelled", "expired"] as const, item.phase)) {
      throw new Error("The OS installation record is malformed.");
    }
    return { request_id: item.request_id, review_id: item.review_id, app_id: item.app_id,
      review_ids: item.review_ids, name: item.name, phase: item.phase };
  });
}
