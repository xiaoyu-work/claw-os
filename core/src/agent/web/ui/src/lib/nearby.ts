import { api } from "@/lib/api";
import { record } from "@/lib/api-shapes";

export const MAX_FILE_BYTES = 128 * 1024 * 1024;
export const MAX_TOTAL_BYTES = 512 * 1024 * 1024;
const MAX_WIRE_BYTES = MAX_TOTAL_BYTES + 16 * 1024 * 1024;
export const CHUNK_BYTES = 192 * 1024;
export type RadioMode = "shared" | "exclusive" | "active_monitor";
export type Phase = "offered" | "accepted" | "sending" | "received" | "delivered" | "declined" | "failed" | "expired" | "interrupted";
export type FileInfo = { name: string; bytes: number | null };
export type Upload = { id: string; name: string; bytes: number; written: number; ready: boolean };
export type NearbyView = {
  schema: 1; owner_uid: number; inbox: string;
  probe: { available: boolean; mode: RadioMode | null; driver: string | null; diagnostics: string[] };
  uploads: Upload[];
  session: {
    id: string; phase: "starting" | "ready" | "stopping" | "ended" | "failed" | "interrupted";
    mode: RadioMode; receive: boolean; expires_at: string; discovering: boolean; message: string | null;
    peers: { id: string; name: string; discovered_at: string }[];
    offers: { id: string; sender: string; files: FileInfo[]; expires_at: string; phase: Phase }[];
    transfers: { id: string; incoming: boolean; peer: string; files: FileInfo[]; phase: Phase; bytes: number;
      total_bytes: number | null; received_paths: string[]; message: string | null }[];
  } | null;
};
export type NearbyAction =
  | { action: "open"; duration_secs: number; receive: boolean; expected_mode: RadioMode; allow_disruption: boolean }
  | { action: "discover" | "stop"; session_id: string }
  | { action: "send"; session_id: string; peer_id: string; paths: string[]; uploads: string[] }
  | { action: "decide"; session_id: string; offer_id: string; accept: boolean }
  | { action: "cancel"; session_id: string; transfer_id: string }
  | { action: "upload_begin"; name: string; bytes: number }
  | { action: "upload_chunk"; upload_id: string; offset: number; data: string }
  | { action: "upload_finish" | "upload_delete"; upload_id: string };

function invalid(): never { throw new Error("Invalid nearby sharing response. Refresh before changing sharing."); }
function obj(value: unknown, keys: string[]): Record<string, unknown> {
  if (!record(value) || Object.keys(value).length !== keys.length
    || !keys.every((key) => Object.prototype.hasOwnProperty.call(value, key))) invalid();
  return value;
}
function str(value: unknown, max = 2048): string {
  if (typeof value !== "string" || !value.trim() || new TextEncoder().encode(value).length > max
    || /[\u0000-\u001f\u007f-\u009f\u202a-\u202e\u2066-\u2069]/.test(value)) invalid();
  return value;
}
function id(value: unknown): string {
  const valueId = str(value, 36);
  if (!/^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/.test(valueId)) invalid();
  return valueId;
}
function time(value: unknown): string {
  const valueTime = str(value, 64);
  if (!Number.isFinite(Date.parse(valueTime))) invalid();
  return valueTime;
}
function num(value: unknown, max = MAX_TOTAL_BYTES): number {
  if (typeof value !== "number" || !Number.isSafeInteger(value) || value < 0 || value > max) invalid();
  return value;
}
function bool(value: unknown): boolean { if (typeof value !== "boolean") invalid(); return value; }
function choice<const T extends readonly string[]>(value: unknown, values: T): T[number] {
  const selected = values.find((item) => item === value);
  if (selected === undefined) invalid();
  return selected;
}
function list<T>(value: unknown, max: number, read: (item: unknown) => T): T[] {
  if (!Array.isArray(value) || value.length > max) invalid();
  return value.map(read);
}
function unique<T extends { id: string }>(items: T[]): T[] {
  if (new Set(items.map((item) => item.id)).size !== items.length) invalid();
  return items;
}
function filename(value: unknown): string {
  const name = str(value, 240);
  if (name === "." || name === ".." || /[/\\:]/.test(name)) invalid();
  return name;
}
function files(value: unknown): FileInfo[] {
  const result = list(value, 16, (item) => {
    const file = obj(item, ["name", "bytes"]);
    return { name: filename(file.name), bytes: file.bytes === null ? null : num(file.bytes, MAX_FILE_BYTES) };
  });
  if (!result.length || result.reduce((sum, file) => sum + (file.bytes ?? 0), 0) > MAX_TOTAL_BYTES) invalid();
  if (new Set(result.map((file) => file.name)).size !== result.length) invalid();
  return result;
}
const phases = ["offered", "accepted", "sending", "received", "delivered", "declined", "failed", "expired", "interrupted"] as const;
const modes = ["shared", "exclusive", "active_monitor"] as const;

