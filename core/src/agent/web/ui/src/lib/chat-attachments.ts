export const MAX_IMAGE_ATTACHMENTS = 4;
export const MAX_IMAGE_ATTACHMENT_BYTES = 256 * 1024;

const SUPPORTED_IMAGE_TYPES = new Set([
  "image/png",
  "image/jpeg",
  "image/gif",
  "image/webp",
]);

export type PendingImageAttachment = {
  id: string;
  name: string;
  mediaType: string;
  data: string;
  bytes: number;
  dataUrl: string;
};

export async function readImageAttachments(
  files: File[],
  existing: PendingImageAttachment[],
): Promise<PendingImageAttachment[]> {
  if (existing.length + files.length > MAX_IMAGE_ATTACHMENTS) {
    throw new Error(`Attach at most ${MAX_IMAGE_ATTACHMENTS} images.`);
  }
  let total = existing.reduce((sum, attachment) => sum + attachment.bytes, 0);
  const attachments: PendingImageAttachment[] = [];
  for (const file of files) {
    if (!SUPPORTED_IMAGE_TYPES.has(file.type)) {
      throw new Error(`${file.name || "Image"} has an unsupported image type.`);
    }
    if (file.size <= 0) {
      throw new Error(`${file.name || "Image"} is empty.`);
    }
    total += file.size;
    if (total > MAX_IMAGE_ATTACHMENT_BYTES) {
      throw new Error(
        `Attached images may total at most ${formatAttachmentBytes(MAX_IMAGE_ATTACHMENT_BYTES)}.`,
      );
    }
    const bytes = new Uint8Array(await file.arrayBuffer());
    const data = bytesToBase64(bytes);
    attachments.push({
      id: `${Date.now()}-${Math.random().toString(36).slice(2, 9)}`,
      name: file.name || "image",
      mediaType: file.type,
      data,
      bytes: file.size,
      dataUrl: `data:${file.type};base64,${data}`,
    });
  }
  return attachments;
}

export function formatAttachmentBytes(bytes: number): string {
  if (bytes < 1_024) return `${bytes} B`;
  return `${(bytes / 1_024).toFixed(bytes < 10 * 1_024 ? 1 : 0)} KiB`;
}

function bytesToBase64(bytes: Uint8Array): string {
  let binary = "";
  const chunk = 0x8000;
  for (let offset = 0; offset < bytes.length; offset += chunk) {
    binary += String.fromCharCode(...bytes.subarray(offset, offset + chunk));
  }
  return btoa(binary);
}
