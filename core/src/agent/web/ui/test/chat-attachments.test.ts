import { describe, expect, test } from "bun:test";

import {
  MAX_IMAGE_ATTACHMENT_BYTES,
  readImageAttachments,
} from "../src/lib/chat-attachments";

describe("chat image attachments", () => {
  test("encodes supported images without changing their bytes", async () => {
    const bytes = new Uint8Array([0x89, 0x50, 0x4e, 0x47, 1, 2, 3]);
    const file = new File([bytes], "screen.png", { type: "image/png" });
    const attachments = await readImageAttachments([file], []);

    expect(attachments).toHaveLength(1);
    expect(attachments[0].name).toBe("screen.png");
    expect(attachments[0].mediaType).toBe("image/png");
    expect(attachments[0].bytes).toBe(bytes.length);
    expect(attachments[0].dataUrl).toBe(
      `data:image/png;base64,${attachments[0].data}`,
    );
  });

  test("rejects unsupported and collectively oversized files", async () => {
    await expect(
      readImageAttachments(
        [new File(["text"], "note.txt", { type: "text/plain" })],
        [],
      ),
    ).rejects.toThrow("unsupported");

    const first = new File(
      [new Uint8Array(MAX_IMAGE_ATTACHMENT_BYTES)],
      "full.png",
      { type: "image/png" },
    );
    const second = new File([new Uint8Array([1])], "extra.png", {
      type: "image/png",
    });
    await expect(readImageAttachments([first, second], [])).rejects.toThrow(
      "total",
    );
  });
});
