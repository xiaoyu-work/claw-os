/**
 * Single source of truth for cos agent web API access. Mirrors what
 * the old single-file SPA used — token is the same Bearer header /
 * ?t= query parameter the axum routes accept.
 *
 * The token is held in localStorage. If missing or rejected we surface
 * a "needs token" event so the App shell can show the bootstrap modal.
 */

const TOKEN_KEY = "cos.token";

export function getToken(): string {
  if (typeof window === "undefined") return "";
  try {
    return localStorage.getItem(TOKEN_KEY) || "";
  } catch {
    return "";
  }
}

export function setToken(token: string) {
  try {
    localStorage.setItem(TOKEN_KEY, token);
  } catch {
    /* ignore quota / privacy mode errors */
  }
}

export function clearToken() {
  try {
    localStorage.removeItem(TOKEN_KEY);
  } catch {
    /* ignore */
  }
}

export type ApiOpts = RequestInit & { signal?: AbortSignal };

export class ApiError extends Error {
  status: number;
  body: any;
  constructor(status: number, body: any, message: string) {
    super(message);
    this.status = status;
    this.body = body;
  }
}

async function request<T = any>(
  path: string,
  opts: ApiOpts = {},
): Promise<T> {
  const tok = getToken();
  const headers = new Headers(opts.headers);
  if (tok) headers.set("Authorization", `Bearer ${tok}`);
  if (opts.body && !headers.has("Content-Type")) {
    headers.set("Content-Type", "application/json");
  }
  const res = await fetch(path, { ...opts, headers });
  if (!res.ok) {
    let body: any;
    try {
      body = await res.json();
    } catch {
      body = await res.text();
    }
    const msg =
      (body && typeof body === "object" && (body.error || body.message)) ||
      `HTTP ${res.status}`;
    throw new ApiError(res.status, body, msg);
  }
  const ct = res.headers.get("content-type") || "";
  if (ct.includes("application/json")) return (await res.json()) as T;
  return (await res.text()) as any;
}

export const api = {
  get: <T = any>(p: string, opts?: ApiOpts) => request<T>(p, { ...opts, method: "GET" }),
  post: <T = any>(p: string, body?: any, opts?: ApiOpts) =>
    request<T>(p, { ...opts, method: "POST", body: body ? JSON.stringify(body) : undefined }),
  delete: <T = any>(p: string, opts?: ApiOpts) => request<T>(p, { ...opts, method: "DELETE" }),
};

/**
 * Stream Server-Sent Events from /api/chat. We can't use EventSource
 * because we need to POST a JSON body and pass the Bearer header.
 */
export async function streamSse(
  path: string,
  body: any,
  on: (event: string, data: any) => void,
  signal?: AbortSignal,
): Promise<void> {
  const tok = getToken();
  const res = await fetch(path, {
    method: "POST",
    headers: {
      "Content-Type": "application/json",
      ...(tok ? { Authorization: `Bearer ${tok}` } : {}),
      Accept: "text/event-stream",
    },
    body: JSON.stringify(body),
    signal,
  });
  if (!res.ok || !res.body) {
    let errBody: any;
    try {
      errBody = await res.json();
    } catch {
      errBody = await res.text();
    }
    throw new ApiError(res.status, errBody, `SSE ${res.status}`);
  }
  const reader = res.body.getReader();
  const decoder = new TextDecoder();
  let buf = "";
  while (true) {
    const { value, done } = await reader.read();
    if (done) break;
    buf += decoder.decode(value, { stream: true });
    // SSE frames are separated by blank lines (\n\n).
    let idx = buf.indexOf("\n\n");
    while (idx >= 0) {
      const raw = buf.slice(0, idx);
      buf = buf.slice(idx + 2);
      const frame = parseSseFrame(raw);
      if (frame) on(frame.event, frame.data);
      idx = buf.indexOf("\n\n");
    }
  }
}

function parseSseFrame(raw: string): { event: string; data: any } | null {
  let event = "message";
  const dataLines: string[] = [];
  for (const line of raw.split("\n")) {
    if (line.startsWith("event:")) event = line.slice(6).trim();
    else if (line.startsWith("data:")) dataLines.push(line.slice(5).trim());
    // ignore id:, retry:, and comment lines (start with ":")
  }
  if (dataLines.length === 0) return null;
  const raw_data = dataLines.join("\n");
  try {
    return { event, data: JSON.parse(raw_data) };
  } catch {
    return { event, data: raw_data };
  }
}
