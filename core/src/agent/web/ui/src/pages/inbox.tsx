/**
 * Inbox page. Tails clawd's context-events.jsonl via `/api/inbox`.
 */

import { useCallback, useEffect, useState } from "react";
import { Loader2 } from "lucide-react";

import { api } from "@/lib/api";
import { Button } from "@/components/ui/button";
import { Card } from "@/components/ui/card";

export function InboxPage() {
  const [events, setEvents] = useState<any[]>([]);
  const [loading, setLoading] = useState(true);
  const [err, setErr] = useState<string | null>(null);

  const load = useCallback(async () => {
    setLoading(true);
    setErr(null);
    try {
      const r = await api.get<any>("/api/inbox");
      if (typeof r === "string") {
        const lines = r.split("\n").filter((l) => l.trim().length > 0);
        const parsed = lines.map((l) => {
          try {
            return JSON.parse(l);
          } catch {
            return { _raw: l };
          }
        });
        setEvents(parsed.reverse());
      } else if (Array.isArray(r)) {
        setEvents(r);
      } else if (r?.events) {
        setEvents(r.events.slice().reverse());
      }
    } catch (e: any) {
      setErr(e?.message || "Failed to load inbox");
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    load();
  }, [load]);

  return (
    <div className="flex h-full flex-col gap-4 p-6">
      <div className="flex items-center justify-between">
        <div>
          <h1 className="text-xl font-semibold">Inbox</h1>
          <p className="text-xs text-muted-foreground">
            Last 256 KB of clawd context-events.jsonl.
          </p>
        </div>
        <Button variant="outline" size="sm" onClick={load} disabled={loading}>
          {loading ? <Loader2 className="h-3.5 w-3.5 animate-spin" /> : "Refresh"}
        </Button>
      </div>
      {err && <p className="text-sm text-destructive">{err}</p>}
      <div className="grid gap-2">
        {events.length === 0 && !loading ? (
          <p className="py-6 text-center text-sm text-muted-foreground">No events.</p>
        ) : (
          events.map((e, i) => <InboxRow key={i} e={e} />)
        )}
      </div>
    </div>
  );
}

function InboxRow({ e }: { e: any }) {
  const [open, setOpen] = useState(false);
  const ts = e?.timestamp || e?.time || e?.ts;
  const kind = e?.kind || e?.type || e?.source || "event";
  return (
    <Card className="px-3 py-2">
      <button
        type="button"
        className="grid w-full grid-cols-[120px_1fr_auto] items-center gap-3 text-left text-xs"
        onClick={() => setOpen((v) => !v)}
      >
        <span className="font-mono text-muted-foreground">{String(kind).slice(0, 28)}</span>
        <span className="truncate">{summarize(e)}</span>
        <span className="font-mono text-[10px] text-muted-foreground">
          {ts ? formatTs(ts) : ""}
        </span>
      </button>
      {open && (
        <pre className="mt-2 overflow-x-auto rounded bg-muted px-2 py-1 text-[11px]">
          {JSON.stringify(e, null, 2)}
        </pre>
      )}
    </Card>
  );
}

function summarize(e: any): string {
  if (e?._raw) return String(e._raw).slice(0, 200);
  return (
    e?.summary ||
    e?.message ||
    e?.title ||
    e?.body ||
    e?.text ||
    JSON.stringify(e).slice(0, 200)
  );
}

function formatTs(v: any): string {
  const n = typeof v === "number" ? v : Number(v);
  if (!isNaN(n)) {
    const ms = n < 1e12 ? n * 1000 : n;
    return new Date(ms).toLocaleString();
  }
  const t = Date.parse(String(v));
  return isNaN(t) ? String(v) : new Date(t).toLocaleString();
}
