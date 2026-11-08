import { useCallback, useEffect, useMemo, useState } from "react";
import { Download, ExternalLink, Loader2, Package, RefreshCw, Search, ShieldCheck } from "lucide-react";

import { api } from "@/lib/api";
import { readAppReview, readInstalled, readStore, readInstallHistory, type AppReview, type InstalledApp, type StoreApp, type StoreView } from "@/lib/apps";
import { navigate } from "@/lib/router";
import { AppReviewDialog } from "@/components/app-review";
import { Button } from "@/components/ui/button";
import { Card } from "@/components/ui/card";
import { Input } from "@/components/ui/input";

function size(bytes: number) {
  return bytes < 1024 * 1024 ? `${Math.ceil(bytes / 1024)} KiB` : `${(bytes / 1024 / 1024).toFixed(1)} MiB`;
}

export function AppStorePage() {
  const [store, setStore] = useState<StoreView | null>(null);
  const [installed, setInstalled] = useState<InstalledApp[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);
  const [query, setQuery] = useState("");
  const [filter, setFilter] = useState("all");
  const [busy, setBusy] = useState<string | null>(null);
  const [pending, setPending] = useState<{ request_id: string; review: AppReview } | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [requests, setRequests] = useState<{request_id:string;review_id:string;app_id:string;name:string;phase:string}[]>([]);

  const load = useCallback(async (signal?: AbortSignal) => {
    setLoading(true);
    const [catalog, local, history] = await Promise.allSettled([
      api.get("/api/app-store", { signal }), api.get("/api/apps", { signal }), api.get("/api/app-installs", { signal }),
    ]);
    if (signal?.aborted) return;
    try {
      if (local.status === "fulfilled") setInstalled(readInstalled(local.value));
      if (history.status === "fulfilled") setRequests(readInstallHistory(history.value));
      if (catalog.status === "fulfilled") {
        const next = readStore(catalog.value);
        setStore(next);
        setInstalled(next.installed);
        setError(null);
      } else {
        setStore(null);
        setError(catalog.reason instanceof Error ? catalog.reason.message : "Official App releases are unavailable.");
      }
    } catch (failure) {
      setError(failure instanceof Error ? failure.message : "App metadata is invalid.");
      setStore(null);
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    const controller = new AbortController();
    void load(controller.signal);
    return () => controller.abort();
  }, [load]);

  const groups = useMemo(() => [...new Set(store?.apps.map((app) => app.display.source_name) || [])].sort(), [store]);
  const apps = useMemo(() => {
    const selected = new Map<string, StoreApp>();
    for (const app of store?.apps || []) {
      if (app.architecture !== "all" && app.architecture !== store?.architecture) continue;
      if (!selected.has(app.app_id) || app.architecture === store?.architecture) selected.set(app.app_id, app);
    }
    return [...selected.values()].filter((app) =>
      (filter === "all" || filter === app.display.source_name) &&
      `${app.display.name} ${app.app_id} ${app.display.description}`.toLowerCase().includes(query.toLowerCase()));
  }, [store, query, filter]);

  async function install(app: StoreApp) {
    setBusy(app.app_id);
    setNotice("Downloading and verifying the official release...");
    try {
      const result = await api.post(`/api/apps/${encodeURIComponent(app.app_id)}/install`,
        { sha256: app.artifact.sha256 }, { signal: AbortSignal.timeout(240_000) });
      if (result?.status !== "review_required" || typeof result.request_id !== "string" ||
          !/^[a-f0-9]{32}$/.test(result.request_id)) throw new Error("The OS did not prepare a reviewed installation.");
      setPending({ request_id: result.request_id, review: readAppReview(result.review) });
      setNotice(null);
    } catch (failure) {
      setNotice(failure instanceof Error ? failure.message : "The download outcome is not confirmed.");
    } finally {
      setBusy(null);
    }
  }

  async function commit() {
    if (!pending) return;
    const current = pending;
    setPending(null);
    setBusy(current.review.subject.app_id);
    setNotice("Installing the reviewed package...");
    try {
      const result = await api.post(`/api/app-installs/${current.request_id}/commit`, undefined,
        { signal: AbortSignal.timeout(60_000) });
      if (result?.status !== "installed" || result.app?.id !== current.review.subject.app_id) {
        throw new Error("Installation was not confirmed. Refresh installed Apps before retrying.");
      }
      setNotice(`${result.app.name} installed. Its declared tools are discoverable by the Agent's next task.`);
      window.dispatchEvent(new CustomEvent("cos:apps-changed"));
      await load();
    } catch (failure) {
      setNotice(failure instanceof Error ? failure.message : "Installation was not confirmed. Refresh before retrying.");
    } finally {
      setBusy(null);
    }
  }

  async function cancel() {
    if (!pending) return;
    const current = pending;
    setPending(null);
    try {
      await api.post(`/api/app-installs/${current.request_id}/cancel`);
      setNotice("Installation cancelled. No runtime permissions were granted.");
    } catch (failure) {
      setNotice(failure instanceof Error ? failure.message : "Cancellation was not confirmed.");
    }
  }

  async function resume(request: {request_id:string;review_id:string}) {
    try {
      const result = await api.get(`/api/app-reviews/${encodeURIComponent(request.review_id)}`);
      setPending({ request_id: request.request_id, review: readAppReview(result.review) });
    } catch (failure) {
      setNotice(failure instanceof Error ? failure.message : "Installation state could not be inspected.");
    }
  }

  return <div className="flex h-full flex-col overflow-y-auto p-6">
    <div className="flex flex-wrap items-start justify-between gap-3">
      <div><h1 className="text-xl font-semibold">App Store</h1>
        <p className="mt-1 text-sm text-muted-foreground">Independent Apps from the official Claw OS Applications release channel.</p></div>
      <Button variant="outline" disabled={loading || !!busy} onClick={() => void load()}>
        <RefreshCw className={`mr-2 h-4 w-4 ${loading ? "animate-spin" : ""}`} /> Refresh
      </Button>
    </div>
    <Card className="mt-5 flex items-start gap-3 p-4">
      <ShieldCheck className="mt-0.5 h-5 w-5 shrink-0 text-primary" />
      <div className="text-xs"><p className="font-medium">Signed releases, protected installation review</p>
        <p className="mt-1 text-muted-foreground">The OS verifies catalog freshness, archive digests and App signatures.
          App interfaces run in isolated views. Installing does not grant every capability or authorize AI.</p></div>
    </Card>
    <div className="mt-5 flex flex-wrap gap-3">
      <div className="relative min-w-52 flex-1"><Search className="absolute left-3 top-2.5 h-4 w-4 text-muted-foreground" />
        <Input className="pl-9" placeholder="Search Apps" value={query} onChange={(event) => setQuery(event.target.value)} /></div>
      <select className="rounded-md border bg-background px-3 text-sm" value={filter} onChange={(event) => setFilter(event.target.value)}>
        <option value="all">All products and capability clients</option>
        {groups.map((group) => <option key={group} value={group}>{group}</option>)}
      </select>
    </div>
    {notice && <p role="status" className="mt-4 rounded-lg border p-3 text-sm">{notice}</p>}
    {requests.filter((item) => item.phase === "ready" || item.phase === "committing" || item.phase === "failed").map((item) =>
      <div key={item.request_id} className="mt-3 flex items-center justify-between rounded-lg border p-3 text-sm">
        <p>{item.name} · {item.phase === "ready" ? "Waiting for installation review" : "Inspect installation outcome"}</p>
        {item.phase === "ready" && <Button variant="outline" size="sm" onClick={() => void resume(item)}>Resume review</Button>}
      </div>)}
    {error && <div role="alert" className="mt-4 rounded-lg border border-destructive/30 p-4 text-sm">
      <p className="font-medium">Official release catalog unavailable</p><p className="mt-1 text-muted-foreground">{error}</p>
      <p className="mt-2 text-xs text-muted-foreground">No source-checkout fallback is used. Already installed, verified Web Apps remain available.</p>
    </div>}
    {loading && <p className="mt-5 flex items-center gap-2 text-sm text-muted-foreground"><Loader2 className="h-4 w-4 animate-spin" /> Checking releases...</p>}
    <div className="mt-5 grid grid-cols-1 gap-4 lg:grid-cols-2 xl:grid-cols-3">
      {apps.map((app) => {
        const local = installed.find((item) => item.id === app.app_id && item.state === "installed");
        const current = local?.content_digest === app.provenance.content_digest;
        return <Card key={app.app_id} className="flex flex-col p-4">
          <div className="flex items-start gap-3"><div className="rounded-xl bg-primary/10 p-3"><Package className="h-6 w-6 text-primary" /></div>
            <div className="min-w-0"><h2 className="font-semibold">{app.display.name}</h2>
              <p className="text-xs text-muted-foreground">{app.app_id}</p>
              <p className="mt-1 text-[11px] text-muted-foreground">{app.display.source_kind === "capability" ? "Shared capability client" : app.display.source_name}</p></div></div>
          <p className="my-4 text-sm text-muted-foreground">{app.display.description}</p>
          <p className="mb-4 text-xs text-muted-foreground">Release {app.release_version} · {app.architecture} · {size(app.artifact.size)}</p>
          <div className="mt-auto flex gap-2">
            {local?.web_ready && <Button className="flex-1" variant={current ? "default" : "outline"}
              onClick={() => navigate(`/apps/${app.app_id}`)}><ExternalLink className="mr-2 h-4 w-4" /> Open</Button>}
            {!current && <Button className="flex-1" disabled={!!busy || app.artifact.size > 256 * 1024 * 1024}
              onClick={() => void install(app)}>
              {busy === app.app_id ? <Loader2 className="mr-2 h-4 w-4 animate-spin" /> : <Download className="mr-2 h-4 w-4" />}
              {local ? "Update" : "Install"}</Button>}
            {current && !local?.web_ready && <span className="text-xs text-muted-foreground">Installed; Web integration unavailable</span>}
          </div>
        </Card>;
      })}
    </div>
    {!loading && store && apps.length === 0 && <p className="mt-8 text-center text-sm text-muted-foreground">No published Apps match this filter and architecture.</p>}
    {installed.length > 0 && <div className="mt-7 border-t pt-5">
      <div className="flex justify-between"><h2 className="text-sm font-semibold">Already installed</h2>
        <Button variant="ghost" size="sm" onClick={() => navigate("/apps")}>View all Apps</Button></div>
      <p className="text-xs text-muted-foreground">Installed availability is separate from the release catalog and runtime authorization.</p>
    </div>}
    {pending && <AppReviewDialog review={pending.review} onConfirmed={() => void commit()} onCancel={() => void cancel()} />}
  </div>;
}
