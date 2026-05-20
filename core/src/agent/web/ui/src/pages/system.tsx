/**
 * System info page. Live data from `cos sysinfo` via `/api/sysinfo/{cmd}`.
 */

import { useCallback, useEffect, useState } from "react";
import { Loader2 } from "lucide-react";

import { api } from "@/lib/api";
import { Button } from "@/components/ui/button";
import { Card } from "@/components/ui/card";

// Real sysinfo command set (matches core/src/sysinfo.rs::run dispatch).
// Grouped to mirror the CLI's organization; each entry is the command
// name passed to /api/sysinfo/{cmd}. Commands that require positional
// arguments (`threads --pid`, `port --port`) are omitted here — the API
// surfaces them too but they aren't usable without a one-shot input UI.
const COMMAND_GROUPS: Array<{ label: string; commands: string[] }> = [
  { label: "Identity", commands: ["info", "env", "uptime", "who", "desktop"] },
  { label: "Resources", commands: ["resources", "loadavg", "sensors", "cgroup"] },
  { label: "Processes", commands: ["proc", "top"] },
  { label: "Network", commands: ["net", "net_rate"] },
  { label: "Storage", commands: ["mounts", "disk_io", "largest_files"] },
  { label: "Logs", commands: ["journal", "dmesg"] },
  { label: "Services", commands: ["services", "failed_units", "coredumps"] },
  { label: "Packages", commands: ["pkg_updates"] },
];

export function SystemPage() {
  const [active, setActive] = useState("info");
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

      <div className="flex flex-1 gap-4 overflow-hidden">
        <aside className="w-44 shrink-0 overflow-y-auto">
          {COMMAND_GROUPS.map((g) => (
            <div key={g.label} className="mb-3">
              <div className="px-2 pb-1 text-[10px] font-medium uppercase tracking-wider text-muted-foreground">
                {g.label}
              </div>
              <div className="grid gap-0.5">
                {g.commands.map((c) => (
                  <button
                    key={c}
                    type="button"
                    onClick={() => setActive(c)}
                    className={
                      "rounded-md px-2 py-1 text-left text-xs transition-colors " +
                      (active === c
                        ? "bg-sidebar-accent text-sidebar-accent-foreground"
                        : "text-sidebar-foreground/80 hover:bg-sidebar-accent/60")
                    }
                  >
                    {c}
                  </button>
                ))}
              </div>
            </div>
          ))}
        </aside>
        <div className="flex-1 overflow-y-auto">
          <Card className="p-3">
            <div className="mb-2 flex items-center justify-between">
              <span className="font-mono text-xs text-muted-foreground">
                GET /api/sysinfo/{active}
              </span>
              <Button
                variant="outline"
                size="sm"
                onClick={() => load(active)}
                disabled={busy === active}
              >
                {busy === active ? (
                  <Loader2 className="h-3.5 w-3.5 animate-spin" />
                ) : (
                  "Refresh"
                )}
              </Button>
            </div>
            {err && <p className="mb-2 text-xs text-destructive">{err}</p>}
            <pre className="max-h-[70vh] overflow-auto rounded bg-muted px-3 py-2 text-[11px]">
              {value === undefined
                ? "loading…"
                : typeof value === "string"
                  ? value
                  : JSON.stringify(value, null, 2)}
            </pre>
          </Card>
        </div>
      </div>
    </div>
  );
}
