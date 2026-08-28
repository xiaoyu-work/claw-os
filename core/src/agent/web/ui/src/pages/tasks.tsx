/**
 * Tasks page. Lists durable agent tasks from `/api/tasks`. Each task has
 * stop/undo/resume actions (POST /api/tasks/{id}/{action}).
 */

import { useCallback, useEffect, useState } from "react";
import { Loader2, Pause, Play, RotateCcw } from "lucide-react";

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
  purpose?: string;
  title?: string;
  status?: string;
  state?: string;
  created_at?: number | string;
  updated_at?: number | string;
  description?: string;
};

export function TasksPage() {
  const [tasks, setTasks] = useState<Task[]>([]);
  const [loading, setLoading] = useState(true);
  const [busy, setBusy] = useState<string | null>(null);
  const [err, setErr] = useState<string | null>(null);

  const load = useCallback(async () => {
    setLoading(true);
    setErr(null);
    try {
      const r = await api.get<{ tasks?: Task[] } | Task[]>("/api/tasks");
      setTasks(Array.isArray(r) ? r : r?.tasks || []);
    } catch (e: any) {
      setErr(e?.message || "Failed to load tasks");
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    load();
    window.addEventListener("cos:notifications-changed", load);
    return () => window.removeEventListener("cos:notifications-changed", load);
  }, [load]);

  async function act(id: string, action: "stop" | "undo" | "resume") {
    setBusy(id);
    try {
      await api.post(`/api/tasks/${id}/${action}`);
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
          <h1 className="text-xl font-semibold">Tasks</h1>
          <p className="text-xs text-muted-foreground">Durable background jobs.</p>
        </div>
        <Button variant="outline" size="sm" onClick={load} disabled={loading}>
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
                    {t.title || t.purpose || t.description || "—"}
                  </TableCell>
                  <TableCell className="text-xs">{t.status || t.state || "—"}</TableCell>
                  <TableCell className="text-right">
                    <div className="flex justify-end gap-1">
                      <Button
                        size="sm"
                        variant="ghost"
                        disabled={busy === t.id}
                        onClick={() => act(t.id, "stop")}
                      >
                        <Pause className="h-3.5 w-3.5" />
                      </Button>
                      <Button
                        size="sm"
                        variant="ghost"
                        disabled={busy === t.id}
                        onClick={() => act(t.id, "resume")}
                      >
                        <Play className="h-3.5 w-3.5" />
                      </Button>
                      <Button
                        size="sm"
                        variant="ghost"
                        disabled={busy === t.id}
                        onClick={() => act(t.id, "undo")}
                      >
                        <RotateCcw className="h-3.5 w-3.5" />
                      </Button>
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
