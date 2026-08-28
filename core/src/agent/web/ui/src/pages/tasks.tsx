/**
 * Tasks page. Lists the durable clawd queue from `/api/tasks`.
 */

import { useCallback, useEffect, useState } from "react";
import { Loader2, RotateCcw, Square } from "lucide-react";

import { api } from "@/lib/api";
import { Button } from "@/components/ui/button";
import { Card } from "@/components/ui/card";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table";

type Task = {
  id: string;
  title?: string;
  status?: string;
  prompt?: string;
  session_id?: string;
  created_at?: string;
  started_at?: string;
  finished_at?: string;
  waiting_on?: string[];
  error?: string;
};

export function TasksPage() {
  const [tasks, setTasks] = useState<Task[]>([]);
  const [loading, setLoading] = useState(true);
  const [busy, setBusy] = useState<string | null>(null);
  const [err, setErr] = useState<string | null>(null);

  const load = useCallback(async (foreground = true) => {
    if (foreground) {
      setLoading(true);
      setErr(null);
    }
    try {
      const r = await api.get<{ tasks?: Task[] } | Task[]>("/api/tasks");
      setTasks(Array.isArray(r) ? r : r?.tasks || []);
    } catch (e: any) {
      setErr(e?.message || "Failed to load tasks");
    } finally {
      if (foreground) setLoading(false);
    }
  }, []);

  useEffect(() => {
    void load();
    const refresh = () => void load(false);
    const timer = window.setInterval(refresh, 3_000);
    window.addEventListener("cos:notifications-changed", refresh);
    return () => {
      window.clearInterval(timer);
      window.removeEventListener("cos:notifications-changed", refresh);
    };
  }, [load]);

  async function act(id: string, action: "stop" | "resume") {
    setBusy(id);
    try {
      await api.post(`/api/tasks/${id}/${action}`);
      await load(false);
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
          <h1 className="text-xl font-semibold">Tasks</h1>
          <p className="text-xs text-muted-foreground">Durable background jobs.</p>
        </div>
        <Button variant="outline" size="sm" onClick={() => void load()} disabled={loading}>
          {loading ? <Loader2 className="h-3.5 w-3.5 animate-spin" /> : "Refresh"}
        </Button>
      </div>
      {err && <p className="text-sm text-destructive">{err}</p>}
      <Card>
        <Table>
          <TableHeader>
            <TableRow>
              <TableHead>ID</TableHead>
              <TableHead>Title</TableHead>
              <TableHead>Status</TableHead>
              <TableHead className="text-right">Actions</TableHead>
            </TableRow>
          </TableHeader>
          <TableBody>
            {tasks.length === 0 && !loading ? (
              <TableRow>
                <TableCell colSpan={4} className="text-center text-sm text-muted-foreground">
                  No tasks.
                </TableCell>
              </TableRow>
            ) : (
              tasks.map((t) => (
                <TableRow key={t.id}>
                  <TableCell className="font-mono text-xs">{t.id.slice(0, 12)}</TableCell>
                  <TableCell className="text-sm">
                    <div>{t.title || t.prompt || "Agent task"}</div>
                    {t.error ? (
                      <div className="mt-1 max-w-xl text-xs text-destructive">{t.error}</div>
                    ) : null}
                    {t.status === "waiting_approval" && t.waiting_on?.length ? (
                      <div className="mt-1 text-xs text-amber-600 dark:text-amber-400">
                        Waiting for approval {t.waiting_on.join(", ")}
                      </div>
                    ) : null}
                  </TableCell>
                  <TableCell className="text-xs">
                    <span className={statusClass(t.status)}>{formatStatus(t.status)}</span>
                  </TableCell>
                  <TableCell className="text-right">
                    <div className="flex justify-end gap-1">
                      {isActive(t.status) ? (
                        <Button
                          size="sm"
                          variant="ghost"
                          disabled={busy === t.id}
                          onClick={() => act(t.id, "stop")}
                          title="Cancel task"
                        >
                          <Square className="h-3.5 w-3.5" />
                        </Button>
                      ) : (
                        <Button
                          size="sm"
                          variant="ghost"
                          disabled={busy === t.id}
                          onClick={() => act(t.id, "resume")}
                          title="Retry task"
                        >
                          <RotateCcw className="h-3.5 w-3.5" />
                        </Button>
                      )}
                    </div>
                  </TableCell>
                </TableRow>
              ))
            )}
          </TableBody>
        </Table>
      </Card>
    </div>
  );
}

function isActive(status?: string) {
  return status === "pending" || status === "running" || status === "waiting_approval";
}

function formatStatus(status?: string) {
  return (status || "unknown").replace(/_/g, " ");
}

function statusClass(status?: string) {
  if (status === "ok") return "text-emerald-500";
  if (status === "error" || status === "cancelled") return "text-destructive";
  if (status === "waiting_approval") return "text-amber-600 dark:text-amber-400";
  return "text-muted-foreground";
}
