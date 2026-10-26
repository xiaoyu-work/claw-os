import { afterEach, expect, spyOn, test } from "bun:test";
import { CHUNK_BYTES, MAX_FILE_BYTES, MAX_TOTAL_BYTES, nearbyApi, readNearby, stageFile,
  type NearbyAction, type NearbyView, type Upload } from "../src/lib/nearby";

const windowId = "11111111-1111-4111-8111-111111111111";
const uploadId = "22222222-2222-4222-8222-222222222222";
function fixture(): NearbyView {
  return {
    schema: 1, owner_uid: 1000, inbox: "/home/user/Downloads/Claw Nearby", uploads: [],
    probe: { available: true, mode: "shared", driver: "fixture", diagnostics: [] },
    session: { id: windowId, phase: "ready", mode: "shared", receive: true,
      expires_at: "2026-11-01T12:00:00.000Z", discovering: false, message: null,
      peers: [], offers: [], transfers: [] },
  };
}
afterEach(() => { spyOn(nearbyApi, "control").mockRestore(); });

test("nearby views are closed and cannot turn metadata into authority", () => {
  const view = fixture();
  expect(readNearby(view)).toEqual(view);
  for (const extra of ["capabilities", "auto_accept", "executable", "owner"]) {
    expect(() => readNearby({ ...view, [extra]: true })).toThrow();
  }
  expect(() => readNearby({ ...view, owner_uid: -1 })).toThrow();
  expect(() => readNearby({ ...view, session: { ...view.session, expires_at: "not a time" } })).toThrow();
  expect(() => readNearby({ ...view, session: { ...view.session, id: "another-window" } })).toThrow();
  expect(() => readNearby({ ...view, probe: { ...view.probe, driver: "\u202ehidden" } })).toThrow();
});

test("nearby browser limits exactly match the bounded file and upload contract", () => {
  const view = fixture();
  view.uploads = Array.from({ length: 4 }, (_, index) => ({
    id: `22222222-2222-4222-8222-22222222222${index}`, name: `file-${index}`,
    bytes: MAX_FILE_BYTES, written: MAX_FILE_BYTES, ready: true,
  }));
  expect(view.uploads.reduce((sum, upload) => sum + upload.bytes, 0)).toBe(MAX_TOTAL_BYTES);
  expect(readNearby(view).uploads.length).toBe(4);
  view.uploads[0].bytes++;
  expect(() => readNearby(view)).toThrow();
  expect(() => readNearby({ ...fixture(), uploads: [{ id: uploadId, name: "../outside", bytes: 1, written: 1, ready: true }] })).toThrow();
});

test("browser staging uses exact bounded offsets and does not send or activate the radio", async () => {
  const requests: NearbyAction[] = [];
  let upload: Upload | null = null;
  spyOn(nearbyApi, "control").mockImplementation(async (request, owner) => {
    expect(owner).toBe(1000); requests.push(request);
    if (request.action === "upload_begin") {
      upload = { id: uploadId, name: request.name, bytes: request.bytes, written: 0, ready: false };
    } else if (request.action === "upload_chunk" && upload) {
      expect(request.offset).toBe(upload.written);
      const count = atob(request.data).length;
      expect(count).toBeLessThanOrEqual(CHUNK_BYTES);
      upload = { ...upload, written: upload.written + count };
    } else if (request.action === "upload_finish" && upload) {
      expect(upload.written).toBe(upload.bytes); upload = { ...upload, ready: true };
    } else { throw new Error("staging must never create a radio or send action"); }
    return { view: { ...fixture(), uploads: upload ? [upload] : [] }, upload };
  });
  const file = new File([new Uint8Array(CHUNK_BYTES + 5)], "fixture.bin");
  const id = await stageFile(file, 1000, () => {}, () => true);
  expect(id).toBe(uploadId);
  expect(requests.map((request) => request.action)).toEqual(["upload_begin", "upload_chunk", "upload_chunk", "upload_finish"]);
});

test("an uncertain upload is never automatically replayed", async () => {
  let chunks = 0;
  spyOn(nearbyApi, "control").mockImplementation(async (request) => {
    if (request.action === "upload_begin") {
      const upload = { id: uploadId, name: request.name, bytes: request.bytes, written: 0, ready: false };
      return { view: { ...fixture(), uploads: [upload] }, upload };
    }
    chunks++;
    throw new Error("connection lost after the chunk may have been applied");
  });
  await expect(stageFile(new File(["payload"], "fixture.txt"), 1000, () => {}, () => true)).rejects.toThrow("connection lost");
  expect(chunks).toBe(1);
});
