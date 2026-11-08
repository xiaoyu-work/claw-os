import { useEffect, useState } from "react";
import { ExternalLink, Loader2, Package, ShoppingBag } from "lucide-react";
import { api } from "@/lib/api";
import { readInstalled, type InstalledApp } from "@/lib/apps";
import { navigate } from "@/lib/router";
import { Button } from "@/components/ui/button";
import { Card } from "@/components/ui/card";

export function AppsPage() {
  const [apps, setApps] = useState<InstalledApp[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  useEffect(() => {
    const controller = new AbortController();
    void api.get("/api/apps", { signal: controller.signal }).then((value) => {
      if (!controller.signal.aborted) setApps(readInstalled(value));
    }).catch((failure) => {
      if (!controller.signal.aborted) setError(failure instanceof Error ? failure.message : "Apps could not be read.");
    });
    return () => controller.abort();
  }, []);
  return <div className="h-full overflow-y-auto p-6">
    <div className="flex justify-between gap-3"><div><h1 className="text-xl font-semibold">Apps</h1>
      <p className="mt-1 text-sm text-muted-foreground">Independent App interfaces. Ordinary operations do not require an AI task.</p></div>
      <Button onClick={() => navigate("/app-store")}><ShoppingBag className="mr-2 h-4 w-4" /> App Store</Button></div>
    {error && <p role="alert" className="mt-4 text-sm text-destructive">{error}</p>}
    {!apps && !error && <Loader2 className="mt-6 h-5 w-5 animate-spin" />}
    <div className="mt-5 grid gap-4 md:grid-cols-2 xl:grid-cols-3">{apps?.map((app) => <Card key={app.id} className="p-4">
      <div className="flex gap-3"><Package className="h-7 w-7 text-primary" />
        <div><h2 className="font-medium">{app.name}</h2><p className="text-xs text-muted-foreground">{app.id} · {app.version}</p></div></div>
      <p className="mt-3 text-xs text-muted-foreground">{app.state === "quarantined" ? app.error :
        `${app.tools.length} declared Agent-callable tools${app.tools.length === 0 ? " · Native-only entry" : ""}`}</p>
      <Button className="mt-4" variant="outline" disabled={!app.web_ready || app.state !== "installed"}
        onClick={() => navigate(`/apps/${app.id}`)}><ExternalLink className="mr-2 h-4 w-4" /> Open App</Button>
      {!app.web_ready && app.state === "installed" && <p className="mt-2 text-xs text-muted-foreground">
        This installed version has no authenticated Web interface.</p>}
    </Card>)}</div>
    {apps?.length === 0 && <div className="mt-8 rounded-lg border border-dashed p-8 text-center text-sm text-muted-foreground">
      No Apps are installed. Choose a signed release in the App Store.</div>}
  </div>;
}