function readUpload(value: unknown): Upload {
  const upload = obj(value, ["id", "name", "bytes", "written", "ready"]);
  const result = { id: id(upload.id), name: filename(upload.name), bytes: num(upload.bytes, MAX_FILE_BYTES),
    written: num(upload.written, MAX_FILE_BYTES), ready: bool(upload.ready) };
  if (result.written > result.bytes || result.ready && result.written !== result.bytes) invalid();
  return result;
}
export function readNearby(value: unknown): NearbyView {
  if (new TextEncoder().encode(JSON.stringify(value)).length > 512 * 1024) invalid();
  const view = obj(value, ["schema", "owner_uid", "inbox", "probe", "session", "uploads"]);
  if (view.schema !== 1) invalid();
  const probe = obj(view.probe, ["available", "mode", "driver", "diagnostics"]);
  const result: NearbyView = {
    schema: 1, owner_uid: num(view.owner_uid, 0xffff_ffff), inbox: str(view.inbox, 4096),
    probe: { available: bool(probe.available), mode: probe.mode === null ? null : choice(probe.mode, modes),
      driver: probe.driver === null ? null : str(probe.driver, 128),
      diagnostics: list(probe.diagnostics, 16, (item) => str(item, 1024)) },
    uploads: unique(list(view.uploads, 16, readUpload)), session: null,
  };
  if (result.probe.available && result.probe.mode === null) invalid();
  if (!result.owner_uid || result.uploads.reduce((sum, upload) => sum + upload.bytes, 0) > MAX_TOTAL_BYTES) invalid();
  if (view.session !== null) {
    const session = obj(view.session, ["id", "phase", "mode", "receive", "expires_at", "peers", "offers", "discovering", "transfers", "message"]);
    result.session = {
      id: id(session.id), phase: choice(session.phase, ["starting", "ready", "stopping", "ended", "failed", "interrupted"] as const),
      mode: choice(session.mode, modes), receive: bool(session.receive), expires_at: time(session.expires_at),
      discovering: bool(session.discovering), message: session.message === null ? null : str(session.message),
      peers: unique(list(session.peers, 32, (item) => {
        const peer = obj(item, ["id", "name", "discovered_at"]);
        return { id: id(peer.id), name: str(peer.name, 240), discovered_at: time(peer.discovered_at) };
      })),
      offers: unique(list(session.offers, 16, (item) => {
        const offer = obj(item, ["id", "sender", "files", "expires_at", "phase"]);
        return { id: id(offer.id), sender: str(offer.sender, 240), files: files(offer.files),
          expires_at: time(offer.expires_at), phase: choice(offer.phase, phases) };
      })),
      transfers: unique(list(session.transfers, 32, (item) => {
        const transfer = obj(item, ["id", "incoming", "peer", "files", "phase", "bytes", "total_bytes", "received_paths", "message"]);
        const bytes = num(transfer.bytes, MAX_WIRE_BYTES);
        const total = transfer.total_bytes === null ? null : num(transfer.total_bytes, MAX_WIRE_BYTES);
        if (total !== null && bytes > total) invalid();
        return { id: id(transfer.id), incoming: bool(transfer.incoming), peer: str(transfer.peer, 240),
          files: files(transfer.files), phase: choice(transfer.phase, phases), bytes,
          total_bytes: total,
          received_paths: list(transfer.received_paths, 16, (item) => str(item, 4096)),
          message: transfer.message === null ? null : str(transfer.message) };
      })),
    };
  }
  return result;
}
export const nearbyApi = {
  status: async (signal?: AbortSignal) => readNearby(await api.get<unknown>("/api/nearby", { signal })),
  control: async (request: NearbyAction, owner: number) => {
    const response = obj(await api.post<unknown>("/api/nearby/control", { request }), ["view", "upload"]);
    const view = readNearby(response.view);
    if (view.owner_uid !== owner) invalid();
    const upload = response.upload === null ? null : readUpload(response.upload);
    if (request.action === "open" && (!view.session || view.session.mode !== request.expected_mode
      || view.session.receive !== request.receive)) invalid();
    if ("session_id" in request && view.session?.id !== request.session_id) invalid();
    if (request.action === "decide" && !view.session?.offers.some((offer) =>
      offer.id === request.offer_id && offer.phase === (request.accept ? "accepted" : "declined"))) invalid();
    if (request.action === "cancel" && !view.session?.transfers.some((transfer) =>
      transfer.id === request.transfer_id && !transfer.incoming)) invalid();
    const last = view.session?.transfers[view.session.transfers.length - 1];
    if (request.action === "send" && (!last || last.incoming || last.phase !== "sending")) invalid();
    if (request.action === "upload_begin" && (!upload || upload.name !== request.name || upload.bytes !== request.bytes)) invalid();
    if (request.action === "upload_chunk" && (!upload || upload.id !== request.upload_id
      || upload.written !== request.offset + atob(request.data).length)) invalid();
    if (request.action === "upload_finish" && (!upload?.ready || upload.id !== request.upload_id)) invalid();
    return { view, upload };
  },
};

export async function stageFile(file: File, owner: number, update: (view: NearbyView) => void, alive: () => boolean): Promise<string> {
  if (file.size > MAX_FILE_BYTES) throw new Error("A selected file exceeds 128 MiB.");
  filename(file.name);
  const started = await nearbyApi.control({ action: "upload_begin", name: file.name, bytes: file.size }, owner);
  if (!started.upload) invalid();
  const uploadId = started.upload.id;
  update(started.view);
  for (let offset = 0; offset < file.size; offset += CHUNK_BYTES) {
    if (!alive()) throw new Error("Upload detached. Its partial staging file remains visible for explicit deletion; no send was started.");
    const bytes = new Uint8Array(await file.slice(offset, offset + CHUNK_BYTES).arrayBuffer());
    if (!alive()) throw new Error("Upload detached; no send was started.");
    const data = btoa(Array.from(bytes, (byte) => String.fromCharCode(byte)).join(""));
    const result = await nearbyApi.control({ action: "upload_chunk", upload_id: uploadId, offset, data }, owner);
    update(result.view);
  }
  if (!alive()) throw new Error("Upload detached; inspect its staged bytes before sending.");
  const finished = await nearbyApi.control({ action: "upload_finish", upload_id: uploadId }, owner);
  update(finished.view); return uploadId;
}
