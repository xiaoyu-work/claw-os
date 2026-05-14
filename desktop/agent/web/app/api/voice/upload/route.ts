import { bridgeUrl } from "@/lib/bridge";

/**
 * Dev-mode passthrough to `cos-agent-bridge`'s `/api/voice/upload`.
 * In production the bridge serves this endpoint directly and this
 * route is bypassed.
 *
 * The body is the raw audio blob (Content-Type carries the mime,
 * e.g. `audio/webm`). The bridge returns `{text, bytes_received,
 * mime_type, placeholder}` which the React hook drops into the
 * chat input.
 */
export async function POST(req: Request) {
  let url: string;
  try {
    url = await bridgeUrl("/api/voice/upload");
  } catch (error) {
    return Response.json(
      {
        error: "cos-agent-bridge is not reachable",
        detail: (error as Error).message,
      },
      { status: 502 },
    );
  }

  const upstream = await fetch(url, {
    method: "POST",
    headers: {
      "Content-Type":
        req.headers.get("content-type") ?? "application/octet-stream",
    },
    body: req.body,
    // @ts-expect-error Node fetch requires `duplex` when streaming a body
    duplex: "half",
  });

  return new Response(upstream.body, {
    status: upstream.status,
    headers: {
      "Content-Type": upstream.headers.get("content-type") ?? "application/json",
    },
  });
}
