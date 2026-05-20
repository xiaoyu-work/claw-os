/**
 * System info page. Live data from `cos sysinfo` via `/api/sysinfo/{cmd}`.
 */

import { useCallback, useEffect, useState } from "react";
import { Loader2 } from "lucide-react";

import { api } from "@/lib/api";
import { Button } from "@/components/ui/button";
import { Card } from "@/components/ui/card";
import { Tabs, TabsContent, TabsList, TabsTrigger } from "@/components/ui/tabs";

const COMMANDS = [
  "loadavg",
  "resources",
  "uptime",
  "kernel",
  "disks",
  "network",
  "services",
  "processes",
];

export function SystemPage() {
  const [active, setActive] = useState("loadavg");
  const [data, setData] = useState<Record<string, any>>({});
  const [busy, setBusy] = useState<string | null>(null);
  const [err, setErr] = useState<string | null>(null);

  const load = useCallback(async (cmd: string) => {
    setBusy(cmd);
    setErr(null);
    try {
      const r = await api.get(`/api/sysinfo/${cmd}`);
      setData((d) => ({ ...d, [cmd]: r }));
    } catch (e: any) {
      setErr(e?.message || `Failed to load ${cmd}`);
    } finally {
      setBusy(null);
    }
  }, []);

  useEffect(() => {
    load(active);
  }, [active, load]);

  const value = data[active];

  return (
    <div className="flex h-full flex-col gap-4 p-6">
      <div>
        <h1 className="text-xl font-semibold">System</h1>
        <p className="text-xs text-muted-foreground">
          Live system info from <code className="font-mono">cos sysinfo</code>.
        </p>
      </div>

      <Tabs value={active} onValueChange={setActive}>
        <TabsList className="flex-wrap">
          {COMMANDS.map((c) => (
            <TabsTrigger key={c} value={c} className="capitalize">
              {c}
            </TabsTrigger>
          ))}
        </TabsList>
        {COMMANDS.map((c) => (
          <TabsContent key={c} value={c}>
            <Card className="p-3">
              <div className="mb-2 flex items-center justify-between">
                <span className="font-mono text-xs text-muted-foreground">
                  GET /api/sysinfo/{c}
                </span>
                <Button variant="outline" size="sm" onClick={() => load(c)} disabled={busy === c}>
                  {busy === c ? (
                    <Loader2 className="h-3.5 w-3.5 animate-spin" />
                  ) : (
                    "Refresh"
                  )}
                </Button>
              </div>
              {err && active === c && (
                <p className="mb-2 text-xs text-destructive">{err}</p>
              )}
              <pre className="max-h-[60vh] overflow-auto rounded bg-muted px-3 py-2 text-[11px]">
                {value === undefined
                  ? "loading…"
                  : typeof value === "string"
                    ? value
                    : JSON.stringify(value, null, 2)}
              </pre>
            </Card>
          </TabsContent>
        ))}
      </Tabs>
    </div>
  );
}
