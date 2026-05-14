import { bridgeUrl } from "@/lib/bridge";

/**
 * Dev-mode passthrough to `cos-agent-bridge`. In production the bridge
 * itself serves /api/chat over SSE, so this route is bypassed.
 */
export async function POST(req: Request) {
  let url: string;
  try {
    url = await bridgeUrl("/api/chat");
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
    headers: { "Content-Type": "application/json" },
    body: req.body,
    // @ts-expect-error Node fetch requires `duplex` when streaming a body
    duplex: "half",
  });

  return new Response(upstream.body, {
    status: upstream.status,
    headers: {
      "Content-Type":
        upstream.headers.get("content-type") ?? "text/event-stream",
      "Cache-Control": "no-cache, no-transform",
      Connection: "keep-alive",
    },
  });
}
