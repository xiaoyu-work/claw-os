/**
 * Approvals page. Pending grants + recent decisions over the
 * `/api/approvals/*` routes (already shipped on main).
 */

import { useCallback, useEffect, useState } from "react";
import { Loader2 } from "lucide-react";

import { api } from "@/lib/api";
import { Button } from "@/components/ui/button";
import { Card } from "@/components/ui/card";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { Tabs, TabsContent, TabsList, TabsTrigger } from "@/components/ui/tabs";

type Approval = {
  id: string;
  verb?: string;
  scope?: any;
  summary?: string;
  reason?: string;
  decision?: string;
  decided_at?: number | string;
  requested_at?: number | string;
};

type Duration = "once" | "session" | "forever";

export function ApprovalsPage() {
  const [pending, setPending] = useState<Approval[]>([]);
  const [recent, setRecent] = useState<Approval[]>([]);
  const [loading, setLoading] = useState(true);
  const [busy, setBusy] = useState<string | null>(null);
  const [duration, setDuration] = useState<Record<string, Duration>>({});
  const [err, setErr] = useState<string | null>(null);

  const load = useCallback(async () => {
    setLoading(true);
    setErr(null);
    try {
      const [p, r] = await Promise.all([
        api.get<{ requests?: Approval[]; approvals?: Approval[] } | Approval[]>("/api/approvals/pending"),
        api.get<{ entries?: Approval[]; approvals?: Approval[] } | Approval[]>("/api/approvals/recent"),
      ]);
      setPending(
        Array.isArray(p) ? p : p?.requests || p?.approvals || [],
      );
      setRecent(
        Array.isArray(r) ? r : r?.entries || r?.approvals || [],
      );
    } catch (e: any) {
      setErr(e?.message || "Failed to load approvals");
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    load();
    window.addEventListener("cos:notifications-changed", load);
    return () => window.removeEventListener("cos:notifications-changed", load);
  }, [load]);

  async function decide(id: string, action: "approve" | "deny") {
    setBusy(id);
    try {
      const dur = duration[id] || "once";
      await api.post(`/api/approvals/${id}/${action}`, { duration: dur });
      await load();
    } catch (e: any) {
      setErr(e?.message || `Failed to ${action}`);
    } finally {
      setBusy(null);
    }
  }

  return (
    <div className="flex h-full flex-col gap-4 p-6">
      <div className="flex items-center justify-between">
        <div>
          <h1 className="text-xl font-semibold">Approvals</h1>
          <p className="text-xs text-muted-foreground">
            Capability grants requested by the agent.
          </p>
        </div>
        <Button variant="outline" size="sm" onClick={load} disabled={loading}>
          {loading ? <Loader2 className="h-3.5 w-3.5 animate-spin" /> : "Refresh"}
        </Button>
      </div>
      {err && <p className="text-sm text-destructive">{err}</p>}

      <Tabs defaultValue="pending">
        <TabsList>
          <TabsTrigger value="pending">Pending ({pending.length})</TabsTrigger>
          <TabsTrigger value="recent">Recent</TabsTrigger>
        </TabsList>
        <TabsContent value="pending" className="grid gap-3">
          {pending.length === 0 ? (
            <p className="py-6 text-center text-sm text-muted-foreground">
              No pending approvals.
            </p>
          ) : (
            pending.map((a) => (
              <Card key={a.id} className="p-4">
                <div className="grid gap-2">
                  <div className="flex items-center justify-between gap-3">
                    <div>
                      <div className="text-sm font-medium">
                        <span className="font-mono">{a.verb || "?"}</span>
                        {a.scope ? (
                          <span className="text-muted-foreground">
                            {" "}
                            on{" "}
                            <span className="font-mono">
                              {typeof a.scope === "string" ? a.scope : JSON.stringify(a.scope)}
                            </span>
                          </span>
                        ) : null}
                      </div>
                      {a.reason && (
                        <p className="mt-1 text-xs text-muted-foreground">{a.reason}</p>
                      )}
                    </div>
                    <Select
                      value={duration[a.id] || "once"}
                      onValueChange={(v) =>
                        setDuration((d) => ({ ...d, [a.id]: v as Duration }))
                      }
                    >
                      <SelectTrigger className="h-8 w-32">
                        <SelectValue />
                      </SelectTrigger>
                      <SelectContent>
                        <SelectItem value="once">Once</SelectItem>
                        <SelectItem value="session">Session</SelectItem>
                        <SelectItem value="forever">Forever</SelectItem>
                      </SelectContent>
                    </Select>
                  </div>
                  {a.summary && (
                    <pre className="overflow-x-auto rounded bg-muted px-2 py-1 text-[11px]">
                      {a.summary}
                    </pre>
                  )}
                  <div className="flex gap-2">
                    <Button
                      size="sm"
                      disabled={busy === a.id}
                      onClick={() => decide(a.id, "approve")}
                    >
                      Approve
                    </Button>
                    <Button
                      size="sm"
                      variant="outline"
                      disabled={busy === a.id}
                      onClick={() => decide(a.id, "deny")}
                    >
                      Deny
                    </Button>
                  </div>
                </div>
              </Card>
            ))
          )}
        </TabsContent>
        <TabsContent value="recent" className="grid gap-2">
          {recent.length === 0 ? (
            <p className="py-6 text-center text-sm text-muted-foreground">
              No recent decisions.
            </p>
          ) : (
            recent.map((a) => (
              <Card key={a.id} className="px-3 py-2">
                <div className="flex items-center justify-between text-sm">
                  <div>
                    <span className="font-mono">{a.verb || "?"}</span>
                    {a.scope ? (
                      <span className="text-muted-foreground">
                        {" "}
                        on{" "}
                        <span className="font-mono">
                          {typeof a.scope === "string" ? a.scope : JSON.stringify(a.scope)}
                        </span>
                      </span>
                    ) : null}
                  </div>
                  <span
                    className={
                      a.decision === "approved"
                        ? "text-xs text-emerald-500"
                        : a.decision === "denied"
                          ? "text-xs text-destructive"
                          : "text-xs text-muted-foreground"
                    }
                  >
                    {a.decision || "—"}
                  </span>
                </div>
              </Card>
            ))
          )}
        </TabsContent>
      </Tabs>
    </div>
  );
}
