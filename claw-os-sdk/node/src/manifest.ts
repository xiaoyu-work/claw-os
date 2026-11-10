/** Pure owner-path validation and lexical previews, never authority or mounts. */
import { validateOwnerPathNeed } from "./generated";

function utf8Length(value: string): number {
  for (const character of value) {
    const unit = character.charCodeAt(0);
    if (character.length === 1 && unit >= 0xd800 && unit <= 0xdfff) {
      throw new Error("owner-path text must be valid UTF-8");
    }
  }
  return Buffer.byteLength(value, "utf8");
}

export function validate_owner_path(verb: string, path: string): void {
  validateOwnerPathNeed({ verb, scope: { kind: "owner-path", path } });
  if (utf8Length(path) > 4096 || path.split("/").some((part) => utf8Length(part) > 255)) {
    throw new Error("owner-path exceeds its UTF-8 path or segment bound");
  }
}

/** Read-only lexical preview; Root independently supplies verified home. */
export function resolve_owner_path(verb: string, path: string, ownerHome: string): string {
  validate_owner_path(verb, path);
  if (typeof ownerHome !== "string" || !ownerHome.startsWith("/") || ownerHome.includes("*")) {
    throw new Error("owner home must be absolute and contain no pattern");
  }
  validate_owner_path("fs.read", ownerHome.slice(1));
  const resolved = `${ownerHome}/${path}`;
  if (utf8Length(resolved) > 4096) throw new Error("resolved owner path exceeds 4096 UTF-8 bytes");
  return resolved;
}

function object(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

export function validate_owner_path_bindings(manifest: Record<string, unknown>): void {
  const entries: unknown[] = [];
  const guiEntries: Record<string, unknown>[] = [];
  if (object(manifest.operations)) entries.push(...Object.values(manifest.operations));
  if (object(manifest.mcp) && Array.isArray(manifest.mcp.tools)) entries.push(...manifest.mcp.tools);
  if (object(manifest.desktop)) {
    guiEntries.push(manifest.desktop);
    if (object(manifest.desktop.entries)) guiEntries.push(...Object.values(manifest.desktop.entries).filter(object));
    entries.push(...guiEntries);
  }
  for (const entry of entries) {
    if (!object(entry) || !Array.isArray(entry.needs)) continue;
    validateOwnerPathNeeds(entry.needs);
  }
  for (const gui of guiEntries) {
    if (gui.optional_needs !== undefined) {
      if (!Array.isArray(gui.optional_needs)) throw new Error("GUI optional_needs must be an array");
      validateOwnerPathNeeds(gui.optional_needs);
    }
  }
}

function validateOwnerPathNeeds(needs: unknown[]): void {
  for (const need of needs) {
    if (object(need) && object(need.scope) && need.scope.kind === "owner-path") {
      validateOwnerPathNeed(need);
      validate_owner_path(need.verb as string, need.scope.path as string);
    }
  }
}
