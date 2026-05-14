import { bridgeUrl } from "@/lib/bridge";

export async function GET() {
  let url: string;
  try {
    url = await bridgeUrl("/api/sessions");
  } catch (error) {
    return Response.json(
      {
        error: "cos-agent-bridge is not reachable",
        detail: (error as Error).message,
      },
      { status: 502 },
    );
  }

  const upstream = await fetch(url, { method: "GET" });
  const body = await upstream.text();
  return new Response(body, {
    status: upstream.status,
    headers: {
      "Content-Type": upstream.headers.get("content-type") ?? "application/json",
    },
  });
}

export async function POST(req: Request) {
  let url: string;
  try {
    url = await bridgeUrl("/api/sessions");
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

  const body = await upstream.text();
  return new Response(body, {
    status: upstream.status,
    headers: {
      "Content-Type": upstream.headers.get("content-type") ?? "application/json",
    },
  });
}
